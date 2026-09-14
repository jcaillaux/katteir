//! Work/break state machine (CLAUDE.md §5). Pure: every method that needs
//! the time takes `now`; nothing here reads the clock or allocates.
//!
//! ```text
//! Idle ──start──▶ Working ──deadline──▶ Break ──dismiss (after min_break)──▶ Working
//!                  │    ▲                  │
//!             pause│    │start (resume)    │
//!                  ▼    │                  │
//!                 Paused                   │
//! any state ──stop──▶ Idle ◀───────────────┘
//! ```

use std::time::{Duration, Instant};

use crate::config::TimerConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerSettings {
    pub work: Duration,
    /// Zero turns the warning off.
    pub warn_before: Duration,
    pub min_break: Duration,
}

impl From<&TimerConfig> for TimerSettings {
    fn from(config: &TimerConfig) -> Self {
        Self {
            work: Duration::from_secs(u64::from(config.work_minutes) * 60),
            warn_before: Duration::from_secs(u64::from(config.warn_before_secs)),
            min_break: Duration::from_secs(u64::from(config.min_break_secs)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Working { deadline: Instant, warned: bool },
    Paused { remaining: Duration, warned: bool },
    Break { started: Instant },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    NotifySoon { secs_left: u32 },
    BreakStarted,
    BreakEnded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimerError {
    #[error("the break can be dismissed in {secs_left} s")]
    TooEarly { secs_left: u32 },
    #[error("there is no break to dismiss")]
    NotOnBreak,
}

#[derive(Debug, Clone)]
pub struct Timer {
    settings: TimerSettings,
    state: State,
}

impl Timer {
    pub fn new(settings: TimerSettings) -> Self {
        assert!(settings.work > Duration::ZERO, "work duration must be positive");
        Self { settings, state: State::Idle }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// New settings apply from the next work period; a running one keeps its
    /// deadline.
    pub fn set_settings(&mut self, settings: TimerSettings) {
        assert!(settings.work > Duration::ZERO, "work duration must be positive");
        self.settings = settings;
    }

    /// Starts working from Idle, or resumes from Paused. Ignored otherwise.
    pub fn start(&mut self, now: Instant) {
        self.state = match self.state {
            State::Idle => State::Working { deadline: now + self.settings.work, warned: false },
            State::Paused { remaining, warned } => State::Working { deadline: now + remaining, warned },
            other => other,
        };
        debug_assert!(!matches!(self.state, State::Idle | State::Paused { .. }));
    }

    /// Pauses a work period, keeping what's left. Ignored otherwise.
    pub fn pause(&mut self, now: Instant) {
        if let State::Working { deadline, warned } = self.state {
            self.state = State::Paused { remaining: deadline.saturating_duration_since(now), warned };
        }
        debug_assert!(!matches!(self.state, State::Working { .. }));
    }

    pub fn stop(&mut self) {
        self.state = State::Idle;
    }

    /// Ends a break once `min_break` has passed, and starts the next work period.
    pub fn dismiss(&mut self, now: Instant) -> Result<Event, TimerError> {
        let State::Break { .. } = self.state else {
            return Err(TimerError::NotOnBreak);
        };
        let wait = self.dismissable_in(now);
        if wait > Duration::ZERO {
            return Err(TimerError::TooEarly { secs_left: whole_secs_up(wait) });
        }
        self.state = State::Working { deadline: now + self.settings.work, warned: false };
        Ok(Event::BreakEnded)
    }

    /// Advances time: at most one event per call. If `now` jumped past both
    /// the warning and the deadline (e.g. after a suspend), only the break
    /// starts.
    pub fn tick(&mut self, now: Instant) -> Option<Event> {
        let State::Working { deadline, warned } = self.state else {
            return None;
        };
        if now >= deadline {
            self.state = State::Break { started: now };
            return Some(Event::BreakStarted);
        }
        let left = deadline - now;
        if warned || self.settings.warn_before.is_zero() || left > self.settings.warn_before {
            return None;
        }
        self.state = State::Working { deadline, warned: true };
        Some(Event::NotifySoon { secs_left: whole_secs_up(left) })
    }

    /// Time until the break while working or paused.
    pub fn time_left(&self, now: Instant) -> Option<Duration> {
        match self.state {
            State::Working { deadline, .. } => Some(deadline.saturating_duration_since(now)),
            State::Paused { remaining, .. } => Some(remaining),
            State::Idle | State::Break { .. } => None,
        }
    }

    /// How long until a break may be dismissed; zero when it may (or when
    /// not on a break).
    pub fn dismissable_in(&self, now: Instant) -> Duration {
        match self.state {
            State::Break { started } => self.settings.min_break.saturating_sub(now.saturating_duration_since(started)),
            _ => Duration::ZERO,
        }
    }
}

/// Whole seconds, rounded up, saturating at `u32::MAX`.
pub fn whole_secs_up(duration: Duration) -> u32 {
    let secs = duration.as_secs() + u64::from(duration.subsec_nanos() > 0);
    u32::try_from(secs).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK_SECS: u64 = 25 * 60;
    const WARN_SECS: u64 = 60;
    const MIN_BREAK_SECS: u64 = 30;

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    /// `t0` plus whole seconds. Tests only move time forward from `t0`, so
    /// they never subtract from an `Instant`.
    fn at(t0: Instant, seconds: u64) -> Instant {
        t0 + secs(seconds)
    }

    fn settings(warn_secs: u64) -> TimerSettings {
        TimerSettings { work: secs(WORK_SECS), warn_before: secs(warn_secs), min_break: secs(MIN_BREAK_SECS) }
    }

    fn timer() -> Timer {
        Timer::new(settings(WARN_SECS))
    }

    #[test]
    fn idle_does_nothing_until_started() {
        let t0 = Instant::now();
        let mut timer = timer();
        assert_eq!(timer.tick(at(t0, 10_000)), None);
        assert_eq!(timer.state(), State::Idle);
        assert_eq!(timer.time_left(t0), None);
    }

    #[test]
    fn full_cycle_warns_once_then_breaks() {
        let t0 = Instant::now();
        let mut timer = timer();
        timer.start(t0);
        assert_eq!(timer.time_left(t0), Some(secs(WORK_SECS)));
        assert_eq!(timer.tick(at(t0, WORK_SECS - WARN_SECS - 1)), None);
        assert_eq!(timer.tick(at(t0, WORK_SECS - WARN_SECS)), Some(Event::NotifySoon { secs_left: 60 }));
        assert_eq!(timer.tick(at(t0, WORK_SECS - 10)), None, "warns only once");
        assert_eq!(timer.tick(at(t0, WORK_SECS)), Some(Event::BreakStarted));
        assert_eq!(timer.state(), State::Break { started: at(t0, WORK_SECS) });
        assert_eq!(timer.tick(at(t0, WORK_SECS + 5)), None);
    }

    #[test]
    fn zero_warning_never_notifies() {
        let t0 = Instant::now();
        let mut timer = Timer::new(settings(0));
        timer.start(t0);
        assert_eq!(timer.tick(at(t0, WORK_SECS - 1)), None);
        assert_eq!(timer.tick(at(t0, WORK_SECS)), Some(Event::BreakStarted));
    }

    #[test]
    fn jumping_past_the_deadline_skips_the_warning() {
        let t0 = Instant::now();
        let mut timer = timer();
        timer.start(t0);
        assert_eq!(timer.tick(at(t0, WORK_SECS + 3600)), Some(Event::BreakStarted));
    }

    #[test]
    fn pause_keeps_the_remaining_time() {
        let t0 = Instant::now();
        let mut timer = timer();
        timer.start(t0);
        timer.pause(at(t0, 600));
        assert_eq!(timer.time_left(at(t0, 5000)), Some(secs(WORK_SECS - 600)));
        assert_eq!(timer.tick(at(t0, 5000)), None, "paused time doesn't count");
        timer.start(at(t0, 5000));
        assert_eq!(timer.tick(at(t0, 5000 + WORK_SECS - 600)), Some(Event::BreakStarted));
    }

    #[test]
    fn dismiss_waits_for_the_minimum_break() {
        let t0 = Instant::now();
        let mut timer = timer();
        timer.start(t0);
        assert_eq!(timer.tick(at(t0, WORK_SECS)), Some(Event::BreakStarted));
        let early = at(t0, WORK_SECS + 10);
        assert_eq!(timer.dismissable_in(early), secs(20));
        assert_eq!(timer.dismiss(early), Err(TimerError::TooEarly { secs_left: 20 }));
        let on_time = at(t0, WORK_SECS + MIN_BREAK_SECS);
        assert_eq!(timer.dismiss(on_time), Ok(Event::BreakEnded));
        assert_eq!(timer.time_left(on_time), Some(secs(WORK_SECS)), "the next work period starts");
    }

    #[test]
    fn dismiss_outside_a_break_is_an_error() {
        let t0 = Instant::now();
        let mut timer = timer();
        assert_eq!(timer.dismiss(t0), Err(TimerError::NotOnBreak));
        timer.start(t0);
        assert_eq!(timer.dismiss(t0), Err(TimerError::NotOnBreak));
    }

    #[test]
    fn stop_returns_to_idle_from_any_state() {
        let t0 = Instant::now();
        let mut timer = timer();
        timer.start(t0);
        timer.stop();
        assert_eq!(timer.state(), State::Idle);
        timer.start(t0);
        timer.pause(at(t0, 1));
        timer.stop();
        assert_eq!(timer.state(), State::Idle);
        timer.start(t0);
        assert_eq!(timer.tick(at(t0, WORK_SECS)), Some(Event::BreakStarted));
        timer.stop();
        assert_eq!(timer.state(), State::Idle);
    }

    #[test]
    fn start_while_working_or_on_break_changes_nothing() {
        let t0 = Instant::now();
        let mut timer = timer();
        timer.start(t0);
        timer.start(at(t0, 100));
        assert_eq!(timer.time_left(at(t0, 100)), Some(secs(WORK_SECS - 100)));
    }

    #[test]
    fn new_settings_apply_to_the_next_period() {
        let t0 = Instant::now();
        let mut timer = timer();
        timer.start(t0);
        timer.set_settings(TimerSettings { work: secs(60), warn_before: Duration::ZERO, min_break: Duration::ZERO });
        assert_eq!(timer.time_left(t0), Some(secs(WORK_SECS)), "running period keeps its deadline");
        assert_eq!(timer.tick(at(t0, WORK_SECS)), Some(Event::BreakStarted));
        assert_eq!(timer.dismiss(at(t0, WORK_SECS)), Ok(Event::BreakEnded));
        assert_eq!(timer.time_left(at(t0, WORK_SECS)), Some(secs(60)));
    }

    #[test]
    fn whole_secs_rounds_up() {
        assert_eq!(whole_secs_up(Duration::from_millis(1)), 1);
        assert_eq!(whole_secs_up(secs(60)), 60);
        assert_eq!(whole_secs_up(Duration::from_millis(59_001)), 60);
    }

    #[test]
    #[should_panic(expected = "work duration must be positive")]
    fn zero_work_is_rejected() {
        let _ = Timer::new(TimerSettings { work: Duration::ZERO, warn_before: secs(WARN_SECS), min_break: secs(MIN_BREAK_SECS) });
    }
}
