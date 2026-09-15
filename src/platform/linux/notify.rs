//! Desktop notifications through `org.freedesktop.Notifications`, sent from
//! a worker thread so a slow or missing notification server never stalls
//! the UI. Each notification replaces our previous one.

use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

use super::bus::{Bus, BusError};
use super::wire::{Header, WireError, Writer};
use crate::limits::NOTIFICATION_QUEUE_DEPTH;

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";
/// app name, replaces id, icon, summary, body, actions, hints, timeout.
const NOTIFY_SIGNATURE: &str = "susssasa{sv}i";
const APP_NAME: &str = crate::app::NAME;
/// 0 low, 1 normal, 2 critical.
const URGENCY_NORMAL: u8 = 1;
/// The server decides how long it stays.
const EXPIRE_DEFAULT: i32 = -1;

struct Note {
    summary: String,
    body: String,
}

pub struct Notifier {
    queue: Option<SyncSender<Note>>,
    worker: Option<JoinHandle<()>>,
}

impl Notifier {
    /// Starts the worker thread. It connects to the bus at the first
    /// notification, not now.
    pub fn start() -> std::io::Result<Self> {
        let (queue, notes) = sync_channel(NOTIFICATION_QUEUE_DEPTH);
        let worker = std::thread::Builder::new().name("notify".to_owned()).spawn(move || run(&notes))?;
        Ok(Self { queue: Some(queue), worker: Some(worker) })
    }

    /// Queues a notification; drops it (logged) when the queue is full. The
    /// body may be shown as markup, so it mustn't contain `<` or `&`.
    pub fn notify(&self, summary: &str, body: &str) {
        assert!(!summary.is_empty());
        assert!(!body.contains(['<', '&']), "notification bodies may be read as markup");
        let Some(queue) = &self.queue else { return };
        let note = Note { summary: summary.to_owned(), body: body.to_owned() };
        match queue.try_send(note) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => log::warn!("notification dropped: too many waiting"),
            Err(TrySendError::Disconnected(_)) => log::warn!("notification dropped: the notification thread stopped"),
        }
    }
}

impl Drop for Notifier {
    fn drop(&mut self) {
        // Closing the queue ends the worker's loop.
        drop(self.queue.take());
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            log::error!("the notification thread panicked");
        }
    }
}

/// Shows queued notifications until the `Notifier` is dropped.
fn run(notes: &Receiver<Note>) {
    let mut bus = None;
    let mut last_id = 0;
    // Ends when the Notifier closes the queue.
    for note in notes {
        match show(&mut bus, &note, last_id) {
            Ok(id) => last_id = id,
            Err(error) => log::warn!("notification not shown: {error}"),
        }
    }
}

/// Shows one notification, connecting first if needed. A connection kept
/// from an earlier notification may have gone stale (a restarted session
/// bus): after an I/O error on it, this retries once on a fresh one.
fn show(bus: &mut Option<Bus>, note: &Note, replaces_id: u32) -> Result<u32, BusError> {
    let reused = bus.is_some();
    let mut connection = match bus.take() {
        Some(connection) => connection,
        None => Bus::session()?,
    };
    let result = match notify_call(&mut connection, note, replaces_id) {
        Err(BusError::Io(error)) if reused => {
            log::debug!("reconnecting to the session bus: {error}");
            connection = Bus::session()?;
            notify_call(&mut connection, note, replaces_id)
        }
        result => result,
    };
    // Keep the connection unless it may be out of step with the bus.
    if matches!(result, Ok(_) | Err(BusError::Remote { .. })) {
        *bus = Some(connection);
    }
    result
}

/// Calls `Notify`; returns the notification's id.
fn notify_call(bus: &mut Bus, note: &Note, replaces_id: u32) -> Result<u32, BusError> {
    let header = Header::method_call(NOTIFICATIONS, NOTIFICATIONS_PATH, NOTIFICATIONS, "Notify", NOTIFY_SIGNATURE);
    let reply = bus.call(&header, &notify_body(&note.summary, &note.body, replaces_id))?;
    if reply.signature() != "u" {
        return Err(WireError::Malformed("Notify didn't reply with an id").into());
    }
    Ok(reply.body().u32()?)
}

fn notify_body(summary: &str, body: &str, replaces_id: u32) -> Vec<u8> {
    let mut writer = Writer::with_capacity(128 + summary.len() + body.len());
    writer.str(APP_NAME);
    writer.u32(replaces_id);
    // No icon.
    writer.str("");
    writer.str(summary);
    writer.str(body);
    let actions = writer.begin_array(4);
    writer.end_array(actions);
    let hints = writer.begin_array(8);
    writer.begin_struct();
    writer.str("urgency");
    writer.variant("y");
    writer.u8(URGENCY_NORMAL);
    writer.end_array(hints);
    writer.i32(EXPIRE_DEFAULT);
    writer.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::super::wire::Reader;
    use super::*;

    #[test]
    fn notify_body_matches_its_signature() {
        let bytes = notify_body("Break in 60 s", "A cat is coming.", 7);
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.str(), Ok(APP_NAME));
        assert_eq!(reader.u32(), Ok(7));
        assert_eq!(reader.str(), Ok(""));
        assert_eq!(reader.str(), Ok("Break in 60 s"));
        assert_eq!(reader.str(), Ok("A cat is coming."));
        let actions = reader.array(4).unwrap();
        assert_eq!(reader.more(actions), Ok(false));
        let hints = reader.array(8).unwrap();
        reader.begin_struct().unwrap();
        assert_eq!(reader.str(), Ok("urgency"));
        assert_eq!(reader.signature(), Ok("y"));
        assert_eq!(reader.u8(), Ok(URGENCY_NORMAL));
        assert_eq!(reader.more(hints), Ok(false));
        assert_eq!(reader.i32(), Ok(EXPIRE_DEFAULT));
        assert_eq!(reader.u8(), Err(WireError::Truncated));

        let mut whole = Reader::new(&bytes);
        whole.skip(NOTIFY_SIGNATURE).unwrap();
        assert_eq!(whole.u8(), Err(WireError::Truncated));
    }

    /// Asks the desktop's notification server who it is; shows nothing.
    #[test]
    #[ignore = "needs a notification server: cargo test -- --ignored"]
    fn the_notification_server_answers() {
        let mut bus = Bus::session().unwrap();
        let header = Header::method_call(NOTIFICATIONS, NOTIFICATIONS_PATH, NOTIFICATIONS, "GetServerInformation", "");
        let reply = bus.call(&header, &[]).unwrap();
        assert_eq!(reply.signature(), "ssss");
        let mut body = reply.body();
        let (name, vendor, version, spec) = (body.str().unwrap(), body.str().unwrap(), body.str().unwrap(), body.str().unwrap());
        eprintln!("notification server: {name} {version} by {vendor}, spec {spec}");
        assert!(!name.is_empty());
    }
}
