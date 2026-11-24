//! Shared accounting for time spent waiting for background pipelines.
//!
//! Adapted from fxceless/victory-fork's reserved wait budget. Reservation
//! happens before sleeping, so simultaneous recording threads cannot each
//! spend the entire frame's allowance.

use std::time::Duration;

#[derive(Default)]
pub(crate) struct WaitBudget {
    frame: Option<u64>,
    spent: Duration,
}

impl WaitBudget {
    pub(crate) const fn new() -> Self {
        Self {
            frame: None,
            spent: Duration::ZERO,
        }
    }

    /// Called under the shared mutex. An older recording thread must not
    /// reset the accounting after the next frame has already started.
    pub(crate) fn reserve(&mut self, frame: u64, budget: Duration) -> Duration {
        match self.frame {
            Some(current) if frame < current => return Duration::ZERO,
            Some(current) if frame == current => {}
            _ => {
                self.frame = Some(frame);
                self.spent = Duration::ZERO;
            }
        }
        let left = budget.saturating_sub(self.spent);
        self.spent += left;
        left
    }

    /// A pipeline becoming ready early returns its unused reservation.
    /// Finishing a previous frame's wait cannot alter the current frame.
    pub(crate) fn finish(&mut self, frame: u64, reserved: Duration, elapsed: Duration) {
        if self.frame == Some(frame) {
            self.spent = self.spent.saturating_sub(reserved.saturating_sub(elapsed));
        }
    }
}

pub(crate) fn wait_budget(value: Option<&str>) -> Duration {
    Duration::from_millis(value.and_then(|v| v.trim().parse().ok()).unwrap_or(500))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Barrier, Mutex};

    #[test]
    fn concurrent_recorders_share_one_frame_allowance() {
        let state = Mutex::new(WaitBudget::new());
        let barrier = Barrier::new(8);
        let budget = Duration::from_millis(500);
        let reserved = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        let amount = state.lock().unwrap().reserve(10, budget);
                        // Keep every reservation live until every recorder has
                        // attempted to take time from the same frame.
                        barrier.wait();
                        amount
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|t| t.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(reserved.iter().copied().sum::<Duration>(), budget);
        assert_eq!(reserved.iter().filter(|v| !v.is_zero()).count(), 1);
        assert_eq!(state.lock().unwrap().reserve(10, budget), Duration::ZERO);
    }

    #[test]
    fn early_completion_refunds_only_its_own_frame() {
        let mut state = WaitBudget::new();
        let budget = Duration::from_millis(500);
        let first = state.reserve(7, budget);
        state.finish(7, first, Duration::from_millis(20));
        let second = state.reserve(7, budget);
        assert_eq!(second, Duration::from_millis(480));
        assert_eq!(state.reserve(8, budget), budget);
        state.finish(7, second, Duration::ZERO);
        assert_eq!(state.reserve(8, budget), Duration::ZERO);
        assert_eq!(state.reserve(7, budget), Duration::ZERO);
        assert_eq!(state.reserve(8, budget), Duration::ZERO);
    }

    #[test]
    fn zero_and_invalid_wait_settings_are_explicit() {
        assert_eq!(wait_budget(Some("0")), Duration::ZERO);
        assert_eq!(wait_budget(Some(" 24 ")), Duration::from_millis(24));
        for value in [
            None,
            Some("-1"),
            Some("invalid"),
            Some("18446744073709551616"),
        ] {
            assert_eq!(wait_budget(value), Duration::from_millis(500));
        }
    }
}
