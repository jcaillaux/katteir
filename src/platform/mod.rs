//! What differs by OS (CLAUDE.md §5): desktop notifications and the tray
//! icon. On Linux both go through catnap's own small D-Bus client
//! (`linux/`). On other systems notifications are only logged, and there's
//! no tray, until M2 reaches macOS and Windows.

#[cfg(target_os = "linux")]
mod linux;

/// What the tray menu shows. main sends it on every timer tick; only
/// changes go further.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayState {
    /// The greyed-out first line, such as "Working: break in 12 min". No
    /// underscores: menus read them as keyboard mnemonics.
    pub status: String,
    pub can_start: bool,
    pub can_pause: bool,
    pub can_stop: bool,
}

/// Whether catnap has a tray icon. A closed settings window can only be
/// opened again from it, so closing the window quits unless it's `Shown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayPresence {
    /// Not known yet: registration is under way.
    Starting,
    Shown,
    /// No tray host (plain GNOME), no session bus, or the host went away.
    Absent,
}

/// A choice made in the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    ShowSettings,
    Start,
    Pause,
    Stop,
    Quit,
}

pub struct Platform {
    #[cfg(target_os = "linux")]
    notifier: Option<linux::notify::Notifier>,
    #[cfg(target_os = "linux")]
    tray: Option<linux::tray::Tray>,
}

impl Platform {
    /// Sets up notifications and the tray icon. `on_tray_action` is called
    /// on the tray's own thread; the tray also sends it `ShowSettings` when
    /// a second catnap starts. `None` if catnap is already running: that one
    /// was asked to show its settings window, and this one should exit.
    /// Nothing else here is fatal.
    pub fn start(on_tray_action: impl Fn(TrayAction) + Send + 'static) -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            let bus = match linux::instance::claim() {
                Ok(linux::instance::Claim::First(bus)) => Some(bus),
                Ok(linux::instance::Claim::AlreadyRunning) => return None,
                Err(error) => {
                    log::warn!("no tray icon, and no check for another catnap: {error}");
                    None
                }
            };
            let tray = bus.and_then(|bus| {
                linux::tray::Tray::start(bus, Box::new(on_tray_action))
                    .map_err(|error| log::warn!("no tray icon: {error}"))
                    .ok()
            });
            let notifier = linux::notify::Notifier::start()
                .map_err(|error| log::warn!("no desktop notifications: {error}"))
                .ok();
            Some(Self { notifier, tray })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = on_tray_action;
            Some(Self {})
        }
    }

    /// Shows a desktop notification without waiting for it. Failures are
    /// logged: a missing notification server mustn't stop the timer.
    pub fn notify(&self, summary: &str, body: &str) {
        assert!(!summary.is_empty(), "a notification needs a summary");
        #[cfg(target_os = "linux")]
        if let Some(notifier) = &self.notifier {
            notifier.notify(summary, body);
            return;
        }
        log::info!("notification: {summary}: {body}");
    }

    pub fn tray_presence(&self) -> TrayPresence {
        #[cfg(target_os = "linux")]
        {
            self.tray.as_ref().map_or(TrayPresence::Absent, linux::tray::Tray::presence)
        }
        #[cfg(not(target_os = "linux"))]
        {
            TrayPresence::Absent
        }
    }

    /// Updates the tray menu. Cheap when nothing changed.
    pub fn set_tray_state(&self, state: &TrayState) {
        #[cfg(target_os = "linux")]
        if let Some(tray) = &self.tray {
            tray.set_state(state);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = state;
    }
}
