//! Linux: a small blocking D-Bus client of catnap's own (`wire`, `bus`),
//! used for notifications and, next, the tray. ksni and notify-rust would
//! bring zbus: +1.21 MB, measured on 2026-09-14 (CLAUDE.md §2).

mod bus;
pub mod notify;
mod wire;
