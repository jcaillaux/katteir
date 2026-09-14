//! Throwaway spike: play a stacked-alpha AV1 clip (entry once, then loop)
//! into an OpenGL texture shown by Slint, and print timing stats at exit.
//!
//! Usage: av1-video-spike <entry.ivf> <loop.ivf> <run_secs> <decoder_threads> [snapshot.pam]
//! `-` as the entry clip runs the no-video baseline. The optional snapshot is
//! the rendered frame read back with glReadPixels at 6 s.
//! Switches: `SPIKE_SEE_THROUGH=1` transparent and frameless, `SPIKE_ON_TOP=1`
//! always on top and frameless (not on Wayland), `SPIKE_FULLSCREEN=1`
//! borderless fullscreen. A button over the video quits after a 5 s hold.

mod gl_video;
mod hold;
mod ivf;
mod readback;
mod video;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use slint::ComponentHandle;

slint::include_modules!();

/// When the optional snapshot is taken: mid-entry clip.
const SNAPSHOT_AT_SECS: u64 = 6;
const STATS_EVERY_TICKS: u64 = 30;

struct Args {
    entry: PathBuf,
    looped: PathBuf,
    run_secs: u64,
    decoder_threads: u32,
    snapshot: Option<PathBuf>,
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 || args.len() > 6 {
        return Err("usage: av1-video-spike <entry.ivf> <loop.ivf> <run_secs> <decoder_threads> [snapshot.pam]".into());
    }
    Ok(Args {
        entry: PathBuf::from(&args[1]),
        looped: PathBuf::from(&args[2]),
        run_secs: args[3].parse()?,
        decoder_threads: args[4].parse()?,
        snapshot: args.get(5).map(PathBuf::from),
    })
}

/// How often the hold progress is refreshed while the button is held.
const HOLD_TICK: Duration = Duration::from_millis(33);

fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| value == "1")
}

/// Applies the SPIKE_* switches and wires the hold-to-dismiss button.
fn configure_window(window: &SpikeWindow) -> Rc<RefCell<hold::HoldToDismiss>> {
    let on_top = env_flag("SPIKE_ON_TOP");
    window.set_see_through(env_flag("SPIKE_SEE_THROUGH"));
    window.set_keep_on_top(on_top);
    window.set_frameless(on_top);
    if env_flag("SPIKE_FULLSCREEN") {
        window.window().set_fullscreen(true);
    }
    install_hold_to_dismiss(window)
}

/// The dismiss button: press and hold for `hold::HOLD_TO_DISMISS` to quit.
/// `hold.rs` owns the logic; this only feeds it the clock and the pointer.
fn install_hold_to_dismiss(window: &SpikeWindow) -> Rc<RefCell<hold::HoldToDismiss>> {
    let hold = Rc::new(RefCell::new(hold::HoldToDismiss::new(hold::HOLD_TO_DISMISS)));
    let ticker = Rc::new(slint::Timer::default());
    let weak = window.as_weak();
    window.on_hold_pressed({
        let (hold, ticker, weak) = (hold.clone(), ticker.clone(), weak.clone());
        move || {
            hold.borrow_mut().press(Instant::now());
            let (hold, weak) = (hold.clone(), weak.clone());
            ticker.start(slint::TimerMode::Repeated, HOLD_TICK, move || {
                let Some(window) = weak.upgrade() else { return };
                let now = Instant::now();
                let mut hold = hold.borrow_mut();
                if hold.tick(now) {
                    println!("dismiss: hold completed");
                    slint::quit_event_loop().expect("event loop running");
                }
                window.set_hold_progress(hold.progress(now));
            });
        }
    });
    window.on_hold_released({
        let (hold, weak) = (hold.clone(), weak.clone());
        move || {
            hold.borrow_mut().release();
            ticker.stop();
            if let Some(window) = weak.upgrade() {
                window.set_hold_progress(0.0);
            }
        }
    });
    hold
}

