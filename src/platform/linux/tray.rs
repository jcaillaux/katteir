//! The tray icon: a `StatusNotifierItem` with a `com.canonical.dbusmenu`
//! menu (`menu.rs`), registered with the desktop's `StatusNotifierWatcher`.
//! The icon is an SVG file found through `IconThemePath`, not pixels over
//! D-Bus: ayatana's watcher (Budgie, Ubuntu) reads only icon names, and
//! Chromium's tray icons work the same way.
//!
//! Two threads. One blocks reading the bus and forwards every message; the
//! other owns the write half and handles those messages and the UI's state
//! updates in order, from one queue. Menu choices go to `on_action`, called
//! on the tray's thread. The connection is the one that owns
//! `catnap.Instance` (`instance.rs`), so its `Show` is answered here too.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;

use super::bus::{Bus, BusError, BusReader, BusWriter};
use super::{instance, menu};
use super::wire::{Header, Kind, Message, NO_REPLY_EXPECTED, Reader, WireError, Writer};
use crate::limits::{MAX_MENU_REQUEST_ITEMS, TRAY_QUEUE_DEPTH};
use crate::platform::{TrayAction, TrayState};

const BUS_NAME: &str = "org.freedesktop.DBus";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM: &str = "org.kde.StatusNotifierItem";
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
const MENU_PATH: &str = "/MenuBar";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const INTROSPECTABLE: &str = "org.freedesktop.DBus.Introspectable";
const PEER: &str = "org.freedesktop.DBus.Peer";

const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";
const UNKNOWN_OBJECT: &str = "org.freedesktop.DBus.Error.UnknownObject";
const UNKNOWN_PROPERTY: &str = "org.freedesktop.DBus.Error.UnknownProperty";
const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";
const READ_ONLY: &str = "org.freedesktop.DBus.Error.PropertyReadOnly";

const ICON_NAME: &str = "catnap-tray";
/// A themed icon from the freedesktop naming spec, if the SVG can't be written.
const FALLBACK_ICON_NAME: &str = "appointment-soon";
const ICON_SVG: &[u8] = include_bytes!("../../../assets/icons/catnap-tray.svg");

/// The item's properties and their types: the set Chromium's tray icons expose.
const ITEM_PROPERTIES: [(&str, &str); 15] = [
    ("AttentionIconName", "s"),
    ("AttentionIconPixmap", "a(iiay)"),
    ("AttentionMovieName", "s"),
    ("Category", "s"),
    ("IconName", "s"),
    ("IconThemePath", "s"),
    ("Id", "s"),
    ("ItemIsMenu", "b"),
    ("Menu", "o"),
    ("OverlayIconName", "s"),
    ("OverlayIconPixmap", "a(iiay)"),
    ("Status", "s"),
    ("Title", "s"),
    ("ToolTip", "(sa(iiay)ss)"),
    ("WindowId", "i"),
];
const MENU_PROPERTIES: [(&str, &str); 4] =
    [("IconThemePath", "as"), ("Status", "s"), ("TextDirection", "s"), ("Version", "u")];

