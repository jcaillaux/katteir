//! The cat window (CLAUDE.md §3): shown when a break starts, hidden when it
//! ends. It owns the decoder and the hold-to-dismiss state for the break.
//! It's always an overlay: fullscreen, see-through, on top where the
//! platform allows (see `ui/cat.slint`).
//!
//! Frames are paced by drawing: a tick at the clip rate takes the next frame
//! only once the previous one was drawn. When the compositor stops drawing a
//! hidden window, decoding stops too (the decoder blocks on its full queue).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use slint::ComponentHandle;

use crate::CatWindow;
use crate::cats::CatClips;
use crate::hold::HoldToDismiss;
use crate::icon;
use crate::timer;
use crate::video::decode::Decoder;
use crate::video::gl::{self, GlVideo};

/// How often the hold progress is refreshed while the button is held.
const HOLD_TICK: Duration = Duration::from_millis(33);
/// Delay between showing the window and starting the slide-in, so the cat
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

/// State shared by the frame tick, the renderer and the hold callbacks.
#[derive(Default)]
struct Playback {
    decoder: Option<Decoder>,
    /// Decoded and waiting to be drawn.
    pending: Option<dav1d::Picture>,
    /// A frame was taken and not drawn yet: don't take the next one.
    awaiting_draw: bool,
    hold_remaining_ticks: u32,
    stacked_size_px: (u32, u32),
    hold: Option<HoldToDismiss>,
}

type Callback = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

pub struct Overlay {
    window: CatWindow,
    visible: Cell<bool>,
    playback: Rc<RefCell<Playback>>,
    frame_timer: slint::Timer,
    hold_timer: slint::Timer,
    arrive_timer: slint::Timer,
    on_dismissed: Callback,
}

impl Overlay {
    /// Creates the (hidden) cat window.
    pub fn new() -> Result<Rc<Self>, OverlayError> {
        let overlay = Rc::new(Self {
            // On the Wayland overlay layer where the compositor allows it.
            window: crate::platform::overlay_window(CatWindow::new)?,
            visible: Cell::new(false),
            playback: Rc::new(RefCell::new(Playback::default())),
            frame_timer: slint::Timer::default(),
            hold_timer: slint::Timer::default(),
            arrive_timer: slint::Timer::default(),
            on_dismissed: Rc::new(RefCell::new(None)),
        });
        overlay.window.set_window_icon(icon::window_icon());
        overlay.install_renderer()?;
        overlay.install_hold();
        Ok(overlay)
    }

    /// Called when a completed hold dismisses the break.
    pub fn on_dismissed(&self, callback: impl Fn() + 'static) {
        *self.on_dismissed.borrow_mut() = Some(Rc::new(callback));
    }

    /// Shows the window fullscreen and starts playing `clips`. `hold` is how
    /// long the dismiss button must be held.
    pub fn show(&self, clips: &CatClips, hold: Duration) -> Result<(), OverlayError> {
        self.hide();
        let decoder = Decoder::start(clips.entry.clone(), clips.looped.clone())?;
        let base_fps = decoder.base_fps();
        *self.playback.borrow_mut() = Playback {
            decoder: Some(decoder),
            stacked_size_px: clips.entry.stacked_size_px(),
            hold: Some(HoldToDismiss::new(hold)),
            ..Playback::default()
        };
        self.window.set_arrived(false);
        self.window.set_hold_progress(0.0);
        self.window.set_hold_text(format!("Hold {} s to dismiss", hold.as_secs()).into());
        self.window.window().set_fullscreen(true);
        self.window.show()?;
        self.visible.set(true);
        self.start_frames(base_fps);
        let weak = self.window.as_weak();
        self.arrive_timer.start(slint::TimerMode::SingleShot, ARRIVE_DELAY, move || {
            if let Some(window) = weak.upgrade() {
                window.set_arrived(true);
            }
        });
        Ok(())
    }

    /// Hides the window and stops the decoder. Does nothing when hidden.
    pub fn hide(&self) {
        if !self.visible.replace(false) {
            return;
        }
        self.frame_timer.stop();
        self.hold_timer.stop();
        self.arrive_timer.stop();
        let decoder = {
            let mut playback = self.playback.borrow_mut();
            playback.pending = None;
            playback.hold = None;
            playback.decoder.take()
        };
        // Joins the decoder thread, outside the borrow.
        drop(decoder);
        if let Err(error) = self.window.hide() {
            log::warn!("cannot hide the cat window: {error}");
        }
        self.window.set_arrived(false);
    }

    /// Updates the break countdown.
    pub fn set_break_left(&self, left: Duration) {
        self.window.set_countdown(timer::minutes_seconds(left).into());
    }

    fn start_frames(&self, base_fps: u32) {
        assert!(base_fps > 0);
        let (playback, weak) = (self.playback.clone(), self.window.as_weak());
        let interval = Duration::from_secs_f64(1.0 / f64::from(base_fps));
        self.frame_timer.start(slint::TimerMode::Repeated, interval, move || {
            let Some(window) = weak.upgrade() else { return };
            let mut playback = playback.borrow_mut();
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
                    playback.pending = Some(frame.picture);
                    playback.awaiting_draw = true;
                    window.window().request_redraw();
                }
                Some(Err(TryRecvError::Disconnected)) => {
                    log::warn!("the decoder stopped; the cat stays on its last frame");
                    playback.decoder = None;
                }
                Some(Err(TryRecvError::Empty)) | None => {}
            }
        });
    }

    fn install_renderer(&self) -> Result<(), slint::SetRenderingNotifierError> {
        let (playback, weak) = (self.playback.clone(), self.window.as_weak());
        let mut context: Option<Rc<glow::Context>> = None;
        let mut video: Option<GlVideo> = None;
        self.window.window().set_rendering_notifier(move |state, graphics_api| match state {
            slint::RenderingState::RenderingSetup => context = gl::context(graphics_api),
            slint::RenderingState::BeforeRendering => {
                let (Some(window), Some(context)) = (weak.upgrade(), context.as_ref()) else { return };
                let (picture, size) = {
                    let mut playback = playback.borrow_mut();
                    (playback.pending.take(), playback.stacked_size_px)
                };
                let Some(picture) = picture else { return };
                if video.as_ref().is_none_or(|video| video.stacked_size_px() != size) {
                    video = GlVideo::new(context.clone(), size.0, size.1)
                        .map_err(|error| log::error!("cannot set up the video shader: {error}"))
                        .ok();
                }
                if let Some(video) = video.as_mut() {
                    window.set_frame(video.draw_frame(&picture));
                }
                playback.borrow_mut().awaiting_draw = false;
            }
            slint::RenderingState::RenderingTeardown => {
                video = None;
                context = None;
            }
            _ => {}
        })
    }

    fn install_hold(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        self.window.on_hold_pressed(move || {
            if let Some(overlay) = this.upgrade() {
                overlay.hold_pressed();
            }
        });
        let this = Rc::downgrade(self);
        self.window.on_hold_released(move || {
            if let Some(overlay) = this.upgrade() {
                overlay.hold_released();
            }
        });
    }

    fn hold_pressed(self: &Rc<Self>) {
        {
            let mut playback = self.playback.borrow_mut();
            let Some(hold) = playback.hold.as_mut() else { return };
            hold.press(Instant::now());
        }
        let this = Rc::downgrade(self);
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
        self.window.set_hold_progress(progress);
        if done {
            // Cloned out first: the callback hides this window, which borrows
            // the playback state again.
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
        self.window.set_hold_progress(0.0);
    }
}
