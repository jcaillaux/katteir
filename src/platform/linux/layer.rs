//! The cat window on Wayland compositors with layer-shell (wlroots-based
//! ones such as labwc and Sway, KDE, Hyprland, niri; not GNOME): a surface
//! on the `overlay` layer, above every window, fullscreen apps and panels
//! included. winit can't make layer surfaces, so this is a Slint window of
//! our own on a second Wayland connection, drawn by Slint's `FemtoVG`
//! (OpenGL ES) renderer like the winit windows.
//!
//! The connection lives for the whole run (`LayerShell`). The surface, its
//! EGL context and the renderer's GL resources exist only while the window
//! is shown, that is during a break (CLAUDE.md §4). While shown, a Slint
//! timer polls the connection; there's no extra thread.

#![allow(unsafe_code)] // Raw Wayland handles for EGL; each unsafe block says why it's sound.

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::ffi::{CStr, c_void};
use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};

use glutin::config::{Api, Config, ConfigSurfaceTypes, ConfigTemplateBuilder, GlConfig};
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext, PossiblyCurrentGlContext, Version,
};
use glutin::display::{Display, DisplayApiPreference, GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface, Surface, SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use i_slint_renderer_femtovg::opengl::OpenGLInterface;
use i_slint_renderer_femtovg::{FemtoVGOpenGLRenderer, FemtoVGOpenGLRendererExt, FemtoVGRendererExt};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use slint::platform::{PointerEventButton, Renderer, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalSize, PlatformError};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell as WlrLayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry, delegate_seat,
    registry_handlers,
};
use wayland_client::backend::WaylandError;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_output, wl_pointer, wl_seat, wl_surface};
use wayland_client::{Connection, EventQueue, Proxy, QueueHandle};

use crate::limits::{LAYER_POLL, MAX_LAYER_EVENTS, MAX_LAYER_SCALE};

/// Mouse buttons, from linux/input-event-codes.h.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