/// A D-Bus error name and message.
type Failure = (&'static str, String);
/// A method call's answer: a reply body and its signature, or an error.
type Answer = Result<(&'static str, Vec<u8>), Failure>;

enum Input {
    Bus(Message),
    BusClosed(BusError),
    State(TrayState),
    Quit,
}

pub struct Tray {
    inputs: SyncSender<Input>,
    available: Arc<AtomicBool>,
    /// The last state queued, so unchanged ticks aren't sent.
    queued: RefCell<Option<TrayState>>,
    worker: Option<JoinHandle<()>>,
}

impl Tray {
    /// Starts the tray's thread on `bus`, which owns `catnap.Instance`. The
    /// thread registers the icon on its own.
    pub fn start(bus: Bus, on_action: Box<dyn Fn(TrayAction) + Send>) -> std::io::Result<Self> {
        let (inputs, queue) = sync_channel(TRAY_QUEUE_DEPTH);
        let available = Arc::new(AtomicBool::new(false));
        let (feed, shared) = (inputs.clone(), Arc::clone(&available));
        let worker = std::thread::Builder::new()
            .name("catnap-tray".to_owned())
            .spawn(move || run(bus, queue, feed, shared, on_action))?;
        Ok(Self { inputs, available, queued: RefCell::new(None), worker: Some(worker) })
    }

    /// Whether a tray host accepted the icon.
    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }

    /// Passes a changed state to the tray. Never blocks: when the queue is
    /// full, the next tick tries again.
    pub fn set_state(&self, state: &TrayState) {
        if self.queued.borrow().as_ref() == Some(state) {
            return;
        }
        if self.inputs.try_send(Input::State(state.clone())).is_ok() {
            *self.queued.borrow_mut() = Some(state.clone());
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // An error means the thread has already ended.
        let _ = self.inputs.send(Input::Quit);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            log::error!("the tray thread panicked");
        }
    }
}

/// The tray's thread: set up, register, then handle the queue until the
/// `Tray` is dropped or the bus goes away.
fn run(
    bus: Bus,
    queue: Receiver<Input>,
    feed: SyncSender<Input>,
    available: Arc<AtomicBool>,
    on_action: Box<dyn Fn(TrayAction) + Send>,
) {
    let name = format!("{ITEM}-{}-1", std::process::id());
    let (writer, reader) = match set_up(bus, &name) {
        Ok(halves) => halves,
        Err(error) => {
            log::warn!("no tray icon: {error}");
            return;
        }
    };
    let reader = match spawn_reader(reader, feed) {
        Ok(reader) => reader,
        Err(error) => {
            log::warn!("no tray icon: {error}");
            return;
        }
    };
    let state = TrayState { status: "catnap".to_owned(), can_start: false, can_pause: false, can_stop: false };
    let mut item = Item { writer, name, icon: Icon::write(), state, revision: 1, register_serial: None, available, on_action };
    item.register();
    for input in &queue {
        match input {
            Input::Bus(message) => item.handle(&message),
            Input::State(state) => item.set_state(state),
            Input::BusClosed(error) => {
                log::warn!("the tray icon lost the session bus: {error}");
                break;
            }
            Input::Quit => break,
        }
    }
    item.available.store(false, Ordering::Relaxed);
    item.writer.shutdown();
    // The reader may be waiting to hand over a message: dropping the queue
    // releases it.
    drop(queue);
    if reader.join().is_err() {
        log::error!("the tray's bus reader panicked");
    }
}

fn set_up(mut bus: Bus, name: &str) -> Result<(BusWriter, BusReader), BusError> {
    bus.request_name(name)?;
    // To register again when a tray host (re)starts.
    bus.add_match(&format!(
        "type='signal',sender='{BUS_NAME}',interface='{BUS_NAME}',member='NameOwnerChanged',arg0='{WATCHER}'"
    ))?;
    Ok(bus.split()?)
}

fn spawn_reader(mut reader: BusReader, feed: SyncSender<Input>) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name("catnap-tray-bus".to_owned()).spawn(move || {
        // Ends when the connection closes (the tray thread shuts it down on
        // quit) or the queue is gone.
        loop {
            let input = match reader.receive() {
                Ok(message) => Input::Bus(message),
                Err(error) => Input::BusClosed(error),
            };
            let closed = matches!(input, Input::BusClosed(_));
            if feed.send(input).is_err() || closed {
                return;
            }
        }
    })
}

/// The tray icon as the host looks it up: a name, and where to find it.
struct Icon {
    name: &'static str,
    dir: String,
}

impl Icon {
    /// Writes the SVG to a private runtime directory. It's left there on
    /// exit: another catnap may be showing it, and the directory is emptied
    /// at logout anyway.
    fn write() -> Self {
        match write_icon_file() {
            Ok(dir) => Self { name: ICON_NAME, dir },
            Err(error) => {
                log::warn!("tray icon file not written ({error}); using the theme's {FALLBACK_ICON_NAME}");
                Self { name: FALLBACK_ICON_NAME, dir: String::new() }
            }
        }
    }
}

fn write_icon_file() -> std::io::Result<String> {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) => PathBuf::from(runtime).join("catnap"),
        None => std::env::temp_dir().join(format!("catnap-{}", std::process::id())),
    };
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{ICON_NAME}.svg")), ICON_SVG)?;
    dir.into_os_string().into_string().map_err(|_| std::io::Error::other("the icon directory isn't UTF-8"))
}

/// The item on the bus: the write half of the connection and what it shows.
struct Item {
    writer: BusWriter,
    /// The well-known name registered with the watcher.
    name: String,
    icon: Icon,
    state: TrayState,
    /// The menu layout's revision, bumped on every change.
    revision: u32,
    /// The `RegisterStatusNotifierItem` call waiting for its reply.
    register_serial: Option<u32>,
    available: Arc<AtomicBool>,
    on_action: Box<dyn Fn(TrayAction) + Send>,
}

