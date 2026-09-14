//! Press-and-hold to dismiss, as a pure state machine: callers pass `now`,
//! nothing here reads the clock. This is the shape catnap's `overlay.rs` needs.

use std::time::{Duration, Instant};

/// How long the dismiss button must be held.
pub const HOLD_TO_DISMISS: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Holding { since: Instant },
    Dismissed,
}

#[derive(Debug, Clone, Copy)]
pub struct HoldToDismiss {
    hold: Duration,
    state: State,
}

impl HoldToDismiss {
    pub fn new(hold: Duration) -> Self {
        assert!(hold > Duration::ZERO, "hold duration must be positive");
        Self { hold, state: State::Idle }
    }

    /// Pointer went down on the button. Ignored while already holding (the
    /// original start counts) and once dismissed.
    pub fn press(&mut self, now: Instant) {
        if self.state == State::Idle {
            self.state = State::Holding { since: now };
        }
        debug_assert!(self.state != State::Idle);
    }

    /// Pointer released or cancelled: the progress is lost, not paused.
    pub fn release(&mut self) {
        if matches!(self.state, State::Holding { .. }) {
            self.state = State::Idle;
        }
        debug_assert!(!matches!(self.state, State::Holding { .. }));
    }

    /// Advances time. Returns `true` exactly once: when the hold completes.
    pub fn tick(&mut self, now: Instant) -> bool {
        let State::Holding { since } = self.state else { return false };
        if now.saturating_duration_since(since) < self.hold {
            return false;
        }
        self.state = State::Dismissed;
        true
    }

    /// Fill of the button, from 0.0 to 1.0.
    pub fn progress(&self, now: Instant) -> f32 {
        let progress = match self.state {
            State::Idle => 0.0,
            State::Holding { since } => {
                now.saturating_duration_since(since).as_secs_f32() / self.hold.as_secs_f32()
            }
            State::Dismissed => 1.0,
        };
        let progress = progress.min(1.0);
        debug_assert!((0.0..=1.0).contains(&progress));
        progress
    }

    pub fn is_dismissed(&self) -> bool {
        self.state == State::Dismissed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOLD: Duration = Duration::from_secs(5);

    fn secs(seconds: f32) -> Duration {
        Duration::from_secs_f32(seconds)
    }

    #[test]
    fn short_press_does_not_dismiss_and_resets() {
        let t0 = Instant::now();
        let mut hold = HoldToDismiss::new(HOLD);
        hold.press(t0);
        assert!((hold.progress(t0 + secs(2.5)) - 0.5).abs() < 1e-3);
        assert!(!hold.tick(t0 + secs(4.9)));
        hold.release();
        assert!(hold.progress(t0 + secs(4.9)) == 0.0);
        assert!(!hold.tick(t0 + secs(10.0)));
        assert!(!hold.is_dismissed());
    }

    #[test]
    fn full_hold_dismisses_exactly_once() {
        let t0 = Instant::now();
        let mut hold = HoldToDismiss::new(HOLD);
        hold.press(t0);
        assert!(hold.tick(t0 + HOLD));
        assert!(hold.is_dismissed());
        assert!(!hold.tick(t0 + secs(6.0)));
        assert!(hold.progress(t0 + secs(6.0)) == 1.0);
    }

    #[test]
    fn progress_does_not_accumulate_across_presses() {
        let t0 = Instant::now();
        let mut hold = HoldToDismiss::new(HOLD);
        hold.press(t0);
        hold.release();
        hold.press(t0 + secs(3.0));
        assert!(!hold.tick(t0 + secs(7.0)), "only 4 s into the second press");
        assert!(hold.tick(t0 + secs(8.0)));
    }

    #[test]
    fn repeated_press_keeps_the_original_start() {
        let t0 = Instant::now();
        let mut hold = HoldToDismiss::new(HOLD);
        hold.press(t0);
        hold.press(t0 + secs(4.0));
        assert!(hold.tick(t0 + HOLD));
    }

    #[test]
    fn press_and_release_after_dismiss_are_ignored() {
        let t0 = Instant::now();
        let mut hold = HoldToDismiss::new(HOLD);
        hold.press(t0);
        assert!(hold.tick(t0 + HOLD));
        hold.press(t0 + secs(6.0));
        hold.release();
        assert!(hold.is_dismissed());
    }

    #[test]
    fn clock_going_backwards_is_harmless() {
        let t0 = Instant::now() + secs(10.0);
        let mut hold = HoldToDismiss::new(HOLD);
        hold.press(t0);
        assert!(hold.progress(t0 - secs(1.0)) == 0.0);
        assert!(!hold.tick(t0 - secs(1.0)));
    }

    #[test]
    #[should_panic(expected = "hold duration must be positive")]
    fn zero_hold_is_rejected() {
        let _ = HoldToDismiss::new(Duration::ZERO);
    }
}