#[derive(Debug, thiserror::Error)]
pub enum LayerError {
    #[error("no Wayland compositor: {0}")]
    Connect(#[from] wayland_client::ConnectError),
    #[error("Wayland globals: {0}")]
    Globals(#[from] wayland_client::globals::GlobalError),
    #[error("the compositor lacks a protocol: {0}")]
    Missing(#[from] wayland_client::globals::BindError),
    #[error("Wayland: {0}")]
    Wayland(#[from] WaylandError),
    #[error("Wayland: {0}")]
    Dispatch(#[from] wayland_client::DispatchError),
    #[error("EGL: {0}")]
    Egl(#[from] glutin::error::Error),
    #[error("EGL offers no configuration with alpha")]
    NoConfig,
    #[error("a Wayland handle is null")]
    NullHandle,
    #[error("the cat window isn't shown")]
    NotShown,
    #[error(transparent)]
    Slint(#[from] PlatformError),
}

/// What the Wayland handlers report, for the window to act on after the
/// dispatch (no Slint calls happen inside it).
enum SurfaceEvent {
    /// The compositor's size for the surface, in logical pixels.
    Configure { width: u32, height: u32 },
    Scale(i32),
    Frame,
    Closed,
    Pointer(WindowEvent),
}

/// The Wayland connection and globals, for the whole run.
pub struct LayerShell {
    // Dropped in this order: EGL before the connection it came from.
    config: Config,
    display: Display,
    compositor: CompositorState,
    layers: WlrLayerShell,
    state: RefCell<State>,
    queue: RefCell<EventQueue<State>>,
    handle: QueueHandle<State>,
    connection: Connection,
}

impl LayerShell {
    /// Connects if the compositor offers layer-shell: `None` on X11, on
    /// GNOME, or on any error, which is logged.
    pub fn connect() -> Option<Rc<Self>> {
        match Self::try_connect() {
            Ok(shell) => {
                log::debug!("the cat window goes on the Wayland overlay layer");
                Some(Rc::new(shell))
            }
            Err(error) => {
                log::info!("no layer-shell cat window ({error}); it will be a fullscreen window");
                None
            }
        }
    }

    fn try_connect() -> Result<Self, LayerError> {
        let connection = Connection::connect_to_env()?;
        let (globals, queue) = registry_queue_init::<State>(&connection)?;
        let handle = queue.handle();
        let compositor = CompositorState::bind(&globals, &handle)?;
        let layers = WlrLayerShell::bind(&globals, &handle)?;
        let state = State {
            registry: RegistryState::new(&globals),
            seats: SeatState::new(&globals, &handle),
            outputs: OutputState::new(&globals, &handle),
            pointer: None,
            events: Vec::with_capacity(MAX_LAYER_EVENTS),
        };
        let (display, config) = egl_display(&connection)?;
        Ok(Self {
            config,
            display,
            compositor,
            layers,
            state: RefCell::new(state),
            queue: RefCell::new(queue),
            handle,
            connection,
        })
    }

    /// Sends pending requests, reads what arrived (never blocking) and runs
    /// the handlers; their events are appended to `events`.
    fn dispatch(&self, events: &mut Vec<SurfaceEvent>) -> Result<(), LayerError> {
        let mut queue = self.queue.borrow_mut();
        let mut state = self.state.borrow_mut();
        queue.dispatch_pending(&mut state)?;
        self.connection.flush()?;
        if let Some(guard) = self.connection.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
        queue.dispatch_pending(&mut state)?;
        assert!(state.events.len() <= MAX_LAYER_EVENTS);
        events.append(&mut state.events);
        Ok(())
    }

    /// A surface on the overlay layer covering the output the compositor
    /// picks, committed so the compositor answers with a configure.
    fn create_overlay(&self) -> LayerSurface {
        let surface = self.compositor.create_surface(&self.handle);
        let layer = self.layers.create_layer_surface(&self.handle, surface, Layer::Overlay, Some("catnap"), None);
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        // Over panels too. The keyboard stays with the focused app.
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        layer
    }

    /// An EGL context and window surface drawing to `surface`.
    fn gl_for(&self, surface: &wl_surface::WlSurface, size: PhysicalSize) -> Result<Gl, LayerError> {
        let pointer = NonNull::new(surface.id().as_ptr().cast::<c_void>()).ok_or(LayerError::NullHandle)?;
        let window = RawWindowHandle::Wayland(WaylandWindowHandle::new(pointer));
        let attributes =
            ContextAttributesBuilder::new().with_context_api(ContextApi::Gles(Some(Version::new(2, 0)))).build(Some(window));
        // SAFETY: `window` is a live wl_surface on this connection, and it
        // outlives the context and EGL surface: `LayerWindow::hide` has the
        // renderer drop them before the layer surface goes.
        let context = unsafe { self.display.create_context(&self.config, &attributes)? };
        let surface_attributes =
            SurfaceAttributesBuilder::<WindowSurface>::new().build(window, nonzero(size.width), nonzero(size.height));
        // SAFETY: as above.
        let egl_surface = unsafe { self.display.create_window_surface(&self.config, &surface_attributes)? };
        let context = context.make_current(&egl_surface)?;
        // Frames are paced by frame callbacks: swapping must never block the UI thread.
        if let Err(error) = egl_surface.set_swap_interval(&context, SwapInterval::DontWait) {
            log::debug!("cat window: no swap interval control ({error})");
        }
        Ok(Gl { context, surface: egl_surface })
    }
}

fn egl_display(connection: &Connection) -> Result<(Display, Config), LayerError> {
    let pointer = NonNull::new(connection.backend().display_ptr().cast::<c_void>()).ok_or(LayerError::NullHandle)?;
    let handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(pointer));
    // SAFETY: the wl_display belongs to `connection`, which `LayerShell`
    // keeps open for longer than the EGL display (its field order).
    let display = unsafe { Display::new(handle, DisplayApiPreference::Egl)? };
    let template = ConfigTemplateBuilder::new()
        .with_alpha_size(8)
        .with_surface_type(ConfigSurfaceTypes::WINDOW)
        .with_api(Api::GLES2)
        .build();
    // SAFETY: the template is plain data and the display is valid.
    let configs = unsafe { display.find_configs(template)? };
    let config = configs.reduce(prefer_transparency).ok_or(LayerError::NoConfig)?;
    Ok((display, config))
}

fn prefer_transparency(best: Config, next: Config) -> Config {
    let transparent = |config: &Config| config.supports_transparency().unwrap_or(false);
    if transparent(&next) && !transparent(&best) { next } else { best }
}

fn nonzero(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or(NonZeroU32::MIN)
}

/// The EGL context and surface of a shown cat window; the renderer owns it.
struct Gl {
    context: PossiblyCurrentContext,
    surface: Surface<WindowSurface>,
}

// SAFETY: the context and surface were made for each other (`gl_for`), and
// the renderer only calls these on the UI thread that created them.
unsafe impl OpenGLInterface for Gl {
    fn ensure_current(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if !self.context.is_current() {
            self.context.make_current(&self.surface)?;
        }
        Ok(())
    }

    fn swap_buffers(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.surface.swap_buffers(&self.context)?;
        Ok(())
    }

    fn resize(&self, width: NonZeroU32, height: NonZeroU32) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.ensure_current()?;
        self.surface.resize(&self.context, width, height);
        Ok(())
    }

    fn get_proc_address(&self, name: &CStr) -> *const c_void {
        self.context.display().get_proc_address(name)
    }
}

/// The layer surface of a shown window.
struct Shown {
    layer: LayerSurface,
    /// The first configure arrived: the size is known and EGL is attached.
    configured: bool,
    /// A frame callback is outstanding: don't draw until it fires.
    frame_pending: bool,
}

/// The cat window as a Slint window adapter on the overlay layer.
pub struct LayerWindow {
    window: slint::Window,
    // Owns the `Gl` while shown. Declared before `shown` so that, when
    // dropped, the EGL surface goes before the wl_surface it draws to.
    renderer: FemtoVGOpenGLRenderer,
    shown: RefCell<Option<Shown>>,
    /// Surface size in logical pixels, from the last configure.
    logical_size: Cell<(u32, u32)>,
    scale: Cell<u8>,
    needs_redraw: Cell<bool>,
    /// Filled by each poll, kept so polling doesn't allocate.
    events: RefCell<Vec<SurfaceEvent>>,
    poll: slint::Timer,
    this: Weak<LayerWindow>,
    shell: Rc<LayerShell>,
}

impl LayerWindow {
    pub fn new(shell: Rc<LayerShell>) -> Rc<Self> {
        Rc::new_cyclic(|this: &Weak<Self>| {
            let adapter: Weak<dyn WindowAdapter> = this.clone();
            Self {
                window: slint::Window::new(adapter),
                renderer: FemtoVGOpenGLRenderer::new_suspended(),
                shown: RefCell::new(None),
                logical_size: Cell::new((1, 1)),
                scale: Cell::new(1),
                needs_redraw: Cell::new(false),
                events: RefCell::new(Vec::with_capacity(MAX_LAYER_EVENTS)),
                poll: slint::Timer::default(),
                this: this.clone(),
                shell,
            }
        })
    }

    fn show(&self) {
        if self.shown.borrow().is_some() {
            return;
        }
        let layer = self.shell.create_overlay();
        *self.shown.borrow_mut() = Some(Shown { layer, configured: false, frame_pending: false });
        let this = self.this.clone();
        self.poll.start(slint::TimerMode::Repeated, LAYER_POLL, move || {
            if let Some(window) = this.upgrade() {
                window.pump();
            }
        });
    }

    fn hide(&self) {
        self.poll.stop();
        let Some(shown) = self.shown.borrow_mut().take() else { return };
        // The renderer drops the EGL surface; then the wl_surface can go.
        if let Err(error) = self.renderer.clear_graphics_context() {
            log::warn!("cat window: {error}");
        }
        drop(shown);
        if let Err(error) = self.shell.connection.flush() {
            log::warn!("cat window: {error}");
        }
    }

    /// One poll: Wayland events in, then a redraw if one is due.
    fn pump(&self) {
        let mut events = self.events.borrow_mut();
        if let Err(error) = self.shell.dispatch(&mut events) {
            drop(events);
            log::error!("cat window: the layer-shell connection failed: {error}");
            self.hide();
            return;
        }
        for event in events.drain(..) {
            self.apply(event);
        }
        drop(events);
        self.draw_if_due();
    }

    fn apply(&self, event: SurfaceEvent) {
        match event {
            SurfaceEvent::Configure { width, height } => self.configured(width, height),
            SurfaceEvent::Scale(factor) => self.rescale(factor),
            SurfaceEvent::Frame => {
                if let Some(shown) = self.shown.borrow_mut().as_mut() {
                    shown.frame_pending = false;
                }
            }
            SurfaceEvent::Closed => {
                log::warn!("cat window: the compositor closed its layer surface");
                self.hide();
            }
            SurfaceEvent::Pointer(event) => {
                if self.shown.borrow().is_some() {
                    self.window.dispatch_event(event);
                }
            }
        }
    }

    fn configured(&self, width: u32, height: u32) {
        let first = {
            let mut shown = self.shown.borrow_mut();
            let Some(shown) = shown.as_mut() else { return };
            !std::mem::replace(&mut shown.configured, true)
        };
        self.logical_size.set((width.max(1), height.max(1)));
        if first && let Err(error) = self.attach_gl() {
            log::error!("cat window: {error}");
            self.hide();
            return;
        }
        self.announce_size();
    }

    fn attach_gl(&self) -> Result<(), LayerError> {
        let surface =
            self.shown.borrow().as_ref().map(|shown| shown.layer.wl_surface().clone()).ok_or(LayerError::NotShown)?;
        let gl = self.shell.gl_for(&surface, self.size())?;
        self.renderer.set_opengl_context(gl)?;
        Ok(())
    }

    fn rescale(&self, factor: i32) {
        let scale = u8::try_from(factor.clamp(1, i32::from(MAX_LAYER_SCALE))).unwrap_or(1);
        if scale == self.scale.get() {
            return;
        }
        self.scale.set(scale);
        let configured = self.shown.borrow().as_ref().map(|shown| {
            shown.layer.wl_surface().set_buffer_scale(i32::from(scale));
            shown.configured
        });
        if configured == Some(true) {
            self.announce_size();
        }
    }

    /// Tells Slint the scale and size, and asks for a redraw.
    fn announce_size(&self) {
        let scale_factor = f32::from(self.scale.get());
        self.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor });
        self.window.dispatch_event(WindowEvent::Resized { size: self.size().to_logical(scale_factor) });
        self.needs_redraw.set(true);
    }

    fn draw_if_due(&self) {
        {
            let mut shown = self.shown.borrow_mut();
            let Some(shown) = shown.as_mut() else { return };
            if !shown.configured || shown.frame_pending || !self.needs_redraw.get() {
                return;
            }
            // Ask for the next frame callback before the swap commits.
            let surface = shown.layer.wl_surface();
            surface.frame(&self.shell.handle, surface.clone());
            shown.frame_pending = true;
        }
        self.needs_redraw.set(false);
        if let Err(error) = self.renderer.render() {
            log::error!("cat window: {error}");
        }
        if self.window.has_active_animations() {
            self.needs_redraw.set(true);
        }
    }
}

impl WindowAdapter for LayerWindow {
    fn window(&self) -> &slint::Window {
        &self.window
    }

    fn set_visible(&self, visible: bool) -> Result<(), PlatformError> {
        if visible {
            self.show();
        } else {
            self.hide();
        }
        Ok(())
    }

    fn size(&self) -> PhysicalSize {
        let (width, height) = self.logical_size.get();
        let scale = u32::from(self.scale.get());
        PhysicalSize::new(width.saturating_mul(scale), height.saturating_mul(scale))
    }

    fn renderer(&self) -> &dyn Renderer {
        &self.renderer
    }

    fn request_redraw(&self) {
        self.needs_redraw.set(true);
    }
}

/// What sctk's handlers update. Events for the window are collected in
/// `events` and taken after each dispatch.
struct State {
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    pointer: Option<wl_pointer::WlPointer>,
    events: Vec<SurfaceEvent>,
}

impl State {
    fn push(&mut self, event: SurfaceEvent) {
        if self.events.len() < MAX_LAYER_EVENTS {
            self.events.push(event);
        }
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, factor: i32) {
        self.push(SurfaceEvent::Scale(factor));
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.push(SurfaceEvent::Frame);
    }

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}

    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.push(SurfaceEvent::Closed);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (width, height) = configure.new_size;
        self.push(SurfaceEvent::Configure { width, height });
    }
}

impl PointerHandler for State {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for event in events {
            let position = logical(event.position);
            let slint_event = match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => WindowEvent::PointerMoved { position },
                PointerEventKind::Leave { .. } => WindowEvent::PointerExited,
                PointerEventKind::Press { button, .. } => {
                    WindowEvent::PointerPressed { position, button: slint_button(button) }
                }
                PointerEventKind::Release { button, .. } => {
                    WindowEvent::PointerReleased { position, button: slint_button(button) }
                }
                PointerEventKind::Axis { .. } => continue,
            };
            self.push(SurfaceEvent::Pointer(slint_event));
        }
    }
}