impl Item {
    fn handle(&mut self, message: &Message) {
        match message.kind() {
            Kind::MethodCall => self.answer(message),
            Kind::MethodReturn | Kind::Error
                if self.register_serial.is_some() && message.reply_serial() == self.register_serial =>
            {
                self.registered(message);
            }
            Kind::Signal if message.sender() == Some(BUS_NAME) && message.member() == Some("NameOwnerChanged") => {
                self.watcher_changed(message);
            }
            _ => {}
        }
    }

    /// Every call gets a reply, or an error, unless it asked for none.
    fn answer(&mut self, call: &Message) {
        let answer = self.dispatch(call);
        if call.flags() & NO_REPLY_EXPECTED != 0 {
            return;
        }
        let sent = match answer {
            Ok((signature, body)) => self.writer.send(&Header::method_return(call.serial(), call.sender(), signature), &body),
            Err((name, text)) => {
                log::debug!("tray: {} {}: {name}: {text}", call.path().unwrap_or(""), call.member().unwrap_or(""));
                let mut body = Writer::with_capacity(text.len() + 8);
                body.str(&text);
                self.writer.send(&Header::error(call.serial(), call.sender(), name), &body.into_bytes())
            }
        };
        if let Err(error) = sent {
            log::warn!("tray: reply not sent: {error}");
        }
    }

    fn dispatch(&mut self, call: &Message) -> Answer {
        let path = call.path().unwrap_or("");
        let member = call.member().unwrap_or("");
        match (call.interface(), member) {
            (Some(INTROSPECTABLE) | None, "Introspect") => introspect(path),
            (Some(PEER) | None, "Ping") => Ok(EMPTY_REPLY),
            (Some(PROPERTIES) | None, "Get") => self.get(call),
            (Some(PROPERTIES) | None, "GetAll") => self.get_all(call),
            (Some(PROPERTIES), "Set") => Err((READ_ONLY, "the tray's properties are read-only".to_owned())),
            // A second catnap started: it exits, this one shows its window.
            (Some(instance::INTERFACE) | None, "Show") if path == instance::PATH => {
                (self.on_action)(TrayAction::ShowSettings);
                Ok(EMPTY_REPLY)
            }
            (Some(ITEM) | None, _) if path == ITEM_PATH => self.item_method(member),
            (Some(MENU) | None, _) if path == MENU_PATH => self.menu_method(call, member),
            _ => Err((UNKNOWN_METHOD, format!("no method {member} at {path}"))),
        }
    }

    fn get(&self, call: &Message) -> Answer {
        expect_args(call, "ss")?;
        let mut args = call.body();
        let interface = args.str().map_err(invalid)?;
        let name = args.str().map_err(invalid)?;
        let mut writer = Writer::with_capacity(256);
        if self.write_property(&mut writer, call.path().unwrap_or(""), interface, name) {
            Ok(("v", writer.into_bytes()))
        } else {
            Err((UNKNOWN_PROPERTY, format!("no property {interface}.{name}")))
        }
    }

    fn get_all(&self, call: &Message) -> Answer {
        expect_args(call, "s")?;
        let interface = call.body().str().map_err(invalid)?;
        let path = call.path().unwrap_or("");
        let properties: &[(&str, &str)] = match (path, interface) {
            (ITEM_PATH, ITEM) => &ITEM_PROPERTIES,
            (MENU_PATH, MENU) => &MENU_PROPERTIES,
            _ => &[],
        };
        let mut writer = Writer::with_capacity(1024);
        let dict = writer.begin_array(8);
        for (name, _) in properties {
            writer.begin_struct();
            writer.str(name);
            assert!(self.write_property(&mut writer, path, interface, name), "{name} is listed but not written");
        }
        writer.end_array(dict);
        Ok(("a{sv}", writer.into_bytes()))
    }

    /// Writes one property as a variant; false (and nothing written) if there's no such property.
    fn write_property(&self, writer: &mut Writer, path: &str, interface: &str, name: &str) -> bool {
        match (path, interface) {
            (ITEM_PATH, ITEM) => write_item_property(writer, name, &self.state, &self.icon),
            (MENU_PATH, MENU) => write_menu_property(writer, name),
            _ => false,
        }
    }

