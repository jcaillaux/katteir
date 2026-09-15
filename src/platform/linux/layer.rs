//! The cat windows on Wayland compositors with layer-shell (wlroots-based
//! ones such as labwc and Sway, KDE, Hyprland, niri; not GNOME): one surface
//! per screen on the `overlay` layer, above every window, fullscreen apps
//! and panels included. winit can't make layer surfaces, so these are Slint
//! windows of our own on a second Wayland connection, drawn by Slint's
//! `FemtoVG` (OpenGL ES) renderer like the winit windows.
//!
//! The connection lives for the whole run (`LayerShell`). A window's
//! surface, EGL context and GL resources exist only while it's shown, that
//! is during a break (CLAUDE.md §4). While any window is shown, one Slint
//! timer polls the connection and routes events to the window whose surface
//! they're for; there's no extra thread.

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
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, SurfaceData};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{
    CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell as WlrLayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry, delegate_seat,
    delegate_shm, registry_handlers,
};
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalList, registry_queue_init};
use wayland_client::protocol::{wl_output, wl_pointer, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::{
    self, WpFractionalScaleManagerV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{self, WpFractionalScaleV1};
use wayland_protocols::wp::viewporter::client::wp_viewport::{self, WpViewport};
use wayland_protocols::wp::viewporter::client::wp_viewporter::{self, WpViewporter};

use crate::limits::{LAYER_POLL, MAX_LAYER_EVENTS, MAX_LAYER_SCALE, MAX_SCREENS};

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

/// What the Wayland handlers report, for a window to act on after the
/// dispatch (no Slint calls happen inside it).
enum SurfaceEvent {
    /// The compositor's size for the surface, in logical pixels.
    Configure { width: u32, height: u32 },
    /// The output's integer scale (`wl_output`); unused with fractional scaling.
    Scale(i32),
    /// The preferred scale in 120ths (`wp_fractional_scale_v1`).
    FractionalScale(u32),
    Frame,
    Closed,
    Pointer(WindowEvent),
}

/// A `SurfaceEvent` and the surface it's for.
type Routed = (wl_surface::WlSurface, SurfaceEvent);

/// Scales in the fractional-scale protocol's unit: 120 is 1.0.
const SCALE_ONE: u16 = 120;

/// The globals for fractional scaling: the compositor's preferred scale for
/// each surface, and viewports to show a buffer of that size at the
/// surface's logical size. Absent on older compositors: integer scales then.
struct Fractional {
    manager: WpFractionalScaleManagerV1,
    viewporter: WpViewporter,
}

fn bind_fractional(globals: &GlobalList, handle: &QueueHandle<State>) -> Option<Fractional> {
    let manager = match globals.bind::<WpFractionalScaleManagerV1, _, _>(handle, 1..=1, ()) {
        Ok(manager) => manager,
        Err(error) => {
            log::debug!("cat window: no fractional scaling ({error})");
            return None;
        }
    };
    match globals.bind::<WpViewporter, _, _>(handle, 1..=1, ()) {
        Ok(viewporter) => Some(Fractional { manager, viewporter }),
        Err(error) => {
            log::debug!("cat window: no viewporter, so no fractional scaling ({error})");
            manager.destroy();
            None
        }
    }
}

/// A logical length at a scale in 120ths, rounded half away from zero as
/// the fractional-scale protocol asks.
fn physical_px(logical: u32, scale_120: u16) -> u32 {
    let scaled = u64::from(logical) * u64::from(scale_120);
    u32::try_from((scaled + u64::from(SCALE_ONE / 2)) / u64::from(SCALE_ONE)).unwrap_or(u32::MAX)
}

/// A preferred scale from the compositor, kept to what we render at: from
/// half size to `MAX_LAYER_SCALE`.
fn clamp_scale_120(scale_120: u32) -> u16 {
    let highest = u32::from(SCALE_ONE) * u32::from(MAX_LAYER_SCALE);
    u16::try_from(scale_120.clamp(u32::from(SCALE_ONE / 2), highest)).unwrap_or(SCALE_ONE)
}

/// The Wayland connection and globals, for the whole run.
pub struct LayerShell {
    // Dropped in this order: EGL before the connection it came from.
    config: Config,
    display: Display,
    layers: WlrLayerShell,
    fractional: Option<Fractional>,
    /// Every cat window made on this connection.
    windows: RefCell<Vec<Weak<LayerWindow>>>,
    /// Filled by each poll, kept so polling doesn't allocate.
    events: RefCell<Vec<Routed>>,
    /// Runs while any window is shown.
    poll: slint::Timer,
    this: Weak<LayerShell>,
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
                log::debug!("the cat windows go on the Wayland overlay layer");
                Some(shell)
            }
            Err(error) => {
                log::info!("no layer-shell cat window ({error}); it will be a fullscreen window");
                None
            }
        }
    }

    fn try_connect() -> Result<Rc<Self>, LayerError> {
        let connection = Connection::connect_to_env()?;
        let (globals, queue) = registry_queue_init::<State>(&connection)?;
        let handle = queue.handle();
        let layers = WlrLayerShell::bind(&globals, &handle)?;
        let fractional = bind_fractional(&globals, &handle);
        let state = State {
            registry: RegistryState::new(&globals),
            compositor: CompositorState::bind(&globals, &handle)?,
            shm: Shm::bind(&globals, &handle)?,
            seats: SeatState::new(&globals, &handle),
            outputs: OutputState::new(&globals, &handle),
            pointer: None,
            events: Vec::with_capacity(MAX_LAYER_EVENTS),
        };
        let (display, config) = egl_display(&connection)?;
        Ok(Rc::new_cyclic(|this| Self {
            config,
            display,
            layers,
            fractional,
            windows: RefCell::new(Vec::with_capacity(MAX_SCREENS)),
            events: RefCell::new(Vec::with_capacity(MAX_LAYER_EVENTS)),
            poll: slint::Timer::default(),
            this: this.clone(),
            state: RefCell::new(state),
            queue: RefCell::new(queue),
            handle,
            connection,
        }))
    }

    /// The screens (outputs) right now, after a round trip so that screens
    /// plugged in or out since the last break count. At least one.
    pub fn screen_count(&self) -> usize {
        let mut queue = self.queue.borrow_mut();
        let mut state = self.state.borrow_mut();
        if let Err(error) = queue.roundtrip(&mut state) {
            log::warn!("cat window: Wayland round trip failed ({error})");
        }
        state.outputs.outputs().count().clamp(1, MAX_SCREENS)
    }

    fn output(&self, screen: usize) -> Option<wl_output::WlOutput> {
        self.state.borrow().outputs.outputs().nth(screen)
    }

    fn window(&self, index: usize) -> Option<Rc<LayerWindow>> {
        self.windows.borrow().get(index).and_then(Weak::upgrade)
    }

    fn window_for(&self, surface: &wl_surface::WlSurface) -> Option<Rc<LayerWindow>> {
        (0..self.windows.borrow().len()).filter_map(|index| self.window(index)).find(|window| window.owns(surface))
    }

    fn start_polling(&self) {
        if self.poll.running() {
            return;
        }
        let this = self.this.clone();
        self.poll.start(slint::TimerMode::Repeated, LAYER_POLL, move || {
            if let Some(shell) = this.upgrade() {
                shell.pump();
            }
        });
    }

    fn stop_polling_if_idle(&self) {
        let any_shown = (0..self.windows.borrow().len()).any(|index| self.window(index).is_some_and(|w| w.is_shown()));
        if !any_shown {
            self.poll.stop();
        }
    }

    /// One poll: Wayland events in and to their windows, then the redraws
    /// that are due.
    fn pump(&self) {
        let mut events = self.events.borrow_mut();
        if let Err(error) = self.dispatch(&mut events) {
            drop(events);
            log::error!("cat window: the layer-shell connection failed: {error}");
            for index in 0..self.windows.borrow().len() {
                if let Some(window) = self.window(index) {
                    window.hide();
                }
            }
            return;
        }
        for (surface, event) in events.drain(..) {
            if let Some(window) = self.window_for(&surface) {
                window.apply(event);
            }
        }
        drop(events);
        for index in 0..self.windows.borrow().len() {
            if let Some(window) = self.window(index) {
                window.draw_if_due();
            }
        }
    }

    /// Sends pending requests, reads what arrived (never blocking) and runs
    /// the handlers; their events are appended to `events`.
    fn dispatch(&self, events: &mut Vec<Routed>) -> Result<(), LayerError> {
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

    /// A surface on `output`'s overlay layer covering the whole output,
    /// committed so the compositor answers with a configure.
    fn create_overlay(&self, output: &wl_output::WlOutput) -> LayerSurface {
        let surface = self.state.borrow().compositor.create_surface(&self.handle);
        let layer = self.layers.create_layer_surface(&self.handle, surface, Layer::Overlay, Some(crate::app::DIR), Some(output));
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

/// A surface's fractional scale object and viewport, destroyed with it.
struct SurfaceScaling {
    fractional: WpFractionalScaleV1,
    viewport: WpViewport,
}

/// The layer surface of a shown window.
struct Shown {
    layer: LayerSurface,
    /// Fractional scaling for this surface, where the compositor offers it.
    scaling: Option<SurfaceScaling>,
    /// The first configure arrived: the size is known and EGL is attached.
    configured: bool,
    /// A frame callback is outstanding: don't draw until it fires.
    frame_pending: bool,
}

/// One screen's cat window, as a Slint window adapter on the overlay layer.
pub struct LayerWindow {
    window: slint::Window,
    // Owns the `Gl` while shown. Declared before `shown` so that, when
    // dropped, the EGL surface goes before the wl_surface it draws to.
    renderer: FemtoVGOpenGLRenderer,
    shown: RefCell<Option<Shown>>,
    /// Surface size in logical pixels, from the last configure.
    logical_size: Cell<(u32, u32)>,
    /// The scale in 120ths (`SCALE_ONE` is 1.0).
    scale_120: Cell<u16>,
    needs_redraw: Cell<bool>,
    /// The output it covers, by index in the compositor's list.
    screen: usize,
    shell: Rc<LayerShell>,
}

impl LayerWindow {
    pub fn new(shell: &Rc<LayerShell>, screen: usize) -> Rc<Self> {
        assert!(screen < MAX_SCREENS, "screen {screen}");
        let window = Rc::new_cyclic(|this: &Weak<Self>| {
            let adapter: Weak<dyn WindowAdapter> = this.clone();
            Self {
                window: slint::Window::new(adapter),
                renderer: FemtoVGOpenGLRenderer::new_suspended(),
                shown: RefCell::new(None),
                logical_size: Cell::new((1, 1)),
                scale_120: Cell::new(SCALE_ONE),
                needs_redraw: Cell::new(false),
                screen,
                shell: Rc::clone(shell),
            }
        });
        shell.windows.borrow_mut().push(Rc::downgrade(&window));
        window
    }

    fn is_shown(&self) -> bool {
        self.shown.borrow().is_some()
    }

    fn owns(&self, surface: &wl_surface::WlSurface) -> bool {
        self.shown.borrow().as_ref().is_some_and(|shown| shown.layer.wl_surface() == surface)
    }

    fn show(&self) {
        if self.is_shown() {
            return;
        }
        let Some(output) = self.shell.output(self.screen) else {
            log::debug!("cat window: screen {} is gone", self.screen);
            return;
        };
        let layer = self.shell.create_overlay(&output);
        let scaling = self.shell.fractional.as_ref().map(|fractional| {
            let surface = layer.wl_surface();
            SurfaceScaling {
                fractional: fractional.manager.get_fractional_scale(surface, &self.shell.handle, surface.clone()),
                viewport: fractional.viewporter.get_viewport(surface, &self.shell.handle, ()),
            }
        });
        // A new wl_surface starts at scale 1. The scale kept from the last
        // break's surface would make us render at that scale without telling
        // this surface, so it would show oversized (across screens); the
        // compositor's scale event for this surface sets the real one.
        self.scale_120.set(SCALE_ONE);
        *self.shown.borrow_mut() = Some(Shown { layer, scaling, configured: false, frame_pending: false });
        self.shell.start_polling();
    }

    fn hide(&self) {
        let Some(shown) = self.shown.borrow_mut().take() else { return };
        // The renderer drops the EGL surface; then the wl_surface can go.
        if let Err(error) = self.renderer.clear_graphics_context() {
            log::warn!("cat window: {error}");
        }
        if let Some(scaling) = &shown.scaling {
            scaling.viewport.destroy();
            scaling.fractional.destroy();
        }
        drop(shown);
        if let Err(error) = self.shell.connection.flush() {
            log::warn!("cat window: {error}");
        }
        self.shell.stop_polling_if_idle();
    }

    fn apply(&self, event: SurfaceEvent) {
        match event {
            SurfaceEvent::Configure { width, height } => self.configured(width, height),
            SurfaceEvent::Scale(factor) => self.rescale(factor),
            SurfaceEvent::FractionalScale(scale_120) => self.set_scale_120(clamp_scale_120(scale_120)),
            SurfaceEvent::Frame => {
                if let Some(shown) = self.shown.borrow_mut().as_mut() {
                    shown.frame_pending = false;
                }
            }
            SurfaceEvent::Closed => {
                log::warn!("cat window: the compositor closed the layer surface on screen {}", self.screen);
                self.hide();
            }
            SurfaceEvent::Pointer(event) => {
                if self.is_shown() {
                    if matches!(event, WindowEvent::PointerPressed { .. }) {
                        log::debug!("cat window: press on screen {}", self.screen);
                    }
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
        // With fractional scaling the buffer is bigger than the surface: the
        // viewport shows it at the surface's logical size.
        if let Some(scaling) = self.shown.borrow().as_ref().and_then(|shown| shown.scaling.as_ref()) {
            let (width, height) = self.logical_size.get();
            scaling
                .viewport
                .set_destination(i32::try_from(width).unwrap_or(i32::MAX), i32::try_from(height).unwrap_or(i32::MAX));
        }
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

    /// The integer scale of the output the surface is on. Ignored when the
    /// surface has fractional scaling, which gives the exact scale instead.
    fn rescale(&self, factor: i32) {
        if self.shown.borrow().as_ref().is_some_and(|shown| shown.scaling.is_some()) {
            return;
        }
        let scale = u8::try_from(factor.clamp(1, i32::from(MAX_LAYER_SCALE))).unwrap_or(1);
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.layer.wl_surface().set_buffer_scale(i32::from(scale));
        }
        self.set_scale_120(u16::from(scale) * SCALE_ONE);
    }

    fn set_scale_120(&self, scale_120: u16) {
        if scale_120 == self.scale_120.get() {
            return;
        }
        self.scale_120.set(scale_120);
        if self.shown.borrow().as_ref().is_some_and(|shown| shown.configured) {
            self.announce_size();
        }
    }

    /// Tells Slint the scale and size, and asks for a redraw.
    fn announce_size(&self) {
        let scale_factor = f32::from(self.scale_120.get()) / f32::from(SCALE_ONE);
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
        let scale_120 = self.scale_120.get();
        PhysicalSize::new(physical_px(width, scale_120), physical_px(height, scale_120))
    }

    fn renderer(&self) -> &dyn Renderer {
        &self.renderer
    }

    fn request_redraw(&self) {
        self.needs_redraw.set(true);
    }
}

/// What sctk's handlers update. Events for the windows are collected in
/// `events` and taken after each dispatch.
struct State {
    registry: RegistryState,
    compositor: CompositorState,
    shm: Shm,
    seats: SeatState,
    outputs: OutputState,
    /// Sets the cursor when the pointer enters a cat: Wayland leaves that to
    /// the client, and without it the cursor is invisible over our surfaces.
    /// Uses the cursor-shape protocol where the compositor has it, else the
    /// cursor theme.
    pointer: Option<ThemedPointer>,
    events: Vec<Routed>,
}

impl State {
    fn push(&mut self, surface: &wl_surface::WlSurface, event: SurfaceEvent) {
        if self.events.len() < MAX_LAYER_EVENTS {
            self.events.push((surface.clone(), event));
        }
    }

    /// The normal arrow, set each time the pointer enters one of our surfaces.
    fn show_cursor(&self, conn: &Connection) {
        if let Some(pointer) = &self.pointer
            && let Err(error) = pointer.set_cursor(conn, CursorIcon::Default)
        {
            log::debug!("cat window: cursor not set ({error})");
        }
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, factor: i32) {
        self.push(surface, SurfaceEvent::Scale(factor));
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        self.push(surface, SurfaceEvent::Frame);
    }

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}

    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.push(layer.wl_surface(), SurfaceEvent::Closed);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (width, height) = configure.new_size;
        self.push(layer.wl_surface(), SurfaceEvent::Configure { width, height });
    }
}

impl PointerHandler for State {
    fn pointer_frame(&mut self, conn: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for event in events {
            if matches!(event.kind, PointerEventKind::Enter { .. }) {
                self.show_cursor(conn);
            }
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
            self.push(&event.surface, SurfaceEvent::Pointer(slint_event));
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
            let cursor_surface = self.compositor.create_surface(qh);
            let themed = self.seats.get_pointer_with_theme::<Self, SurfaceData>(
                qh,
                &seat,
                self.shm.wl_shm(),
                cursor_surface,
                ThemeSpec::default(),
            );
            match themed {
                Ok(pointer) => self.pointer = Some(pointer),
                Err(error) => log::warn!("cat window: no pointer ({error})"),
            }
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer {
            // Dropping it releases the pointer and its cursor surface.
            self.pointer = None;
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

impl Dispatch<WpFractionalScaleV1, wl_surface::WlSurface> for State {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        surface: &wl_surface::WlSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.push(surface, SurfaceEvent::FractionalScale(scale));
        }
    }
}

// The manager, viewporter and viewports send no events.
impl Dispatch<WpFractionalScaleManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &WpFractionalScaleManagerV1,
        _: wp_fractional_scale_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpViewporter, ()> for State {
    fn event(_: &mut Self, _: &WpViewporter, _: wp_viewporter::Event, (): &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WpViewport, ()> for State {
    fn event(_: &mut Self, _: &WpViewport, _: wp_viewport::Event, (): &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
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
delegate_shm!(State);
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
    fn fractional_sizes_round_half_away_from_zero() {
        // The dev laptop's panel: 1920x1080 at 1.25 is 1536x864 logical.
        assert_eq!(physical_px(1536, 150), 1920);
        assert_eq!(physical_px(864, 150), 1080);
        assert_eq!(physical_px(1280, SCALE_ONE), 1280);
        // 101 x 1.5 = 151.5 rounds up.
        assert_eq!(physical_px(101, 180), 152);
        assert_eq!(physical_px(100, 2 * SCALE_ONE), 200);
    }

    #[test]
    fn preferred_scales_stay_in_range() {
        assert_eq!(clamp_scale_120(150), 150);
        assert_eq!(clamp_scale_120(0), SCALE_ONE / 2);
        assert_eq!(clamp_scale_120(10_000), SCALE_ONE * u16::from(MAX_LAYER_SCALE));
    }

    #[test]
    fn zero_sizes_become_one_pixel() {
        assert_eq!(nonzero(0).get(), 1);
        assert_eq!(nonzero(1920).get(), 1920);
    }

    /// Connects, binds layer-shell, sets up EGL and counts the screens;
    /// shows nothing.
    #[test]
    #[ignore = "needs a Wayland compositor with layer-shell: make test-live"]
    fn the_compositor_offers_layer_shell() {
        let shell = LayerShell::try_connect().unwrap();
        let screens = shell.screen_count();
        eprintln!("layer-shell screens: {screens}, fractional scaling: {}", shell.fractional.is_some());
        let state = shell.state.borrow();
        for output in state.outputs.outputs() {
            if let Some(info) = state.outputs.info(&output) {
                eprintln!("  {:?}: integer scale {}, logical size {:?}", info.name, info.scale_factor, info.logical_size);
            }
        }
        assert!(screens >= 1);
    }
}
