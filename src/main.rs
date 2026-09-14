//! catnap: every N minutes of work, a cat takes over the screen for a break.
//! Wiring only (CLAUDE.md §3): load the config, show the settings window,
//! drive the timer. The cat window arrives in M1.

mod config;
mod limits;
mod timer;
mod video;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Context;
use slint::ComponentHandle;

use crate::config::{Config, DisplayMode};
use crate::timer::{Event, State, Timer, TimerSettings};

slint::include_modules!();

/// How often the timer is polled; its deadlines don't depend on this.
const TICK: Duration = Duration::from_millis(250);

struct App {
    config: Config,
    config_path: PathBuf,
    timer: Timer,
}

type Shared = Rc<RefCell<App>>;

fn main() -> anyhow::Result<()> {
    // Our own messages at info, dependencies (zbus, winit...) only from warn.
    // RUST_LOG overrides this.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("catnap=info,warn")).init();
    slint::BackendSelector::new().require_opengl_es().select().context("selecting the OpenGL ES renderer")?;
    let config_path = config::config_path()?;
    let (config, notice) = load_config(&config_path);

    let ui = SettingsWindow::new()?;
    set_limits(&ui);
    show_config(&ui, &config);
    ui.set_notice_is_warning(!notice.is_empty());
    ui.set_notice(notice.into());
    ui.set_config_path(config_path.display().to_string().into());

    let timer = Timer::new(TimerSettings::from(&config.timer));
    let app: Shared = Rc::new(RefCell::new(App { config, config_path, timer }));
    wire_timer_buttons(&ui, &app);
    wire_settings(&ui, &app);
    let _ticker = start_ticker(&ui, &app);
    refresh_status(&ui, &app.borrow().timer, Instant::now());

    ui.run()?;
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
    ui.set_min_break_range(ui_range(&limits::MIN_BREAK_SECS));
    ui.set_hold_range(ui_range(&limits::DISMISS_HOLD_SECS));
}

fn show_config(ui: &SettingsWindow, config: &Config) {
    ui.set_work_minutes(to_ui(config.timer.work_minutes));
    ui.set_warn_before_secs(to_ui(config.timer.warn_before_secs));
    ui.set_min_break_secs(to_ui(config.timer.min_break_secs));
    ui.set_dismiss_hold_secs(to_ui(config.display.dismiss_hold_secs));
    ui.set_display_mode(match config.display.mode {
        DisplayMode::Fullscreen => 0,
        DisplayMode::Overlay => 1,
    });
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
    config.timer.min_break_secs = from_ui(ui.get_min_break_secs());
    config.display.dismiss_hold_secs = from_ui(ui.get_dismiss_hold_secs());
    config.display.mode = if ui.get_display_mode() == 1 { DisplayMode::Overlay } else { DisplayMode::Fullscreen };
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
            format!("AV1 clip, {}×{}, {} frames at {:.0} fps", clip.width_px, clip.picture_height_px, clip.frames, clip.fps),
            1,
        ),
        Err(error) => (error.to_string(), 2),
    }
}

fn wire_timer_buttons(ui: &SettingsWindow, app: &Shared) {
    let on_timer = |ui: &SettingsWindow, action: fn(&mut Timer, Instant)| {
        let (weak, app) = (ui.as_weak(), app.clone());
        move || {
            let now = Instant::now();
            let mut app = app.borrow_mut();
            action(&mut app.timer, now);
            if let Some(ui) = weak.upgrade() {
                refresh_status(&ui, &app.timer, now);
            }
        }
    };
    ui.on_start(on_timer(ui, Timer::start));
    ui.on_pause(on_timer(ui, Timer::pause));
    ui.on_stop(on_timer(ui, |timer, _| timer.stop()));
    let (weak, app) = (ui.as_weak(), app.clone());
    ui.on_dismiss_break(move || {
        let Some(ui) = weak.upgrade() else { return };
        let now = Instant::now();
        let mut app = app.borrow_mut();
        match app.timer.dismiss(now) {
            Ok(event) => handle_event(&ui, event),
            Err(error) => {
                ui.set_notice(error.to_string().into());
                ui.set_notice_is_warning(false);
            }
        }
        refresh_status(&ui, &app.timer, now);
    });
}

fn wire_settings(ui: &SettingsWindow, app: &Shared) {
    let weak = ui.as_weak();
    ui.on_clip_edited(move |which, text| {
        if let Some(ui) = weak.upgrade() {
            show_clip_status(&ui, which, &text);
        }
    });
    let (weak, app) = (ui.as_weak(), app.clone());
    ui.on_save(move || {
        let Some(ui) = weak.upgrade() else { return };
        let mut app = app.borrow_mut();
        let mut config = read_config(&ui, &app.config);
        let adjusted = config.sanitise();
        let (notice, is_warning) = match config::save_file(&app.config_path, &config) {
            Ok(()) => saved_notice(&adjusted, &clip_warnings(&config)),
            Err(error) => (format!("Not saved: {error}"), true),
        };
        log::info!("{notice}");
        app.timer.set_settings(TimerSettings::from(&config.timer));
        show_config(&ui, &config);
        app.config = config;
        ui.set_notice(notice.into());
        ui.set_notice_is_warning(is_warning);
    });
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

fn start_ticker(ui: &SettingsWindow, app: &Shared) -> slint::Timer {
    let (weak, app) = (ui.as_weak(), app.clone());
    let ticker = slint::Timer::default();
    ticker.start(slint::TimerMode::Repeated, TICK, move || {
        let Some(ui) = weak.upgrade() else { return };
        let now = Instant::now();
        let mut app = app.borrow_mut();
        if let Some(event) = app.timer.tick(now) {
            handle_event(&ui, event);
        }
        refresh_status(&ui, &app.timer, now);
    });
    ticker
}

/// Until the cat window (M1) and notifications (M2) exist, events are logged
/// and shown as a notice.
fn handle_event(ui: &SettingsWindow, event: Event) {
    let notice = match event {
        Event::NotifySoon { secs_left } => format!("Break in {secs_left} s."),
        Event::BreakStarted => "Break time! (The cat window arrives in M1.)".to_owned(),
        Event::BreakEnded => "Back to work.".to_owned(),
    };
    log::info!("{event:?}: {notice}");
    ui.set_notice(notice.into());
    ui.set_notice_is_warning(false);
}

fn refresh_status(ui: &SettingsWindow, timer: &Timer, now: Instant) {
    let left = timer.time_left(now).map(minutes_seconds).unwrap_or_default();
    let status = match timer.state() {
        State::Idle => "Idle".to_owned(),
        State::Working { .. } => format!("Working: break in {left}"),
        State::Paused { .. } => format!("Paused: {left} left"),
        State::Break { .. } => match timer::whole_secs_up(timer.dismissable_in(now)) {
            0 => "Break: the cat can be dismissed".to_owned(),
            wait => format!("Break: can be dismissed in {wait} s"),
        },
    };
    let state = timer.state();
    ui.set_status(status.into());
    ui.set_can_start(matches!(state, State::Idle | State::Paused { .. }));
    ui.set_can_pause(matches!(state, State::Working { .. }));
    ui.set_can_stop(!matches!(state, State::Idle));
    ui.set_can_dismiss(matches!(state, State::Break { .. }) && timer.dismissable_in(now).is_zero());
}

/// "mm:ss", rounded up so a fresh 25-minute period shows 25:00.
fn minutes_seconds(duration: Duration) -> String {
    let secs = timer::whole_secs_up(duration);
    format!("{}:{:02}", secs / 60, secs % 60)
}