    fn item_method(&self, member: &str) -> Answer {
        match member {
            // A left click, on hosts that don't open the menu for it (KDE).
            "Activate" => {
                (self.on_action)(TrayAction::ShowSettings);
                Ok(EMPTY_REPLY)
            }
            "SecondaryActivate" | "ContextMenu" | "Scroll" | "ProvideXdgActivationToken" => Ok(EMPTY_REPLY),
            _ => Err((UNKNOWN_METHOD, format!("no method {member} on the tray item"))),
        }
    }

    fn menu_method(&self, call: &Message, member: &str) -> Answer {
        match member {
            "GetLayout" => self.get_layout(call),
            "GetGroupProperties" => self.group_properties(call),
            "GetProperty" => self.menu_item_property(call),
            "Event" => self.event(call),
            "EventGroup" => self.event_group(call),
            // The menu never changes just because it opens.
            "AboutToShow" => {
                let mut writer = Writer::with_capacity(4);
                writer.bool(false);
                Ok(("b", writer.into_bytes()))
            }
            "AboutToShowGroup" => {
                let mut writer = Writer::with_capacity(8);
                let updates = writer.begin_array(4);
                writer.end_array(updates);
                let errors = writer.begin_array(4);
                writer.end_array(errors);
                Ok(("aiai", writer.into_bytes()))
            }
            _ => Err((UNKNOWN_METHOD, format!("no method {member} on the tray menu"))),
        }
    }

    fn get_layout(&self, call: &Message) -> Answer {
        expect_args(call, "iias")?;
        let mut args = call.body();
        let parent = args.i32().map_err(invalid)?;
        let depth = args.i32().map_err(invalid)?;
        let wanted = read_strings(&mut args)?;
        menu::layout(self.revision, parent, depth, &wanted, &self.state)
            .map(|body| ("u(ia{sv}av)", body))
            .ok_or_else(|| (INVALID_ARGS, format!("no menu item {parent}")))
    }

    fn group_properties(&self, call: &Message) -> Answer {
        expect_args(call, "aias")?;
        let mut args = call.body();
        let ids = read_ints(&mut args)?;
        let wanted = read_strings(&mut args)?;
        Ok(("a(ia{sv})", menu::group_properties(&ids, &wanted, &self.state)))
    }

    fn menu_item_property(&self, call: &Message) -> Answer {
        expect_args(call, "is")?;
        let mut args = call.body();
        let id = args.i32().map_err(invalid)?;
        let name = args.str().map_err(invalid)?;
        menu::property(id, name, &self.state)
            .map(|body| ("v", body))
            .ok_or_else(|| (UNKNOWN_PROPERTY, format!("no property {name} on menu item {id}")))
    }

    fn event(&self, call: &Message) -> Answer {
        expect_args(call, "isvu")?;
        let (id, event_id) = read_event(&mut call.body())?;
        self.menu_event(id, event_id);
        Ok(EMPTY_REPLY)
    }

    /// Several events at once; replies with the ids that aren't items.
    fn event_group(&self, call: &Message) -> Answer {
        expect_args(call, "a(isvu)")?;
        let mut args = call.body();
        let end = args.array(8).map_err(invalid)?;
        let mut unknown = Vec::new();
        for _ in 0..MAX_MENU_REQUEST_ITEMS {
            if !args.more(end).map_err(invalid)? {
                break;
            }
            args.begin_struct().map_err(invalid)?;
            let (id, event_id) = read_event(&mut args)?;
            if menu::is_item(id) {
                self.menu_event(id, event_id);
            } else {
                unknown.push(id);
            }
        }
        let mut writer = Writer::with_capacity(4 + 4 * unknown.len());
        let ids = writer.begin_array(4);
        for id in unknown {
            writer.i32(id);
        }
        writer.end_array(ids);
        Ok(("ai", writer.into_bytes()))
    }

    fn menu_event(&self, id: i32, event_id: &str) {
        if event_id != "clicked" {
            return;
        }
        if let Some(action) = menu::clicked(id, &self.state) {
            (self.on_action)(action);
        }
    }

    /// Shows a new state: the host re-reads the menu and the tooltip.
    fn set_state(&mut self, state: TrayState) {
        if state == self.state {
            return;
        }
        self.state = state;
        self.revision = self.revision.wrapping_add(1);
        let mut body = Writer::with_capacity(8);
        body.u32(self.revision);
        body.i32(menu::ROOT);
        self.signal(MENU_PATH, MENU, "LayoutUpdated", "ui", &body.into_bytes());
        self.signal(ITEM_PATH, ITEM, "NewToolTip", "", &[]);
    }

