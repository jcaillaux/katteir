//! Our Slint platform: Slint's own winit backend for every window,
//! except the cat windows when the Wayland compositor offers layer-shell
//! (`linux/layer.rs`). That's decided by what the compositor supports, never
//! by its name; GNOME and X11 keep one winit window.

use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use i_slint_backend_winit::Backend as WinitBackend;
use i_slint_core::graphics::{RequestedGraphicsAPI, RequestedOpenGLVersion};
use slint::PlatformError;
use slint::platform::{Clipboard, EventLoopProxy, Platform, WindowAdapter};

use crate::limits::MAX_SCREENS;

/// The screen index + 1 of the cat window being created; 0 for any other
/// window (see `overlay_window`).
static OVERLAY_NEXT: AtomicUsize = AtomicUsize::new(0);

/// How many cat windows a break gets: one per output on Wayland compositors
/// with layer-shell, one fullscreen window elsewhere (X11, GNOME) for now.
#[derive(Clone, Default)]
pub struct Screens {
    #[cfg(target_os = "linux")]
    layer_shell: Option<Rc<super::linux::layer::LayerShell>>,
}

impl Screens {
    /// The screens right now (screens can be plugged in between breaks).
    pub fn count(&self) -> usize {
        #[cfg(target_os = "linux")]
        if let Some(layer_shell) = &self.layer_shell {
            return layer_shell.screen_count();
        }
        1
    }
}

struct AppPlatform {
    winit: WinitBackend,
    #[cfg(target_os = "linux")]
    layer_shell: Option<Rc<super::linux::layer::LayerShell>>,
}

/// Installs our platform. Call once, before any window is created.
pub fn install_slint() -> Result<Screens, PlatformError> {
    let winit = WinitBackend::builder()
        .with_renderer_name("femtovg")
        .request_graphics_api(RequestedGraphicsAPI::OpenGL(RequestedOpenGLVersion::OpenGLES(None)))
        .build()?;
    #[cfg(target_os = "linux")]
    let layer_shell = super::linux::layer::LayerShell::connect();
    let screens = Screens {
        #[cfg(target_os = "linux")]
        layer_shell: layer_shell.clone(),
    };
    let platform = AppPlatform {
        winit,
        #[cfg(target_os = "linux")]
        layer_shell,
    };
    slint::platform::set_platform(Box::new(platform))
        .map_err(|error| PlatformError::from(format!("cannot install our Slint platform: {error}")))?;
    Ok(screens)
}

/// Runs `create`, which creates the cat window for `screen`, so that the
/// window goes on that screen's overlay layer where the compositor allows it.
pub fn overlay_window<T>(screen: usize, create: impl FnOnce() -> T) -> T {
    assert!(screen < MAX_SCREENS, "screen {screen}");
    OVERLAY_NEXT.store(screen + 1, Ordering::Relaxed);
    let window = create();
    OVERLAY_NEXT.store(0, Ordering::Relaxed);
    window
}

// Everything is forwarded to the winit backend, defaults included, so that
// what it overrides (clipboard, the event loop, quit-on-last-window) keeps
// working; only `create_window_adapter` may pick a layer-shell window.
impl Platform for AppPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let overlay_screen = OVERLAY_NEXT.swap(0, Ordering::Relaxed).checked_sub(1);
        #[cfg(target_os = "linux")]
        if let (Some(screen), Some(layer_shell)) = (overlay_screen, &self.layer_shell) {
            let window: Rc<dyn WindowAdapter> = super::linux::layer::LayerWindow::new(layer_shell, screen);
            return Ok(window);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = overlay_screen;
        self.winit.create_window_adapter()
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        self.winit.run_event_loop()
    }

    fn process_events(
        &self,
        timeout: Option<Duration>,
        token: i_slint_core::InternalToken,
    ) -> Result<core::ops::ControlFlow<()>, PlatformError> {
        self.winit.process_events(timeout, token)
    }

    fn bind_context(&self, context: i_slint_core::SlintContextWeak, token: i_slint_core::InternalToken) {
        self.winit.bind_context(context, token);
    }

    #[allow(deprecated)] // Still how run_event_loop_until_quit reaches the backend.
    fn set_event_loop_quit_on_last_window_closed(&self, quit_on_last_window_closed: bool) {
        self.winit.set_event_loop_quit_on_last_window_closed(quit_on_last_window_closed);
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        self.winit.new_event_loop_proxy()
    }

    fn duration_since_start(&self) -> Duration {
        self.winit.duration_since_start()
    }

    fn click_interval(&self) -> Duration {
        self.winit.click_interval()
    }

    fn cursor_flash_cycle(&self) -> Duration {
        self.winit.cursor_flash_cycle()
    }

    fn set_clipboard_text(&self, text: &str, clipboard: Clipboard) {
        self.winit.set_clipboard_text(text, clipboard);
    }

    fn clipboard_text(&self, clipboard: Clipboard) -> Option<String> {
        self.winit.clipboard_text(clipboard)
    }

    fn debug_log(&self, arguments: core::fmt::Arguments) {
        self.winit.debug_log(arguments);
    }

    fn open_url(&self, url: &str) -> Result<(), PlatformError> {
        self.winit.open_url(url)
    }
}