#[allow(clippy::cast_possible_truncation)] // Surface coordinates fit an f32.
fn logical((x, y): (f64, f64)) -> LogicalPosition {
    LogicalPosition::new(x as f32, y as f32)
}

fn slint_button(button: u32) -> PointerEventButton {
    match button {
        BTN_LEFT => PointerEventButton::Left,
        BTN_RIGHT => PointerEventButton::Right,
        BTN_MIDDLE => PointerEventButton::Middle,
        _ => PointerEventButton::Other,
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seats.get_pointer(qh, &seat) {
                Ok(pointer) => self.pointer = Some(pointer),
                Err(error) => log::warn!("cat window: no pointer ({error})"),
            }
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            pointer.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(State);
delegate_output!(State);
delegate_seat!(State);
delegate_pointer!(State);
delegate_layer!(State);
delegate_registry!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_buttons_map_to_slint_buttons() {
        assert_eq!(slint_button(BTN_LEFT), PointerEventButton::Left);
        assert_eq!(slint_button(BTN_RIGHT), PointerEventButton::Right);
        assert_eq!(slint_button(BTN_MIDDLE), PointerEventButton::Middle);
        assert_eq!(slint_button(0x113), PointerEventButton::Other);
    }

    #[test]
    fn zero_sizes_become_one_pixel() {
        assert_eq!(nonzero(0).get(), 1);
        assert_eq!(nonzero(1920).get(), 1920);
    }

    /// Connects, binds layer-shell and sets up EGL; shows nothing.
    #[test]
    #[ignore = "needs a Wayland compositor with layer-shell: make test-live"]
    fn the_compositor_offers_layer_shell() {
        LayerShell::try_connect().unwrap();
    }
}