    fn signal(&mut self, path: &str, interface: &str, member: &str, signature: &str, body: &[u8]) {
        if let Err(error) = self.writer.send(&Header::signal(path, interface, member, signature), body) {
            log::warn!("tray: {member} not sent: {error}");
        }
    }

    fn register(&mut self) {
        let header = Header::method_call(WATCHER, WATCHER_PATH, WATCHER, "RegisterStatusNotifierItem", "s");
        let mut body = Writer::with_capacity(64);
        body.str(&self.name);
        match self.writer.send(&header, &body.into_bytes()) {
            Ok(serial) => self.register_serial = Some(serial),
            Err(error) => log::warn!("no tray icon: {error}"),
        }
    }

    fn registered(&mut self, reply: &Message) {
        self.register_serial = None;
        let accepted = reply.kind() == Kind::MethodReturn;
        if accepted {
            log::info!("tray icon shown");
        } else {
            log::warn!("no tray icon: {}", reply.error_name().unwrap_or("the tray host refused it"));
        }
        self.available.store(accepted, Ordering::Relaxed);
    }

    /// The watcher's `NameOwnerChanged` (our match rule): register again when
    /// a tray host (re)starts, and note when it goes away.
    fn watcher_changed(&mut self, signal: &Message) {
        let mut args = signal.body();
        let (Ok(name), Ok(_old_owner), Ok(new_owner)) = (args.str(), args.str(), args.str()) else { return };
        if name != WATCHER {
            return;
        }
        if new_owner.is_empty() {
            log::info!("the tray host went away");
            self.available.store(false, Ordering::Relaxed);
        } else {
            self.register();
        }
    }
}

/// The reply to a call that returns nothing.
const EMPTY_REPLY: (&str, Vec<u8>) = ("", Vec::new());

fn invalid(error: WireError) -> Failure {
    (INVALID_ARGS, error.to_string())
}

/// Checks a call's argument types before they're read.
fn expect_args(call: &Message, signature: &str) -> Result<(), Failure> {
    if call.signature() == signature {
        Ok(())
    } else {
        Err((INVALID_ARGS, format!("expected arguments ({signature}), got ({})", call.signature())))
    }
}

/// Reads an "as" argument of at most `MAX_MENU_REQUEST_ITEMS` entries.
fn read_strings<'m>(args: &mut Reader<'m>) -> Result<Vec<&'m str>, Failure> {
    let end = args.array(4).map_err(invalid)?;
    let mut strings = Vec::new();
    for _ in 0..MAX_MENU_REQUEST_ITEMS {
        if !args.more(end).map_err(invalid)? {
            return Ok(strings);
        }
        strings.push(args.str().map_err(invalid)?);
    }
    Err((INVALID_ARGS, format!("more than {MAX_MENU_REQUEST_ITEMS} entries")))
}

/// Reads an "ai" argument of at most `MAX_MENU_REQUEST_ITEMS` entries.
fn read_ints(args: &mut Reader<'_>) -> Result<Vec<i32>, Failure> {
    let end = args.array(4).map_err(invalid)?;
    let mut ints = Vec::new();
    for _ in 0..MAX_MENU_REQUEST_ITEMS {
        if !args.more(end).map_err(invalid)? {
            return Ok(ints);
        }
        ints.push(args.i32().map_err(invalid)?);
    }
    Err((INVALID_ARGS, format!("more than {MAX_MENU_REQUEST_ITEMS} entries")))
}

/// One menu event, "isvu": the item, what happened, then data and a
/// timestamp that catnap doesn't need.
fn read_event<'m>(args: &mut Reader<'m>) -> Result<(i32, &'m str), Failure> {
    let id = args.i32().map_err(invalid)?;
    let event_id = args.str().map_err(invalid)?;
    args.skip("vu").map_err(invalid)?;
    Ok((id, event_id))
}

