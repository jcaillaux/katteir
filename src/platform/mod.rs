//! What differs by OS (CLAUDE.md §5). So far: desktop notifications, on
//! Linux through catnap's own small D-Bus client (`linux/`). On other
//! systems they're only logged until M2 reaches macOS and Windows.

#[cfg(target_os = "linux")]
mod linux;

pub struct Platform {
    #[cfg(target_os = "linux")]
    notifier: Option<linux::notify::Notifier>,
}

impl Platform {
    /// Sets up what the platform offers. Nothing here is fatal: without a
    /// notification service, notifications are only logged.
    pub fn start() -> Self {
        #[cfg(target_os = "linux")]
        let notifier = linux::notify::Notifier::start()
            .map_err(|error| log::warn!("no desktop notifications: {error}"))
            .ok();
        Self {
            #[cfg(target_os = "linux")]
            notifier,
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
}
