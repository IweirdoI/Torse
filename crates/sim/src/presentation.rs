//! Fixed logic steps sampled by a variable presentation frame.
//!
//! The host measures the frame interval and passes it in. This type never
//! reads a wall clock and never advances the authoritative [`Tick`](torse_types::Tick).
//! Each step it reports has the configured duration. A long frame runs at most
//! `max_steps` steps and discards the rest, so a hitch cannot enlarge a step
//! or leave an unbounded backlog. [`PresentationClock::presentation_alpha`] is
//! the fraction of the next step already elapsed, for read-only interpolation.
//! Drawing does not go through this type.

/// Accumulates frame time into a bounded number of fixed logic steps.
#[derive(Clone, Copy, Debug)]
pub struct PresentationClock {
    step: f64,
    max_steps: u32,
    accumulator: f64,
}

impl PresentationClock {
    /// `step` is the logic step in seconds. `max_steps` is the most steps one
    /// call may run.
    ///
    /// Returns `None` when `step` is not a positive finite number or
    /// `max_steps` is zero. A zero cap would make the catch-up bound meaningless.
    pub fn new(step: f64, max_steps: u32) -> Option<Self> {
        if !step.is_finite() || step <= 0.0 || max_steps == 0 {
            return None;
        }
        Some(Self {
            step,
            max_steps,
            accumulator: 0.0,
        })
    }

    /// Configured logic step in seconds.
    pub const fn step(&self) -> f64 {
        self.step
    }

    /// Most logic steps one [`Self::pump_logic`] call may run.
    pub const fn max_steps(&self) -> u32 {
        self.max_steps
    }

    /// Add `dt` seconds and return how many fixed steps the host should run.
    ///
    /// Non-finite and non-positive intervals change nothing. After the cap,
    /// whole steps still waiting are dropped and only the fractional remainder
    /// stays, so the next frame does not inherit the hitch.
    pub fn pump_logic(&mut self, dt: f64) -> u32 {
        if !dt.is_finite() || dt <= 0.0 {
            return 0;
        }
        let sum = self.accumulator + dt;
        self.accumulator = if sum.is_finite() {
            sum
        } else {
            self.step * f64::from(self.max_steps)
        };
        let mut steps = 0u32;
        while self.accumulator >= self.step && steps < self.max_steps {
            self.accumulator -= self.step;
            steps += 1;
        }
        if self.accumulator >= self.step {
            let dropped = (self.accumulator / self.step).floor();
            self.accumulator -= dropped * self.step;
            if !(0.0..self.step).contains(&self.accumulator) {
                self.accumulator = 0.0;
            }
        }
        steps
    }

    /// Fraction of the current logic step elapsed since the last completed step.
    ///
    /// The value is in `0.0..1.0`. It is for presentation only and does not
    /// select which logic step runs.
    pub fn presentation_alpha(&self) -> f64 {
        let alpha = self.accumulator / self.step;
        if alpha.is_finite() {
            alpha.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Drop accumulated frame time after an external pose correction.
    ///
    /// The host snaps the presented pose itself. This only clears the
    /// interpolation phase so the next frames do not glide from the stale span.
    pub fn reset(&mut self) {
        self.accumulator = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> PresentationClock {
        PresentationClock::new(0.5, 2).expect("positive step and cap")
    }

    #[test]
    fn partial_frame_exposes_alpha_without_a_step() {
        let mut clock = clock();
        assert_eq!(clock.pump_logic(0.25), 0);
        assert_eq!(clock.presentation_alpha(), 0.5);
        assert_eq!(clock.pump_logic(0.25), 1);
        assert_eq!(clock.presentation_alpha(), 0.0);
    }

    #[test]
    fn a_long_frame_keeps_the_step_and_drops_the_backlog() {
        let mut clock = clock();
        assert_eq!(clock.pump_logic(1.25), 2);
        assert!((clock.presentation_alpha() - 0.5).abs() < 1e-9);
        assert_eq!(clock.pump_logic(10.0), 2);
        assert!((clock.presentation_alpha() - 0.5).abs() < 1e-9);
        assert_eq!(clock.pump_logic(0.5), 1);
        clock.reset();
        assert_eq!(clock.pump_logic(10.0), 2);
        assert_eq!(clock.presentation_alpha(), 0.0);
        assert_eq!(clock.step(), 0.5);
    }

    #[test]
    fn invalid_intervals_and_reset_leave_a_defined_phase() {
        let mut clock = clock();
        assert_eq!(clock.pump_logic(0.25), 0);
        assert_eq!(clock.pump_logic(-1.0), 0);
        assert_eq!(clock.pump_logic(f64::NAN), 0);
        assert_eq!(clock.presentation_alpha(), 0.5);
        clock.reset();
        assert_eq!(clock.presentation_alpha(), 0.0);
        assert_eq!(clock.pump_logic(0.5), 1);
        assert!(PresentationClock::new(0.0, 2).is_none());
        assert!(PresentationClock::new(0.5, 0).is_none());
        assert!(PresentationClock::new(f64::INFINITY, 2).is_none());
    }
}