/// Writes one of `ITEM_PROPERTIES` as a variant; false, writing nothing, for
/// any other name.
fn write_item_property(writer: &mut Writer, name: &str, state: &TrayState, icon: &Icon) -> bool {
    match name {
        "Category" => variant_str(writer, "ApplicationStatus"),
        "Id" | "Title" => variant_str(writer, "catnap"),
        "Status" => variant_str(writer, "Active"),
        "IconName" => variant_str(writer, icon.name),
        "IconThemePath" => variant_str(writer, &icon.dir),
        "AttentionIconName" | "OverlayIconName" | "AttentionMovieName" => variant_str(writer, ""),
        "AttentionIconPixmap" | "OverlayIconPixmap" => {
            writer.variant("a(iiay)");
            let none = writer.begin_array(8);
            writer.end_array(none);
        }
        "ItemIsMenu" => {
            writer.variant("b");
            writer.bool(false);
        }
        "Menu" => {
            writer.variant("o");
            writer.object_path(MENU_PATH);
        }
        "WindowId" => {
            writer.variant("i");
            writer.i32(0);
        }
        "ToolTip" => {
            // Icon name, icon pixmaps, title, text.
            writer.variant("(sa(iiay)ss)");
            writer.begin_struct();
            writer.str("");
            let none = writer.begin_array(8);
            writer.end_array(none);
            writer.str("catnap");
            writer.str(&state.status);
        }
        _ => return false,
    }
    true
}

/// Writes one of `MENU_PROPERTIES` as a variant; false for any other name.
fn write_menu_property(writer: &mut Writer, name: &str) -> bool {
    match name {
        "Version" => {
            writer.variant("u");
            writer.u32(3);
        }
        "TextDirection" => variant_str(writer, "ltr"),
        "Status" => variant_str(writer, "normal"),
        "IconThemePath" => {
            writer.variant("as");
            let none = writer.begin_array(4);
            writer.end_array(none);
        }
        _ => return false,
    }
    true
}

fn variant_str(writer: &mut Writer, value: &str) {
    writer.variant("s");
    writer.str(value);
}

const DOCTYPE: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">"#;
const ITEM_METHODS: &str = r#"<method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method><signal name="NewToolTip"/>"#;
const MENU_METHODS: &str = r#"<method name="GetLayout"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="as" direction="in"/><arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/></method><method name="GetGroupProperties"><arg type="ai" direction="in"/><arg type="as" direction="in"/><arg type="a(ia{sv})" direction="out"/></method><method name="GetProperty"><arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method><method name="Event"><arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="in"/><arg type="u" direction="in"/></method><method name="EventGroup"><arg type="a(isvu)" direction="in"/><arg type="ai" direction="out"/></method><method name="AboutToShow"><arg type="i" direction="in"/><arg type="b" direction="out"/></method><method name="AboutToShowGroup"><arg type="ai" direction="in"/><arg type="ai" direction="out"/><arg type="ai" direction="out"/></method><signal name="LayoutUpdated"><arg type="u"/><arg type="i"/></signal>"#;
const STANDARD_INTERFACES: &str = r#"<interface name="org.freedesktop.DBus.Properties"><method name="Get"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method><method name="GetAll"><arg type="s" direction="in"/><arg type="a{sv}" direction="out"/></method></interface><interface name="org.freedesktop.DBus.Introspectable"><method name="Introspect"><arg type="s" direction="out"/></method></interface><interface name="org.freedesktop.DBus.Peer"><method name="Ping"/></interface>"#;

