//! The tray menu, served over `com.canonical.dbusmenu`: its items, their
//! properties, and the reply bodies that describe them. Pure: the tray
//! passes in its state and gets bytes back.

use super::wire::Writer;
use crate::platform::{TrayAction, TrayState};

/// Item ids. 0 is the root, as dbusmenu requires.
pub const ROOT: i32 = 0;
pub const STATUS: i32 = 1;
pub const START: i32 = 2;
pub const PAUSE: i32 = 3;
pub const STOP: i32 = 4;
pub const SETTINGS: i32 = 5;
pub const QUIT: i32 = 6;
const SEPARATOR_1: i32 = 7;
const SEPARATOR_2: i32 = 8;

/// The root's children, top to bottom.
const CHILDREN: [i32; 8] = [STATUS, SEPARATOR_1, START, PAUSE, STOP, SEPARATOR_2, SETTINGS, QUIT];
/// Every item, the root first.
const ITEMS: [i32; 9] = [ROOT, STATUS, SEPARATOR_1, START, PAUSE, STOP, SEPARATOR_2, SETTINGS, QUIT];
/// The last item: "Quit" and the app's name.
const QUIT_LABEL: &str = concat!("Quit ", env!("APP_NAME"));
/// One item of a layout: id, properties, children.
const ITEM_SIGNATURE: &str = "(ia{sv}av)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Value<'a> {
    Str(&'a str),
    Bool(bool),
}

type Property<'a> = (&'static str, Value<'a>);

pub fn is_item(id: i32) -> bool {
    ITEMS.contains(&id)
}

/// An item's properties. Defaults (a visible, enabled, standard item) are
/// left out, as dbusmenu allows.
fn properties(id: i32, state: &TrayState) -> [Option<Property<'_>>; 2] {
    assert!(is_item(id), "menu item {id}");
    debug_assert!(!state.status.contains('_'), "dbusmenu reads _ as a mnemonic");
    match id {
        ROOT => [Some(("children-display", Value::Str("submenu"))), None],
        STATUS => [Some(("label", Value::Str(&state.status))), Some(("enabled", Value::Bool(false)))],
        SEPARATOR_1 | SEPARATOR_2 => [Some(("type", Value::Str("separator"))), None],
        _ => [Some(("label", Value::Str(label(id)))), Some(("enabled", Value::Bool(enabled(id, state))))],
    }
}

fn label(id: i32) -> &'static str {
    match id {
        START => "Start",
        PAUSE => "Pause",
        STOP => "Stop",
        SETTINGS => "Settings…",
        QUIT => QUIT_LABEL,
        _ => "",
    }
}

fn enabled(id: i32, state: &TrayState) -> bool {
    match id {
        START => state.can_start,
        PAUSE => state.can_pause,
        STOP => state.can_stop,
        _ => true,
    }
}

/// What a click on `id` asks for. Disabled items and the status line ask
/// for nothing.
pub fn clicked(id: i32, state: &TrayState) -> Option<TrayAction> {
    match id {
        START if state.can_start => Some(TrayAction::Start),
        PAUSE if state.can_pause => Some(TrayAction::Pause),
        STOP if state.can_stop => Some(TrayAction::Stop),
        SETTINGS => Some(TrayAction::ShowSettings),
        QUIT => Some(TrayAction::Quit),
        _ => None,
    }
}

/// `GetLayout`'s reply, "u(ia{sv}av)": the revision, then `parent` with
/// its children unless `depth` is 0. `None` if there's no such item.
pub fn layout(revision: u32, parent: i32, depth: i32, wanted: &[&str], state: &TrayState) -> Option<Vec<u8>> {
    if !is_item(parent) {
        return None;
    }
    let mut writer = Writer::with_capacity(1024);
    writer.u32(revision);
    write_item_head(&mut writer, parent, wanted, state);
    let children = writer.begin_array(1);
    if parent == ROOT && depth != 0 {
        for child in CHILDREN {
            writer.variant(ITEM_SIGNATURE);
            write_item_head(&mut writer, child, wanted, state);
            let none = writer.begin_array(1);
            writer.end_array(none);
        }
    }
    writer.end_array(children);
    Some(writer.into_bytes())
}

/// An item's id and properties: a layout item up to its children.
fn write_item_head(writer: &mut Writer, id: i32, wanted: &[&str], state: &TrayState) {
    writer.begin_struct();
    writer.i32(id);
    write_properties(writer, id, wanted, state);
}

/// An item's properties as "a{sv}": those in `wanted`, or all if it's empty.
fn write_properties(writer: &mut Writer, id: i32, wanted: &[&str], state: &TrayState) {
    let dict = writer.begin_array(8);
    for (name, value) in properties(id, state).into_iter().flatten() {
        if wanted.is_empty() || wanted.contains(&name) {
            writer.begin_struct();
            writer.str(name);
            write_value(writer, value);
        }
    }
    writer.end_array(dict);
}

fn write_value(writer: &mut Writer, value: Value<'_>) {
    match value {
        Value::Str(text) => {
            writer.variant("s");
            writer.str(text);
        }
        Value::Bool(flag) => {
            writer.variant("b");
            writer.bool(flag);
        }
    }
}

/// `GetGroupProperties`' reply, "a(ia{sv})", for `ids` (every item if it's
/// empty). Unknown ids are left out.
pub fn group_properties(ids: &[i32], wanted: &[&str], state: &TrayState) -> Vec<u8> {
    let ids = if ids.is_empty() { &ITEMS[..] } else { ids };
    let mut writer = Writer::with_capacity(1024);
    let array = writer.begin_array(8);
    for &id in ids.iter().filter(|&&id| is_item(id)) {
        writer.begin_struct();
        writer.i32(id);
        write_properties(&mut writer, id, wanted, state);
    }
    writer.end_array(array);
    writer.into_bytes()
}

