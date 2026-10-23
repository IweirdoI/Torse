//! Fixed-step simulation clock.
//!
//! Gameplay reads the tick, never the wall clock. The clock reports how long a
//! tick took so the host can measure load, but that measurement never decides
//! gameplay: an overrun is recorded and reported, not silently compensated by
//! enlarging the step or skipping ticks.

use torse_types::Tick;

/// Result of finishing one tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TickReport {
    /// Tick that was just executed.
    pub tick: Tick,
    /// Measured duration reported by the host.
    pub elapsed_ms: u64,
    /// Amount by which the duration exceeded the budget, or zero.
    pub overrun_ms: u64,
    /// Number of overruns observed since the clock was created.
    pub overruns: u64,
}

/// Fixed-step clock with overload reporting.
#[derive(Clone, Copy, Debug)]
pub struct TickClock {
    current: Tick,
    budget_ms: u64,
    overruns: u64,
    last_elapsed_ms: u64,
}

impl TickClock {
    /// Create a clock whose first tick is the baseline tick zero.
    pub fn new(budget_ms: u64) -> Self {
        Self {
            current: Tick(0),
            budget_ms,
            overruns: 0,
            last_elapsed_ms: 0,
        }
    }

    /// Tick currently being simulated.
    pub const fn current(&self) -> Tick {
        self.current
    }

    /// Configured per-tick budget in milliseconds.
    pub const fn budget_ms(&self) -> u64 {
        self.budget_ms
    }

    /// Duration measured for the most recently finished tick.
    pub const fn last_elapsed_ms(&self) -> u64 {
        self.last_elapsed_ms
    }

    /// Number of finished ticks that exceeded the budget.
    pub const fn overruns(&self) -> u64 {
        self.overruns
    }

    /// Record the measured duration of the current tick.
    ///
    /// The host passes a measurement it already took; this crate never reads a
    /// clock itself, which keeps authority code deterministic and testable.
    pub fn finish(&mut self, elapsed_ms: u64) -> TickReport {
        let overrun_ms = elapsed_ms.saturating_sub(self.budget_ms);
        if overrun_ms > 0 {
            self.overruns += 1;
        }
        self.last_elapsed_ms = elapsed_ms;
        TickReport {
            tick: self.current,
            elapsed_ms,
            overrun_ms,
            overruns: self.overruns,
        }
    }

    /// Advance to the next tick.
    ///
    /// Returns `None` instead of wrapping when the tick counter is exhausted;
    /// the caller must stop the world rather than silently reusing tick numbers.
    pub fn advance(&mut self) -> Option<Tick> {
        let next = self.current.0.checked_add(1)?;
        self.current = Tick(next);
        Some(self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_advance_one_at_a_time() {
        let mut clock = TickClock::new(50);
        assert_eq!(clock.current(), Tick(0));
        assert_eq!(clock.advance(), Some(Tick(1)));
        assert_eq!(clock.advance(), Some(Tick(2)));
        assert_eq!(clock.current(), Tick(2));
    }

    #[test]
    fn overruns_are_reported_not_compensated() {
        let mut clock = TickClock::new(50);
        let report = clock.finish(20);
        assert_eq!(report.overrun_ms, 0);
        assert_eq!(report.overruns, 0);
        let report = clock.finish(80);
        assert_eq!(report.overrun_ms, 30);
        assert_eq!(report.overruns, 1);
        // A slow tick still advances exactly one tick when the host advances.
        assert_eq!(clock.advance(), Some(Tick(1)));
        assert_eq!(clock.current(), Tick(1));
    }

    #[test]
    fn exhaustion_returns_none_instead_of_wrapping() {
        let mut clock = TickClock::new(50);
        clock.current = Tick(u64::MAX);
        assert_eq!(clock.advance(), None);
        assert_eq!(clock.current(), Tick(u64::MAX));
    }
}
