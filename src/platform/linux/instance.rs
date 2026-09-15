//! One instance per session. The first owns the app id as a D-Bus name and
//! answers `Show` there (the tray thread serves it, on the same connection).
//! A second one asks it to show its settings window, then exits instead of
//! starting another timer and tray icon.

use super::bus::{Bus, BusError};
use super::wire::Header;

pub const NAME: &str = crate::app::ID;
pub const PATH: &str = "/Instance";
pub const INTERFACE: &str = concat!(env!("APP_ID"), ".Instance");

pub enum Claim {
    /// This is the only instance: the connection owns the name and must stay
    /// open for as long as the app runs.
    First(Bus),
    /// Another instance runs; it was asked to show its window.
    AlreadyRunning,
}

pub fn claim() -> Result<Claim, BusError> {
    let mut bus = Bus::session()?;
    match bus.request_name(NAME) {
        Ok(()) => Ok(Claim::First(bus)),
        Err(BusError::NameTaken(_)) => {
            // Even if it doesn't answer, it holds the name: don't start a second timer.
            if let Err(error) = bus.call(&Header::method_call(NAME, PATH, INTERFACE, "Show", ""), &[]) {
                log::warn!("the running instance didn't show its window: {error}");
            }
            Ok(Claim::AlreadyRunning)
        }
        Err(error) => Err(error),
    }
}
