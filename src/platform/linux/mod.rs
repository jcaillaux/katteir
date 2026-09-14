//! Linux: a small blocking D-Bus client of catnap's own (`wire`, `bus`),
//! for notifications, the tray icon and the single-instance check. ksni and
//! notify-rust would bring zbus: +1.21 MB, measured on 2026-09-14
//! (CLAUDE.md §2).

mod bus;
pub mod instance;
mod menu;
pub mod notify;
pub mod tray;
mod wire;
