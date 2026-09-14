//! catnap: every N minutes of work, a cat takes over the screen for a break.
//! Wiring only (CLAUDE.md §3): load the config, show the settings window,
//! drive the timer, and show the cat window during breaks.

mod cats;
mod config;
mod hold;
mod icon;
mod limits;
mod overlay;
mod platform;
mod timer;
mod video;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use anyhow::Context;
use slint::ComponentHandle;

use crate::cats::CatClips;
use crate::config::Config;
use crate::overlay::Overlay;
use crate::platform::{Platform, TrayAction, TrayPresence, TrayState};
use crate::timer::{Event, State, Timer, TimerSettings};

slint::include_modules!();

/// How often the timer is polled; its deadlines don't depend on this.
const TICK: Duration = Duration::from_millis(250);
/// The Wayland app id (X11 class). Docks match it to `catnap.desktop` for
/// the icon (`make install-desktop`).
const APP_ID: &str = "catnap";

struct App {
    config: Config,
    config_path: PathBuf,
    timer: Timer,
    /// The configured clips, loaded at the first break and kept until the
    /// cat settings change (CLAUDE.md §4: load a cat's clips once).
    cat_clips: Option<CatClips>,
}

/// What every callback needs: the app state, and weak handles on both
/// windows so no callback keeps a window alive.
#[derive(Clone)]
struct Ctx {
    app: Rc<RefCell<App>>,
    ui: slint::Weak<SettingsWindow>,
    overlay: Weak<Overlay>,
    platform: Rc<Platform>,
}

fn main() -> anyhow::Result<()> {
    // Our own messages at info, dependencies (winit...) only from warn.
    // RUST_LOG overrides this.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("catnap=info,warn")).init();
    let screens = platform::install_slint().context("setting up Slint")?;
    slint::set_xdg_app_id(APP_ID).context("setting the app id")?;
    let config_path = config::config_path()?;
    let (config, notice) = load_config(&config_path);

    let ui = SettingsWindow::new()?;
    let Some(platform) = Platform::start(tray_handler(ui.as_weak())) else {
        log::info!("catnap is already running: its settings window is shown instead");
        return Ok(());
    };
    let platform = Rc::new(platform);
    ui.set_window_icon(icon::window_icon());
    set_limits(&ui);
    show_config(&ui, &config);
    ui.set_notice_is_warning(!notice.is_empty());
    ui.set_notice(notice.into());
    ui.set_config_path(config_path.display().to_string().into());
    let overlay = Overlay::new(screens)?;

    let timer = Timer::new(TimerSettings::from(&config.timer));
    let app = Rc::new(RefCell::new(App { config, config_path, timer, cat_clips: None }));
    let ctx = Ctx { app, ui: ui.as_weak(), overlay: Rc::downgrade(&overlay), platform };
    wire_timer_buttons(&ui, &ctx);
    wire_settings(&ui, &ctx);
    let dismiss_ctx = ctx.clone();
    overlay.on_dismissed(move || dismiss_ctx.dismiss_break());
    let _ticker = start_ticker(&ctx);
    ctx.after_timer_change(Instant::now());

    let close_platform = Rc::clone(&ctx.platform);
    ui.window().on_close_requested(move || {
        // With a tray icon the window can be opened again, so catnap keeps
        // running; without one, a hidden window would be lost.
        if close_platform.tray_presence() == TrayPresence::Shown {
            return slint::CloseRequestResponse::HideWindow;
        }
        if let Err(error) = slint::quit_event_loop() {
            log::error!("cannot quit: {error}");
        }
        slint::CloseRequestResponse::HideWindow
    });
    ui.show()?;
    slint::run_event_loop_until_quit()?;
    Ok(())
}

/// Loads the config, falling back to the defaults. Returns a notice for the UI.
fn load_config(path: &Path) -> (Config, String) {
    match config::load_file(path) {
        Ok((config, adjusted)) => {
            for field in &adjusted {
                log::warn!("{}: {field} was out of range or unusable and has been adjusted", path.display());
            }
            let notice = if adjusted.is_empty() { String::new() } else { format!("Adjusted: {}", adjusted.join(", ")) };
            (config, notice)
        }
        Err(error) => {
            log::error!("{}: {error}; using the defaults", path.display());
            (Config::default(), format!("Config file ignored ({error}); saving will replace it."))
        }
    }
}

fn to_ui(value: u32) -> i32 {
    i32::try_from(value).expect("config values are clamped far below i32::MAX")
}

