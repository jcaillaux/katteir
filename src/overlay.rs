//! The cat windows (CLAUDE.md §3): one per screen, shown when a break starts
//! and hidden when it ends. They share one decoder and one hold-to-dismiss:
//! each decoded frame goes to every window, which uploads it to its own GL
//! context, and holding the button on any screen ends the break everywhere.
//! They're always overlays: fullscreen, see-through, on top where the
//! platform allows (see `ui/cat.slint` and `platform/backend.rs`).
//!
//! Frames are paced by drawing: a tick at the clip rate takes the next frame
//! only once the first screen has drawn the previous one. When the
//! compositor stops drawing, decoding stops too (the decoder blocks on its
//! full queue).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use slint::ComponentHandle;

use crate::CatWindow;
use crate::cats::CatClips;
use crate::hold::HoldToDismiss;
use crate::icon;
use crate::limits::MAX_SCREENS;
use crate::platform::{self, Screens};
use crate::timer;
use crate::video::decode::Decoder;
use crate::video::gl::{self, GlVideo};

/// How often the hold progress is refreshed while the button is held.
const HOLD_TICK: Duration = Duration::from_millis(33);
/// Delay between showing the windows and starting the slide-in, so the cat
/// starts off-screen at the window's final size.
const ARRIVE_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("cat window: {0}")]
    Platform(#[from] slint::PlatformError),
    #[error("cat window rendering: {0}")]
    Rendering(#[from] slint::SetRenderingNotifierError),
    #[error("cannot start the decoder: {0}")]
    Decoder(#[from] std::io::Error),
}

/// State shared by the frame tick, the renderers and the hold callbacks.
#[derive(Default)]
struct Playback {
    decoder: Option<Decoder>,
    /// The first screen hasn't drawn the last frame yet: don't take the next.
    awaiting_draw: bool,
    hold_remaining_ticks: u32,
    stacked_size_px: (u32, u32),
    hold: Option<HoldToDismiss>,
}

/// A frame waiting for one window's next draw. Pictures are reference
/// counted, so every window holding the same frame costs nothing.
type FrameSlot = Rc<RefCell<Option<dav1d::Picture>>>;

/// One screen's cat window.
struct Screen {
    window: CatWindow,
    frame: FrameSlot,
}

type Callback = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

pub struct Overlay {
    screens: Screens,
    /// One per screen seen so far, reused across breaks. The first paces
    /// the video.
    windows: RefCell<Vec<Screen>>,
    /// How many windows the current break uses; 0 when hidden.
    shown_count: Cell<usize>,
    playback: Rc<RefCell<Playback>>,
    frame_timer: slint::Timer,
    hold_timer: slint::Timer,
    arrive_timer: slint::Timer,
    on_dismissed: Callback,
    this: Weak<Overlay>,
}

impl Overlay {
    /// Creates the first (hidden) cat window; the others come with the
    /// breaks that need them.
    pub fn new(screens: Screens) -> Result<Rc<Self>, OverlayError> {
        let overlay = Rc::new_cyclic(|this| Self {
            screens,
            windows: RefCell::new(Vec::with_capacity(MAX_SCREENS)),
            shown_count: Cell::new(0),
            playback: Rc::new(RefCell::new(Playback::default())),
            frame_timer: slint::Timer::default(),
            hold_timer: slint::Timer::default(),
            arrive_timer: slint::Timer::default(),
            on_dismissed: Rc::new(RefCell::new(None)),
            this: this.clone(),
        });
        overlay.ensure_windows(1)?;
        Ok(overlay)
    }

    /// Called when a completed hold dismisses the break.
    pub fn on_dismissed(&self, callback: impl Fn() + 'static) {
        *self.on_dismissed.borrow_mut() = Some(Rc::new(callback));
    }

    /// Shows a cat on every screen and starts playing `clips`. `hold` is how
    /// long the dismiss button must be held.
    pub fn show(&self, clips: &CatClips, hold: Duration) -> Result<(), OverlayError> {
        self.hide();
        let count = self.screens.count().clamp(1, MAX_SCREENS);
        self.ensure_windows(count)?;
        let decoder = Decoder::start(clips.entry.clone(), clips.looped.clone())?;
        let base_fps = decoder.base_fps();
        *self.playback.borrow_mut() = Playback {
            decoder: Some(decoder),
            stacked_size_px: clips.entry.stacked_size_px(),
            hold: Some(HoldToDismiss::new(hold)),
            ..Playback::default()
        };
        let hold_text = slint::SharedString::from(format!("Hold {} s to dismiss", hold.as_secs()));
        self.shown_count.set(count);
        for index in 0..count {
            let Some(window) = self.window(index) else { continue };
            window.set_arrived(false);
            window.set_hold_progress(0.0);
            window.set_hold_text(hold_text.clone());
            window.window().set_fullscreen(true);
            window.show()?;
        }
        self.start_frames(base_fps);
        let this = self.this.clone();
        self.arrive_timer.start(slint::TimerMode::SingleShot, ARRIVE_DELAY, move || {
            if let Some(overlay) = this.upgrade() {
                overlay.each_shown(|window| window.set_arrived(true));
            }
        });
        Ok(())
    }

    /// Hides the windows and stops the decoder. Does nothing when hidden.
    pub fn hide(&self) {
        let count = self.shown_count.replace(0);
        if count == 0 {
            return;
        }
        self.frame_timer.stop();
        self.hold_timer.stop();
        self.arrive_timer.stop();
        let decoder = {
            let mut playback = self.playback.borrow_mut();
            playback.hold = None;
            playback.decoder.take()
        };
        // Joins the decoder thread, outside the borrow.
        drop(decoder);
        for screen in self.windows.borrow().iter() {
            screen.frame.borrow_mut().take();
        }
        for index in 0..count {
            let Some(window) = self.window(index) else { continue };
            if let Err(error) = window.hide() {
                log::warn!("cannot hide the cat window on screen {index}: {error}");
            }
            window.set_arrived(false);
        }
    }

    /// Updates the break countdown.
    pub fn set_break_left(&self, left: Duration) {
        let text = slint::SharedString::from(timer::minutes_seconds(left));
        self.each_shown(|window| window.set_countdown(text.clone()));
    }

    fn window(&self, index: usize) -> Option<CatWindow> {
        self.windows.borrow().get(index).map(|screen| screen.window.clone_strong())
    }

    fn each_shown(&self, action: impl Fn(&CatWindow)) {
        for index in 0..self.shown_count.get() {
            if let Some(window) = self.window(index) {
                action(&window);
            }
        }
    }

    /// Creates cat windows up to `count`, each for its own screen.
    fn ensure_windows(&self, count: usize) -> Result<(), OverlayError> {
        assert!((1..=MAX_SCREENS).contains(&count));
        let existing = self.windows.borrow().len();
        for index in existing..count {
            let window = platform::overlay_window(index, CatWindow::new)?;
            window.set_window_icon(icon::window_icon());
            let frame: FrameSlot = Rc::new(RefCell::new(None));
            self.install_renderer(&window, &frame, index == 0)?;
            self.install_hold(&window);
            self.windows.borrow_mut().push(Screen { window, frame });
        }
        Ok(())
    }

    fn start_frames(&self, base_fps: u32) {
        assert!(base_fps > 0);
        let this = self.this.clone();
        let interval = Duration::from_secs_f64(1.0 / f64::from(base_fps));
        self.frame_timer.start(slint::TimerMode::Repeated, interval, move || {
            if let Some(overlay) = this.upgrade() {
                overlay.frame_tick();
            }
        });
    }

    /// Takes the next frame when it's due and hands it to every window.
    fn frame_tick(&self) {
        let picture = {
            let mut playback = self.playback.borrow_mut();
            if playback.awaiting_draw {
                return;
            }
            if playback.hold_remaining_ticks > 0 {
                playback.hold_remaining_ticks -= 1;
                return;
            }
            let next = playback.decoder.as_ref().map(Decoder::try_next);
            match next {
                Some(Ok(frame)) => {
                    assert!(frame.hold_ticks >= 1);
                    playback.hold_remaining_ticks = frame.hold_ticks - 1;
                    playback.awaiting_draw = true;
                    frame.picture
                }
                Some(Err(TryRecvError::Disconnected)) => {
                    log::warn!("the decoder stopped; the cat stays on its last frame");
                    playback.decoder = None;
                    return;
                }
                Some(Err(TryRecvError::Empty)) | None => return,
            }
        };
        let windows = self.windows.borrow();
        for screen in windows.iter().take(self.shown_count.get()) {
            *screen.frame.borrow_mut() = Some(picture.clone());
            screen.window.window().request_redraw();
        }
    }

    /// Draws the window's waiting frame before each render. `paces`: this is
    /// the window whose draws let the next frame through.
    fn install_renderer(
        &self,
        window: &CatWindow,
        frame: &FrameSlot,
        paces: bool,
    ) -> Result<(), slint::SetRenderingNotifierError> {
        let (playback, frame, weak) = (self.playback.clone(), Rc::clone(frame), window.as_weak());
        let mut context: Option<Rc<glow::Context>> = None;
        let mut video: Option<GlVideo> = None;
        window.window().set_rendering_notifier(move |state, graphics_api| match state {
            slint::RenderingState::RenderingSetup => context = gl::context(graphics_api),
            slint::RenderingState::BeforeRendering => {
                let (Some(window), Some(context)) = (weak.upgrade(), context.as_ref()) else { return };
                let Some(picture) = frame.borrow_mut().take() else { return };
                let size = playback.borrow().stacked_size_px;
                if video.as_ref().is_none_or(|video| video.stacked_size_px() != size) {
                    video = GlVideo::new(context.clone(), size.0, size.1)
                        .map_err(|error| log::error!("cannot set up the video shader: {error}"))
                        .ok();
                }
                if let Some(video) = video.as_mut() {
                    window.set_frame(video.draw_frame(&picture));
                }
                if paces {
                    playback.borrow_mut().awaiting_draw = false;
                }
            }
            slint::RenderingState::RenderingTeardown => {
                video = None;
                context = None;
            }
            _ => {}
        })
    }

    fn install_hold(&self, window: &CatWindow) {
        let this = self.this.clone();
        window.on_hold_pressed(move || {
            if let Some(overlay) = this.upgrade() {
                overlay.hold_pressed();
            }
        });
        let this = self.this.clone();
        window.on_hold_released(move || {
            if let Some(overlay) = this.upgrade() {
                overlay.hold_released();
            }
        });
    }

    fn hold_pressed(&self) {
        {
            let mut playback = self.playback.borrow_mut();
            let Some(hold) = playback.hold.as_mut() else { return };
            hold.press(Instant::now());
        }
        let this = self.this.clone();
        self.hold_timer.start(slint::TimerMode::Repeated, HOLD_TICK, move || {
            if let Some(overlay) = this.upgrade() {
                overlay.hold_tick();
            }
        });
    }

    fn hold_tick(&self) {
        let now = Instant::now();
        let Some((done, progress)) = self
            .playback
            .borrow_mut()
            .hold
            .as_mut()
            .map(|hold| (hold.tick(now), hold.progress(now)))
        else {
            return;
        };
        self.each_shown(|window| window.set_hold_progress(progress));
        if done {
            // Cloned out first: the callback hides these windows, which
            // borrows the playback state again.
            let callback = self.on_dismissed.borrow().clone();
            if let Some(callback) = callback {
                callback();
            }
        }
    }

    fn hold_released(&self) {
        if let Some(hold) = self.playback.borrow_mut().hold.as_mut() {
            hold.release();
        }
        self.hold_timer.stop();
        self.each_shown(|window| window.set_hold_progress(0.0));
    }
}
