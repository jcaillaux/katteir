//! The app's names. They're written once, in `Cargo.toml`: `build.rs` passes
//! the product name and identifier of `[package.metadata.packager]` in as
//! `APP_NAME` and `APP_ID`, and the crate name comes from Cargo. Constants that need a name inside them (`concat!`) read
//! the same variables, next to where they're used.

/// What people see: window titles, the tray, notifications, the menu.
pub const NAME: &str = env!("APP_NAME");
/// The reverse-DNS id: the Wayland app id (X11 class), the desktop entry and
/// icon names, the single-instance D-Bus name.
pub const ID: &str = env!("APP_ID");
/// The crate name, for folders and namespaces: the config folder
/// (`$XDG_CONFIG_HOME/katteir/`), the runtime folder, the layer-shell
/// namespace.
pub const DIR: &str = env!("CARGO_PKG_NAME");
/// Desktop entries (start at login, and the one packages install) are named
/// after the app id.
pub const DESKTOP_FILE: &str = concat!(env!("APP_ID"), ".desktop");
/// Our own messages at info, dependencies (winit...) only from warn.
/// `RUST_LOG` overrides it.
pub const LOG_FILTER: &str = concat!(env!("CARGO_CRATE_NAME"), "=info,warn");
