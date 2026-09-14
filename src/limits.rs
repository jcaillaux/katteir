//! Fixed limits (CLAUDE.md §4). Config values are clamped to these on load,
//! and file inputs are rejected past them.

use std::ops::RangeInclusive;
use std::time::Duration;

/// Most cats catnap will list.
#[allow(dead_code)] // Used by the cat registry (M3).
pub const MAX_CATS: usize = 32;
/// Most frames in one clip (20 s at 30 fps).
pub const MAX_FRAMES_PER_CLIP: usize = 600;
/// Highest clip frame rate.
pub const MAX_CLIP_FPS: u32 = 30;
/// A single AV1 temporal unit bigger than this is certainly corrupt.
pub const MAX_CLIP_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Largest clip file catnap will read.
pub const MAX_CLIP_BYTES: u64 = 64 * 1024 * 1024;

pub const WORK_MINUTES: RangeInclusive<u32> = 1..=180;
pub const WARN_BEFORE_SECS: RangeInclusive<u32> = 0..=300;
/// Break length: 10 s to an hour.
pub const BREAK_SECS: RangeInclusive<u32> = 10..=3600;
pub const DISMISS_HOLD_SECS: RangeInclusive<u32> = 1..=30;

/// Cat names are directory names under `assets/cats/`.
pub const MAX_CAT_NAME_BYTES: usize = 64;
pub const MAX_PATH_BYTES: usize = 4096;
/// Largest config file catnap will read.
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;

/// Largest D-Bus message catnap sends or reads (the spec allows 128 MiB).
/// The biggest planned is the tray icon: a few ARGB sizes, tens of KiB.
pub const MAX_DBUS_MESSAGE_BYTES: usize = 256 * 1024;
/// Deepest container nesting read from a D-Bus signature (the spec allows 64).
pub const MAX_DBUS_DEPTH: usize = 16;
/// Header fields read per D-Bus message (the spec defines 9).
pub const MAX_DBUS_HEADER_FIELDS: usize = 16;
/// Longest line of the D-Bus authentication exchange.
pub const MAX_DBUS_AUTH_LINE_BYTES: usize = 512;
/// Messages read while waiting for one reply; the others are dropped.
pub const MAX_DBUS_MESSAGES_PER_CALL: usize = 64;
/// Entries tried from `DBUS_SESSION_BUS_ADDRESS`.
pub const MAX_DBUS_ADDRESSES: usize = 8;
/// Longest a D-Bus read or write may block.
pub const DBUS_TIMEOUT: Duration = Duration::from_secs(5);
/// Notifications waiting for the notification thread; more are dropped.
pub const NOTIFICATION_QUEUE_DEPTH: usize = 4;
/// Bus messages and state updates waiting for the tray thread.
pub const TRAY_QUEUE_DEPTH: usize = 16;
/// Items, property names or events read from one tray menu request.
pub const MAX_MENU_REQUEST_ITEMS: usize = 64;
/// How often the layer-shell cat window reads its Wayland connection while
/// shown.
pub const LAYER_POLL: Duration = Duration::from_millis(8);
/// Wayland events kept between two polls; more are dropped (pointer motion).
pub const MAX_LAYER_EVENTS: usize = 256;
/// Largest output scale the layer-shell cat window renders at.
pub const MAX_LAYER_SCALE: u8 = 4;
/// Most screens that get a cat, one window each.
pub const MAX_SCREENS: usize = 8;
