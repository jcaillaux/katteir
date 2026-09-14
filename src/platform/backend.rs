//! catnap's Slint platform: Slint's own winit backend for every window,
//! except the cat window when the Wayland compositor offers layer-shell
//! (`linux/layer.rs`). That's decided by what the compositor supports, never
//! by its name; GNOME and X11 keep the winit window.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use i_slint_backend_winit::Backend as WinitBackend;
use i_slint_core::graphics::{RequestedGraphicsAPI, RequestedOpenGLVersion};
use slint::PlatformError;
use slint::platform::{Clipboard, EventLoopProxy, Platform, WindowAdapter};

/// Set while the cat window is being created (see `overlay_window`).
static OVERLAY_NEXT: AtomicBool = AtomicBool::new(false);

struct CatnapPlatform {
    winit: WinitBackend,
    #[cfg(target_os = "linux")]
    layer_shell: Option<Rc<super::linux::layer::LayerShell>>,
}

/// Installs catnap's platform. Call once, before any window is created.
pub fn install_slint() -> Result<(), PlatformError> {
    let winit = WinitBackend::builder()
        .with_renderer_name("femtovg")
        .request_graphics_api(RequestedGraphicsAPI::OpenGL(RequestedOpenGLVersion::OpenGLES(None)))
        .build()?;
    let platform = CatnapPlatform {
        winit,
        #[cfg(target_os = "linux")]
        layer_shell: super::linux::layer::LayerShell::connect(),
    };
    slint::platform::set_platform(Box::new(platform))
        .map_err(|error| PlatformError::from(format!("cannot install catnap's Slint platform: {error}")))
}

/// Runs `create`, which creates the cat window, so that the window goes on
/// the overlay layer where the compositor allows it.
pub fn overlay_window<T>(create: impl FnOnce() -> T) -> T {
    OVERLAY_NEXT.store(true, Ordering::Relaxed);
    let window = create();
    OVERLAY_NEXT.store(false, Ordering::Relaxed);
    window
}

// Everything is forwarded to the winit backend, defaults included, so that
// what it overrides (clipboard, the event loop, quit-on-last-window) keeps
// working; only `create_window_adapter` may pick the layer-shell window.
impl Platform for CatnapPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        #[cfg(target_os = "linux")]
        if OVERLAY_NEXT.swap(false, Ordering::Relaxed)
            && let Some(layer_shell) = &self.layer_shell
        {
            let window: Rc<dyn WindowAdapter> = super::linux::layer::LayerWindow::new(Rc::clone(layer_shell));
            return Ok(window);
        }
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