/// The XML description of one of our objects, for `busctl introspect` and
/// hosts that ask.
fn introspect(path: &str) -> Answer {
    let mut xml = String::with_capacity(4096);
    xml.push_str(DOCTYPE);
    xml.push_str("<node>");
    match path {
        "/" => xml.push_str(r#"<node name="StatusNotifierItem"/><node name="MenuBar"/><node name="catnap"/>"#),
        "/catnap" => xml.push_str(r#"<node name="Instance"/>"#),
        ITEM_PATH => push_interface(&mut xml, ITEM, ITEM_METHODS, &ITEM_PROPERTIES),
        MENU_PATH => push_interface(&mut xml, MENU, MENU_METHODS, &MENU_PROPERTIES),
        instance::PATH => push_interface(&mut xml, instance::INTERFACE, r#"<method name="Show"/>"#, &[]),
        _ => return Err((UNKNOWN_OBJECT, format!("no object at {path}"))),
    }
    xml.push_str(STANDARD_INTERFACES);
    xml.push_str("</node>");
    let mut writer = Writer::with_capacity(xml.len() + 8);
    writer.str(&xml);
    Ok(("s", writer.into_bytes()))
}

fn push_interface(xml: &mut String, name: &str, members: &str, properties: &[(&str, &str)]) {
    let _ = write!(xml, r#"<interface name="{name}">{members}"#);
    for (property, signature) in properties {
        let _ = write!(xml, r#"<property name="{property}" type="{signature}" access="read"/>"#);
    }
    xml.push_str("</interface>");
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::sync::Mutex;
    use std::time::Duration;

    use super::super::wire::encode;
    use super::*;

    fn working() -> TrayState {
        TrayState { status: "Working: break in 12 min".to_owned(), can_start: false, can_pause: true, can_stop: true }
    }

    /// An item on one end of a socket pair; the test plays the bus on the other.
    fn item() -> (Item, BusReader, Arc<Mutex<Vec<TrayAction>>>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let actions = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&actions);
        let item = Item {
            writer: BusWriter::from_stream(ours),
            name: "org.kde.StatusNotifierItem-1-1".to_owned(),
            icon: Icon { name: ICON_NAME, dir: "/run/user/1000/catnap".to_owned() },
            state: working(),
            revision: 1,
            register_serial: None,
            available: Arc::new(AtomicBool::new(false)),
            on_action: Box::new(move |action| seen.lock().unwrap().push(action)),
        };
        (item, BusReader::from_stream(theirs), actions)
    }

    fn call(path: &str, interface: &str, member: &str, signature: &str, args: Writer) -> Message {
        let header = Header::method_call(":1.7", path, interface, member, signature);
        Message::parse(encode(&header, 77, &args.into_bytes()).unwrap()).unwrap()
    }

    fn click(id: i32) -> Writer {
        let mut args = Writer::default();
        args.i32(id);
        args.str("clicked");
        args.variant("i");
        args.i32(0);
        args.u32(0);
        args
    }

    #[test]
    fn a_menu_click_runs_its_action_and_is_answered() {
        let (mut item, mut bus, actions) = item();
        item.handle(&call(MENU_PATH, MENU, "Event", "isvu", click(menu::PAUSE)));
        let reply = bus.receive().unwrap();
        assert_eq!(reply.kind(), Kind::MethodReturn);
        assert_eq!(reply.reply_serial(), Some(77));
        // Start is disabled while working: clicking it does nothing.
        item.handle(&call(MENU_PATH, MENU, "Event", "isvu", click(menu::START)));
        bus.receive().unwrap();
        let mut activate = Writer::default();
        activate.i32(10);
        activate.i32(20);
        item.handle(&call(ITEM_PATH, ITEM, "Activate", "ii", activate));
        bus.receive().unwrap();
        assert_eq!(*actions.lock().unwrap(), [TrayAction::Pause, TrayAction::ShowSettings]);
    }

    #[test]
    fn a_second_catnap_asking_to_show_opens_the_settings() {
        let (mut item, mut bus, actions) = item();
        item.handle(&call(instance::PATH, instance::INTERFACE, "Show", "", Writer::default()));
        assert_eq!(bus.receive().unwrap().kind(), Kind::MethodReturn);
        assert_eq!(*actions.lock().unwrap(), [TrayAction::ShowSettings]);
        // The same method on another object is unknown.
        item.handle(&call(ITEM_PATH, instance::INTERFACE, "Show", "", Writer::default()));
        assert_eq!(bus.receive().unwrap().error_name(), Some(UNKNOWN_METHOD));
    }

    #[test]
    fn get_layout_answers_with_the_menu() {
        let (mut item, mut bus, _) = item();
        let mut args = Writer::default();
        args.i32(menu::ROOT);
        args.i32(-1);
        let names = args.begin_array(4);
        args.end_array(names);
        item.handle(&call(MENU_PATH, MENU, "GetLayout", "iias", args));
        let reply = bus.receive().unwrap();
        assert_eq!(reply.signature(), "u(ia{sv}av)");
        let mut body = reply.body();
        assert_eq!(body.u32(), Ok(1));
        body.skip("(ia{sv}av)").unwrap();
        assert_eq!(body.u8(), Err(WireError::Truncated));
    }

    #[test]
    fn unknown_calls_get_an_error_not_silence() {
        let (mut item, mut bus, _) = item();
        item.handle(&call(ITEM_PATH, ITEM, "Frobnicate", "", Writer::default()));
        let reply = bus.receive().unwrap();
        assert_eq!(reply.kind(), Kind::Error);
        assert_eq!(reply.error_name(), Some(UNKNOWN_METHOD));
        item.handle(&call("/elsewhere", INTROSPECTABLE, "Introspect", "", Writer::default()));
        assert_eq!(bus.receive().unwrap().error_name(), Some(UNKNOWN_OBJECT));
        item.handle(&call(MENU_PATH, MENU, "GetLayout", "s", {
            let mut wrong = Writer::default();
            wrong.str("x");
            wrong
        }));
        assert_eq!(bus.receive().unwrap().error_name(), Some(INVALID_ARGS));
    }

    #[test]
    fn get_all_names_the_icon_file() {
        let (mut item, mut bus, _) = item();
        let mut args = Writer::default();
        args.str(ITEM);
        item.handle(&call(ITEM_PATH, PROPERTIES, "GetAll", "s", args));
        let reply = bus.receive().unwrap();
        assert_eq!(reply.signature(), "a{sv}");
        let mut body = reply.body();
        let end = body.array(8).unwrap();
        let mut strings = Vec::new();
        while body.more(end).unwrap() {
            body.begin_struct().unwrap();
            let name = body.str().unwrap();
            match body.signature().unwrap() {
                "s" => strings.push((name, body.str().unwrap())),
                other => body.skip(other).unwrap(),
            }
        }
        assert!(strings.contains(&("IconName", ICON_NAME)));
        assert!(strings.contains(&("IconThemePath", "/run/user/1000/catnap")));
        assert!(strings.contains(&("Id", "catnap")));
        assert!(strings.contains(&("Status", "Active")));
    }

    #[test]
    fn every_listed_property_has_its_listed_type() {
        let icon = Icon { name: ICON_NAME, dir: String::new() };
        for (name, signature) in ITEM_PROPERTIES {
            let mut writer = Writer::default();
            assert!(write_item_property(&mut writer, name, &working(), &icon), "{name}");
            let bytes = writer.into_bytes();
            let mut value = Reader::new(&bytes);
            assert_eq!(value.signature(), Ok(signature), "{name}");
            value.skip(signature).unwrap();
            assert_eq!(value.u8(), Err(WireError::Truncated), "{name}");
        }
        for (name, signature) in MENU_PROPERTIES {
            let mut writer = Writer::default();
            assert!(write_menu_property(&mut writer, name), "{name}");
            let bytes = writer.into_bytes();
            let mut value = Reader::new(&bytes);
            assert_eq!(value.signature(), Ok(signature), "{name}");
        }
        let mut untouched = Writer::default();
        assert!(!write_item_property(&mut untouched, "Nonsense", &working(), &icon));
        assert!(untouched.into_bytes().is_empty());
    }

    #[test]
    fn a_new_state_is_announced_once() {
        let (mut item, mut bus, _) = item();
        let idle = TrayState { status: "Idle".to_owned(), can_start: true, can_pause: false, can_stop: false };
        item.set_state(idle.clone());
        let layout = bus.receive().unwrap();
        assert_eq!(layout.kind(), Kind::Signal);
        assert_eq!(layout.member(), Some("LayoutUpdated"));
        assert_eq!(layout.body().u32(), Ok(2));
        assert_eq!(bus.receive().unwrap().member(), Some("NewToolTip"));
        item.set_state(idle);
        assert!(matches!(bus.receive(), Err(BusError::Io(_))), "nothing for an unchanged state");
    }

    #[test]
    fn the_watchers_answer_decides_whether_the_tray_is_up() {
        let (mut item, _bus, _) = item();
        item.register();
        let serial = item.register_serial.unwrap();
        let accepted = Header::method_return(serial, None, "");
        item.handle(&Message::parse(encode(&accepted, 5, &[]).unwrap()).unwrap());
        assert!(item.available.load(Ordering::Relaxed));
        assert_eq!(item.register_serial, None);

        item.register();
        let serial = item.register_serial.unwrap();
        let mut text = Writer::default();
        text.str("no watcher");
        let refused = Header::error(serial, None, "org.freedesktop.DBus.Error.ServiceUnknown");
        item.handle(&Message::parse(encode(&refused, 6, &text.into_bytes()).unwrap()).unwrap());
        assert!(!item.available.load(Ordering::Relaxed));
    }

    #[test]
    fn introspection_describes_our_objects() {
        for path in ["/", "/catnap", ITEM_PATH, MENU_PATH, instance::PATH] {
            let (signature, body) = introspect(path).unwrap();
            assert_eq!(signature, "s");
            let xml = Reader::new(&body).str().unwrap().to_owned();
            assert!(xml.starts_with("<!DOCTYPE") && xml.ends_with("</node>"), "{path}");
        }
        assert_eq!(introspect("/nope").unwrap_err().0, UNKNOWN_OBJECT);
    }
}