/// `GetProperty`'s reply, "v". `None` if there's no such item or property.
pub fn property(id: i32, name: &str, state: &TrayState) -> Option<Vec<u8>> {
    if !is_item(id) {
        return None;
    }
    let (_, value) = properties(id, state).into_iter().flatten().find(|(key, _)| *key == name)?;
    let mut writer = Writer::with_capacity(64);
    write_value(&mut writer, value);
    Some(writer.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::super::wire::{Reader, WireError};
    use super::*;

    fn working() -> TrayState {
        TrayState { status: "Working: break in 12 min".to_owned(), can_start: false, can_pause: true, can_stop: true }
    }

    fn pair(name: &str, value: &str) -> (String, String) {
        (name.to_owned(), value.to_owned())
    }

    /// Reads an item's "a{sv}" as (name, value) text.
    fn read_properties(reader: &mut Reader<'_>) -> Vec<(String, String)> {
        let end = reader.array(8).unwrap();
        let mut found = Vec::new();
        while reader.more(end).unwrap() {
            reader.begin_struct().unwrap();
            let name = reader.str().unwrap().to_owned();
            let value = match reader.signature().unwrap() {
                "s" => reader.str().unwrap().to_owned(),
                "b" => reader.bool().unwrap().to_string(),
                other => panic!("unexpected property type {other}"),
            };
            found.push((name, value));
        }
        found
    }

    #[test]
    fn layout_lists_the_menu_top_to_bottom() {
        let bytes = layout(7, ROOT, -1, &[], &working()).unwrap();
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.u32(), Ok(7));
        reader.begin_struct().unwrap();
        assert_eq!(reader.i32(), Ok(ROOT));
        assert_eq!(read_properties(&mut reader), [pair("children-display", "submenu")]);
        let children = reader.array(1).unwrap();
        let mut items = Vec::new();
        while reader.more(children).unwrap() {
            assert_eq!(reader.signature(), Ok(ITEM_SIGNATURE));
            reader.begin_struct().unwrap();
            let id = reader.i32().unwrap();
            let properties = read_properties(&mut reader);
            let grandchildren = reader.array(1).unwrap();
            assert_eq!(reader.more(grandchildren), Ok(false));
            items.push((id, properties));
        }
        assert_eq!(reader.u8(), Err(WireError::Truncated));
        let ids: Vec<i32> = items.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, CHILDREN);
        assert_eq!(items[0].1, [pair("label", "Working: break in 12 min"), pair("enabled", "false")]);
        assert_eq!(items[1].1, [pair("type", "separator")]);
        assert_eq!(items[2].1, [pair("label", "Start"), pair("enabled", "false")]);
        assert_eq!(items[3].1, [pair("label", "Pause"), pair("enabled", "true")]);
        assert_eq!(items[7].1, [pair("label", QUIT_LABEL), pair("enabled", "true")]);
    }

    #[test]
    fn layout_honours_depth_filters_and_parents() {
        let bytes = layout(1, ROOT, 0, &["label"], &working()).unwrap();
        let mut reader = Reader::new(&bytes);
        reader.u32().unwrap();
        reader.begin_struct().unwrap();
        reader.i32().unwrap();
        assert_eq!(read_properties(&mut reader), [], "children-display isn't a wanted property");
        let children = reader.array(1).unwrap();
        assert_eq!(reader.more(children), Ok(false), "depth 0 has no children");

        let bytes = layout(1, STOP, -1, &[], &working()).unwrap();
        let mut whole = Reader::new(&bytes);
        whole.skip("u(ia{sv}av)").unwrap();
        assert_eq!(whole.u8(), Err(WireError::Truncated));
        assert_eq!(layout(1, 99, -1, &[], &working()), None);
    }

    #[test]
    fn group_properties_skip_unknown_ids() {
        let bytes = group_properties(&[PAUSE, 99], &["enabled"], &working());
        let mut reader = Reader::new(&bytes);
        let end = reader.array(8).unwrap();
        reader.begin_struct().unwrap();
        assert_eq!(reader.i32(), Ok(PAUSE));
        assert_eq!(read_properties(&mut reader), [pair("enabled", "true")]);
        assert_eq!(reader.more(end), Ok(false));

        let all = group_properties(&[], &[], &working());
        let mut reader = Reader::new(&all);
        reader.skip("a(ia{sv})").unwrap();
        assert_eq!(reader.u8(), Err(WireError::Truncated));
    }

    #[test]
    fn single_properties() {
        let bytes = property(QUIT, "label", &working()).unwrap();
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.signature(), Ok("s"));
        assert_eq!(reader.str(), Ok(QUIT_LABEL));
        assert_eq!(property(QUIT, "icon-name", &working()), None);
        assert_eq!(property(99, "label", &working()), None);
    }

    #[test]
    fn clicks_on_disabled_items_do_nothing() {
        let state = working();
        assert_eq!(clicked(START, &state), None);
        assert_eq!(clicked(PAUSE, &state), Some(TrayAction::Pause));
        assert_eq!(clicked(STOP, &state), Some(TrayAction::Stop));
        assert_eq!(clicked(STATUS, &state), None);
        assert_eq!(clicked(SETTINGS, &state), Some(TrayAction::ShowSettings));
        assert_eq!(clicked(QUIT, &state), Some(TrayAction::Quit));
        assert_eq!(clicked(99, &state), None);
    }
}