#[derive(Debug, Default)]
struct UiStats {
    ticks: u64,
    shown: u64,
    late_ticks: u64,
    replaced_before_render: u64,
    gl_frames: u64,
    gl_time: Duration,
}

/// Frame handed from the timer (UI thread) to the rendering notifier.
type PendingFrame = Rc<RefCell<Option<dav1d::Picture>>>;

/// Where to write the readback snapshot, and the flag that asks for one.
struct Capture {
    requested: Rc<Cell<bool>>,
    path: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    if args.entry.as_os_str() == "-" {
        return run_baseline(args.run_secs);
    }
    let entry = video::Clip::load(&args.entry)?;
    let looped = video::Clip::load(&args.looped)?;
    let stacked = (u32::from(entry.index.width_px), u32::from(entry.index.height_px));
    assert_eq!(stacked, (u32::from(looped.index.width_px), u32::from(looped.index.height_px)));
    let base_fps = entry.fps().max(looped.fps());

    slint::BackendSelector::new().require_opengl_es().select()?;
    let window = SpikeWindow::new()?;
    let hold = configure_window(&window);
    let pending: PendingFrame = Rc::new(RefCell::new(None));
    let stats = Rc::new(RefCell::new(UiStats::default()));
    let capture = Capture { requested: Rc::new(Cell::new(false)), path: args.snapshot };
    let snapshot_timer = schedule_capture(&capture);
    install_renderer(&window, stacked, pending.clone(), stats.clone(), capture)?;

    let (frames, decode_thread) = video::spawn(entry, looped, base_fps, args.decoder_threads)?;
    let tick_timer = start_ticks(&window, frames, base_fps, pending, stats.clone());
    let quit_timer = slint::Timer::default();
    quit_timer.start(slint::TimerMode::SingleShot, Duration::from_secs(args.run_secs), || {
        slint::quit_event_loop().expect("event loop running");
    });

    let started = Instant::now();
    window.run()?;
    let wall = started.elapsed();
    drop(snapshot_timer);
    drop(tick_timer); // drops the receiver, which stops the decoder thread
    let decode = decode_thread.join().expect("decoder thread panicked")?;
    report(&stats.borrow(), &decode, wall);
    println!("dismissed by hold: {}", hold.borrow().is_dismissed());
    Ok(())
}

/// Same window without any video: what Slint and the GL driver cost on their
/// own when redrawing at 30 fps. Selected with `-` as the entry clip.
fn run_baseline(run_secs: u64) -> Result<(), Box<dyn Error>> {
    slint::BackendSelector::new().require_opengl_es().select()?;
    let window = SpikeWindow::new()?;
    let hold = configure_window(&window);
    window.set_stats("baseline: no video, redraw at 30 fps".into());
    let weak = window.as_weak();
    let redraw_timer = slint::Timer::default();
    redraw_timer.start(slint::TimerMode::Repeated, Duration::from_secs_f64(1.0 / 30.0), move || {
        if let Some(window) = weak.upgrade() {
            window.window().request_redraw();
        }
    });
    let quit_timer = slint::Timer::default();
    quit_timer.start(slint::TimerMode::SingleShot, Duration::from_secs(run_secs), || {
        slint::quit_event_loop().expect("event loop running");
    });
    window.run()?;
    println!("dismissed by hold: {}", hold.borrow().is_dismissed());
    Ok(())
}

/// Asks for a readback at SNAPSHOT_AT_SECS. The next video frame triggers the
/// render that gets captured.
fn schedule_capture(capture: &Capture) -> Option<slint::Timer> {
    capture.path.as_ref()?;
    let requested = capture.requested.clone();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::SingleShot, Duration::from_secs(SNAPSHOT_AT_SECS), move || {
        requested.set(true);
    });
    Some(timer)
}