fn from_ui(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

fn ui_range(range: &std::ops::RangeInclusive<u32>) -> IntRange {
    IntRange { min: to_ui(*range.start()), max: to_ui(*range.end()) }
}

fn set_limits(ui: &SettingsWindow) {
    ui.set_work_range(ui_range(&limits::WORK_MINUTES));
    ui.set_warn_range(ui_range(&limits::WARN_BEFORE_SECS));
    ui.set_break_range(ui_range(&limits::BREAK_SECS));
    ui.set_hold_range(ui_range(&limits::DISMISS_HOLD_SECS));
}

fn show_config(ui: &SettingsWindow, config: &Config) {
    ui.set_work_minutes(to_ui(config.timer.work_minutes));
    ui.set_warn_before_secs(to_ui(config.timer.warn_before_secs));
    ui.set_break_secs(to_ui(config.timer.break_secs));
    ui.set_dismiss_hold_secs(to_ui(config.display.dismiss_hold_secs));
    let entry = path_text(config.cat.entry_clip.as_deref());
    let looped = path_text(config.cat.loop_clip.as_deref());
    show_clip_status(ui, 0, &entry);
    show_clip_status(ui, 1, &looped);
    ui.set_entry_clip(entry.into());
    ui.set_loop_clip(looped.into());
}

fn path_text(path: Option<&Path>) -> String {
    path.map(|path| path.display().to_string()).unwrap_or_default()
}

/// Builds a config from the window's fields, keeping what the window doesn't show.
fn read_config(ui: &SettingsWindow, base: &Config) -> Config {
    let mut config = base.clone();
    config.timer.work_minutes = from_ui(ui.get_work_minutes());
    config.timer.warn_before_secs = from_ui(ui.get_warn_before_secs());
    config.timer.break_secs = from_ui(ui.get_break_secs());
    config.display.dismiss_hold_secs = from_ui(ui.get_dismiss_hold_secs());
    config.cat.entry_clip = clip_from_text(&ui.get_entry_clip());
    config.cat.loop_clip = clip_from_text(&ui.get_loop_clip());
    config
}

fn clip_from_text(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

/// Checks a typed clip path and shows the result under its field.
fn show_clip_status(ui: &SettingsWindow, which: i32, text: &str) {
    let (status, kind) = clip_status(text);
    if which == 0 {
        ui.set_entry_clip_status(status.into());
        ui.set_entry_clip_status_kind(kind);
    } else {
        ui.set_loop_clip_status(status.into());
        ui.set_loop_clip_status_kind(kind);
    }
}

/// Status text and kind (0 neutral, 1 ok, 2 error) for a clip path.
fn clip_status(text: &str) -> (String, i32) {
    let Some(path) = clip_from_text(text) else {
        return ("Not set: the bundled cat is used.".to_owned(), 0);
    };
    if !config::is_usable_clip_path(&path) {
        return ("Must be an absolute path.".to_owned(), 2);
    }
    match video::probe_clip(&path) {
        Ok(clip) => (
            format!("AV1 clip, {}×{}, {} frames at {} fps", clip.width_px, clip.picture_height_px, clip.frames, clip.fps),
            1,
        ),
        Err(error) => (error.to_string(), 2),
    }
}

fn wire_timer_buttons(ui: &SettingsWindow, ctx: &Ctx) {
    let on_timer = |action: fn(&mut Timer, Instant)| {
        let ctx = ctx.clone();
        move || {
            let now = Instant::now();
            action(&mut ctx.app.borrow_mut().timer, now);
            ctx.after_timer_change(now);
        }
    };
    ui.on_start(on_timer(Timer::start));
    ui.on_pause(on_timer(Timer::pause));
    ui.on_stop(on_timer(|timer, _| timer.stop()));
}

fn wire_settings(ui: &SettingsWindow, ctx: &Ctx) {
    let weak = ui.as_weak();
    ui.on_clip_edited(move |which, text| {
        if let Some(ui) = weak.upgrade() {
            show_clip_status(&ui, which, &text);
        }
    });
    let ctx = ctx.clone();
    ui.on_save(move || ctx.save());
}

fn start_ticker(ctx: &Ctx) -> slint::Timer {
    let ctx = ctx.clone();
    let ticker = slint::Timer::default();
    ticker.start(slint::TimerMode::Repeated, TICK, move || {
        let now = Instant::now();
        let event = ctx.app.borrow_mut().timer.tick(now);
        if let Some(event) = event {
            ctx.handle_event(event);
        }
        ctx.after_timer_change(now);
    });
    ticker
}

impl Ctx {
    fn with_ui(&self, action: impl FnOnce(&SettingsWindow)) {
        if let Some(ui) = self.ui.upgrade() {
            action(&ui);
        }
    }

    fn set_notice(&self, notice: String, is_warning: bool) {
        self.with_ui(|ui| {
            ui.set_notice(notice.into());
            ui.set_notice_is_warning(is_warning);
        });
    }

    /// Refreshes the settings window, and keeps the cat window in step with
    /// the timer: its countdown during a break, hidden otherwise (Stop
    /// during a break hides the cat, for example).
    fn after_timer_change(&self, now: Instant) {
        let app = self.app.borrow();
        if let Some(overlay) = self.overlay.upgrade() {
            match (app.timer.state(), app.timer.time_left(now)) {
                (State::Break { .. }, Some(left)) => overlay.set_break_left(left),
                _ => overlay.hide(),
            }
        }
        let controls = tray_state(&app.timer, now);
        self.platform.set_tray_state(&controls);
        let hint = tray_hint(self.platform.tray_presence());
        self.with_ui(|ui| {
            refresh_status(ui, &app.timer, now, &controls);
            ui.set_tray_hint(hint);
        });
    }

    /// Ends the break early: the cat window's hold-to-dismiss button.
    fn dismiss_break(&self) {
        let now = Instant::now();
        let result = self.app.borrow_mut().timer.dismiss(now);
        match result {
            Ok(event) => self.handle_event(event),
            Err(error) => log::debug!("dismiss ignored: {error}"),
        }
        self.after_timer_change(now);
    }

    fn handle_event(&self, event: Event) {
        let (notice, is_warning) = match event {
            Event::NotifySoon { secs_left } => {
                self.platform.notify(&format!("Break in {secs_left} s"), "A cat is about to take over the screen.");
                (format!("Break in {secs_left} s."), false)
            }
            Event::BreakStarted => self.start_cat(),
            Event::BreakEnded => ("Back to work.".to_owned(), false),
        };
        log::info!("{event:?}: {notice}");
        self.set_notice(notice, is_warning);
    }

    /// Shows the cat window for a break that just started. Returns the notice
    /// and whether it's a warning.
    fn start_cat(&self) -> (String, bool) {
        let (clips, problem) = self.cat_clips();
        let Some(clips) = clips else {
            return (format!("Break time! No cat can be shown: {problem}"), true);
        };
        let hold_secs = self.app.borrow().config.display.dismiss_hold_secs;
        let Some(overlay) = self.overlay.upgrade() else {
            return ("Break time!".to_owned(), false);
        };
        match overlay.show(&clips, Duration::from_secs(u64::from(hold_secs))) {
            Ok(()) if problem.is_empty() => ("Break time!".to_owned(), false),
            Ok(()) => (format!("Break time! Showing the placeholder cat: {problem}"), true),
            Err(error) => {
                log::error!("{error}");
                (format!("Break time! The cat window failed: {error}"), true)
            }
        }
    }

    /// The cat for this break: the configured clips when usable, else the
    /// placeholder. Returns why the configured clips weren't used, if they
    /// were set. Only the configured clips are cached, so a clip on a drive
    /// that gets mounted later is picked up at the next break.
    fn cat_clips(&self) -> (Option<CatClips>, String) {
        let mut app = self.app.borrow_mut();
        if let Some(clips) = &app.cat_clips {
            return (Some(clips.clone()), String::new());
        }
        let problem = match cats::configured(&app.config) {
            Some(Ok(clips)) => {
                app.cat_clips = Some(clips.clone());
                return (Some(clips), String::new());
            }
            Some(Err(error)) => {
                log::warn!("the configured clips can't be used ({error}); showing the placeholder");
                error.to_string()
            }
            None => String::new(),
        };
        match cats::placeholder() {
            Ok(clips) => (Some(clips), problem),
            Err(error) => {
                log::error!("the placeholder cat is broken: {error}");
                (None, error.to_string())
            }
        }
    }

    fn save(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let mut app = self.app.borrow_mut();
        let mut config = read_config(&ui, &app.config);
        let adjusted = config.sanitise();
        let (notice, is_warning) = match config::save_file(&app.config_path, &config) {
            Ok(()) => saved_notice(&adjusted, &clip_warnings(&config)),
            Err(error) => (format!("Not saved: {error}"), true),
        };
        log::info!("{notice}");
        if config.cat != app.config.cat {
            app.cat_clips = None;
        }
        app.timer.set_settings(TimerSettings::from(&config.timer));
        show_config(&ui, &config);
        app.config = config;
        ui.set_notice(notice.into());
        ui.set_notice_is_warning(is_warning);
    }
}

/// Clip problems never stop a save (a clip may be on a drive that isn't
/// mounted yet); they're reported so the user knows the bundled cat will play.
fn clip_warnings(config: &Config) -> Vec<String> {
    let clips = [("entry clip", config.cat.entry_clip.as_deref()), ("loop clip", config.cat.loop_clip.as_deref())];
    let mut warnings: Vec<String> = clips
        .into_iter()
        .filter_map(|(name, path)| Some((name, video::probe_clip(path?).err()?)))
        .map(|(name, error)| format!("{name}: {error}"))
        .collect();
    if config.has_lone_clip() {
        warnings.push("only one clip is set, so the bundled cat plays".to_owned());
    }
    warnings
}

/// The Save notice, and whether it's a warning (shown in red).
fn saved_notice(adjusted: &[config::Adjusted], warnings: &[String]) -> (String, bool) {
    let mut notice = "Saved.".to_owned();
    if !adjusted.is_empty() {
        notice.push_str(" Adjusted: ");
        notice.push_str(&adjusted.join(", "));
        notice.push('.');
    }
    if !warnings.is_empty() {
        notice.push_str(" Warning: ");
        notice.push_str(&warnings.join("; "));
        notice.push('.');
    }
    (notice, !adjusted.is_empty() || !warnings.is_empty())
}

/// The settings window's status line and buttons; the buttons follow the
/// same rules as the tray menu's (`controls`).
fn refresh_status(ui: &SettingsWindow, timer: &Timer, now: Instant, controls: &TrayState) {
    let left = timer.time_left(now).map(timer::minutes_seconds).unwrap_or_default();
    let status = match timer.state() {
        State::Idle => "Idle".to_owned(),
        State::Working { .. } => format!("Working: break in {left}"),
        State::Paused { .. } => format!("Paused: {left} left"),
        State::Break { .. } => format!("Break: {left} left"),
    };
    ui.set_status(status.into());
    ui.set_can_start(controls.can_start);
    ui.set_can_pause(controls.can_pause);
    ui.set_can_stop(controls.can_stop);
}

/// The tray menu's view of the timer. It counts whole minutes, so the menu
/// changes at most once a minute.
fn tray_state(timer: &Timer, now: Instant) -> TrayState {
    let minutes = timer.time_left(now).map_or(0, |left| timer::whole_secs_up(left).div_ceil(60));
    let state = timer.state();
    let status = match state {
        State::Idle => "Idle".to_owned(),
        State::Working { .. } => format!("Working: break in {minutes} min"),
        State::Paused { .. } => format!("Paused: {minutes} min left"),
        State::Break { .. } => format!("On a break: {minutes} min left"),
    };
    TrayState {
        status,
        can_start: matches!(state, State::Idle | State::Paused { .. }),
        can_pause: matches!(state, State::Working { .. }),
        can_stop: !matches!(state, State::Idle),
    }
}

/// What the settings window says closing it does.
fn tray_hint(presence: TrayPresence) -> TrayHint {
    match presence {
        TrayPresence::Starting => TrayHint::Unknown,
        TrayPresence::Shown => TrayHint::InTray,
        TrayPresence::Absent => TrayHint::NoTray,
    }
}

/// Carries out tray menu choices. The tray calls this on its own thread, so
/// each choice is handed to the UI thread.
fn tray_handler(ui: slint::Weak<SettingsWindow>) -> impl Fn(TrayAction) + Send + 'static {
    move |action| {
        let handed_over = match action {
            TrayAction::ShowSettings => ui.upgrade_in_event_loop(|ui| {
                if let Err(error) = ui.show() {
                    log::warn!("cannot show the settings window: {error}");
                }
            }),
            TrayAction::Start => ui.upgrade_in_event_loop(|ui| ui.invoke_start()),
            TrayAction::Pause => ui.upgrade_in_event_loop(|ui| ui.invoke_pause()),
            TrayAction::Stop => ui.upgrade_in_event_loop(|ui| ui.invoke_stop()),
            TrayAction::Quit => slint::invoke_from_event_loop(|| {
                if let Err(error) = slint::quit_event_loop() {
                    log::error!("cannot quit: {error}");
                }
            }),
        };
        if let Err(error) = handed_over {
            log::warn!("tray choice {action:?} lost: {error}");
        }
    }
}