fn install_renderer(
    window: &SpikeWindow,
    stacked: (u32, u32),
    pending: PendingFrame,
    stats: Rc<RefCell<UiStats>>,
    capture: Capture,
) -> Result<(), slint::SetRenderingNotifierError> {
    let mut renderer: Option<gl_video::GlVideo> = None;
    let weak = window.as_weak();
    window.window().set_rendering_notifier(move |state, graphics_api| match state {
        slint::RenderingState::RenderingSetup => {
            let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api else {
                panic!("spike needs the OpenGL renderer");
            };
            // SAFETY: Slint made its GL context current before calling us, and
            // get_proc_address resolves symbols for exactly that context.
            let gl = unsafe { glow::Context::from_loader_function_cstr(|s| get_proc_address(s)) };
            renderer = Some(gl_video::GlVideo::new(gl, stacked.0, stacked.1).expect("GL setup"));
        }
        slint::RenderingState::BeforeRendering => {
            let (Some(renderer), Some(window)) = (renderer.as_mut(), weak.upgrade()) else {
                return;
            };
            let Some(picture) = pending.borrow_mut().take() else { return };
            let started = Instant::now();
            let image = renderer.draw_frame(&picture);
            let mut stats = stats.borrow_mut();
            stats.gl_time += started.elapsed();
            stats.gl_frames += 1;
            window.set_frame(image);
        }
        slint::RenderingState::AfterRendering => {
            if !capture.requested.replace(false) {
                return;
            }
            let (Some(renderer), Some(window), Some(path)) = (renderer.as_ref(), weak.upgrade(), capture.path.as_ref())
            else {
                return;
            };
            let size = window.window().size();
            let pixels = readback::read_back(renderer.gl(), size.width, size.height);
            readback::write_pam_flipped(path, size.width, size.height, &pixels).expect("write snapshot");
        }
        slint::RenderingState::RenderingTeardown => drop(renderer.take()),
        _ => {}
    })
}

fn start_ticks(
    window: &SpikeWindow,
    frames: Receiver<video::Frame>,
    base_fps: u32,
    pending: PendingFrame,
    stats: Rc<RefCell<UiStats>>,
) -> slint::Timer {
    let weak = window.as_weak();
    let mut hold_remaining: u32 = 0;
    let timer = slint::Timer::default();
    let interval = Duration::from_secs_f64(1.0 / f64::from(base_fps));
    timer.start(slint::TimerMode::Repeated, interval, move || {
        let Some(window) = weak.upgrade() else { return };
        let mut stats = stats.borrow_mut();
        stats.ticks += 1;
        if stats.ticks.is_multiple_of(STATS_EVERY_TICKS) {
            window.set_stats(stats_line(&stats).into());
        }
        if hold_remaining > 0 {
            hold_remaining -= 1;
            return;
        }
        match frames.try_recv() {
            Ok(frame) => {
                assert!(frame.hold_ticks >= 1);
                hold_remaining = frame.hold_ticks - 1;
                if pending.borrow_mut().replace(frame.picture).is_some() {
                    stats.replaced_before_render += 1;
                }
                stats.shown += 1;
                window.window().request_redraw();
            }
            Err(TryRecvError::Empty) => stats.late_ticks += 1,
            Err(TryRecvError::Disconnected) => slint::quit_event_loop().expect("event loop running"),
        }
    });
    timer
}

fn stats_line(stats: &UiStats) -> String {
    let gl_ms = if stats.gl_frames == 0 {
        0.0
    } else {
        stats.gl_time.as_secs_f64() * 1000.0 / stats.gl_frames as f64
    };
    format!(
        "shown {}  late ticks {}  replaced {}  GL upload+draw {:.2} ms/frame",
        stats.shown, stats.late_ticks, stats.replaced_before_render, gl_ms
    )
}

fn report(stats: &UiStats, decode: &video::DecodeStats, wall: Duration) {
    let decode_ms = if decode.frames == 0 {
        0.0
    } else {
        decode.decode_time.as_secs_f64() * 1000.0 / decode.frames as f64
    };
    println!("wall {:.2} s, ticks {}", wall.as_secs_f64(), stats.ticks);
    println!("{}", stats_line(stats));
    println!("rendered frames {}", stats.gl_frames);
    println!("decoded {} frames, dav1d {:.2} ms/frame (wall time inside decode calls)", decode.frames, decode_ms);
}
