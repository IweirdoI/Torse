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
    /// Rounding allowance in step units carried with the fractional phase.
    phase_roundoff: f64,
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
            phase_roundoff: 0.0,
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
        // Scale before adding only when the finite operands overflow their
        // sum. accumulator < step, so this scaled sum remains finite.
        let mut owed = if sum.is_finite() {
            sum / self.step
        } else {
            self.accumulator / self.step + dt / self.step
        };
        // Normalize in step units before applying the catch-up cap. Decimal
        // steps such as 1/20 are not exactly representable: taking 2.0 % 0.05
        // instead leaves almost a full step and invents a tick next frame.
        // Capped and uncapped frames must use the same boundary tolerance.
        if owed.is_finite() {
            let tolerance = self.phase_roundoff + 8.0 * f64::EPSILON * owed.max(1.0);
            if (owed - owed.round()).abs() <= tolerance {
                owed = owed.round();
                self.phase_roundoff = 0.0;
            } else {
                // A fractional hitch can lose precision at its original
                // scale. Preserve that allowance until the phase reaches a
                // boundary; the next short frame must not shrink it away.
                self.phase_roundoff = tolerance;
            }
            self.accumulator = owed.fract() * self.step;
            return owed.floor().min(f64::from(self.max_steps)) as u32;
        }
        // A very small step can overflow the quotient even with a finite
        // interval. All whole overdue steps exceed the cap in that case;
        // remainder still provides a finite presentation phase.
        self.accumulator = sum % self.step;
        self.phase_roundoff = 0.0;
        self.max_steps
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
        self.phase_roundoff = 0.0;
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

    #[test]
    fn decimal_steps_do_not_delay_a_tick_at_the_boundary() {
        let mut clock = PresentationClock::new(0.05, 8).expect("clock");
        assert_eq!(clock.pump_logic(0.15), 3);
        assert!(clock.presentation_alpha() < 1e-12);
        assert_eq!(clock.pump_logic(1.0), 8);
        assert!(clock.presentation_alpha() < 1e-12);
    }

    #[test]
    fn two_second_hitch_does_not_leave_a_phantom_tick() {
        let mut clock = PresentationClock::new(0.05, 8).expect("clock");
        assert_eq!(clock.pump_logic(2.0), 8);
        let alpha = clock.presentation_alpha();
        assert_eq!(clock.pump_logic(0.001), 0, "phase after hitch was {alpha}");
        assert!((clock.presentation_alpha() - 0.02).abs() < 1e-12);
        for _ in 0..48 {
            assert_eq!(clock.pump_logic(0.001), 0);
        }
        assert_eq!(clock.pump_logic(0.001), 1);
        assert_eq!(clock.presentation_alpha(), 0.0);
    }

    #[test]
    fn capped_frames_normalize_boundaries_and_preserve_real_remainders() {
        for step in [0.05, 0.02, 0.125, 1.0 / 60.0] {
            for whole_steps in [8, 9, 20, 40, 50, 200, 1200, 72_000] {
                let mut clock = PresentationClock::new(step, 8).expect("clock");
                assert_eq!(clock.pump_logic(step * f64::from(whole_steps)), 8);
                assert!(
                    clock.presentation_alpha() < 1e-12,
                    "step {step}, count {whole_steps}"
                );
                assert_eq!(clock.pump_logic(step * 0.02), 0);
                assert!((clock.presentation_alpha() - 0.02).abs() < 1e-12);

                clock.reset();
                assert_eq!(clock.pump_logic(step * (f64::from(whole_steps) + 0.5)), 8);
                assert!((clock.presentation_alpha() - 0.5).abs() < 1e-9);
                assert_eq!(clock.pump_logic(step * 0.25), 0);
                assert_eq!(clock.pump_logic(step * 0.25), 1);
                assert_eq!(clock.presentation_alpha(), 0.0);
            }
        }

        let mut clock = PresentationClock::new(0.05, 8).expect("clock");
        assert_eq!(clock.pump_logic(2.025), 8);
        assert!((clock.presentation_alpha() - 0.5).abs() < 1e-12);
        assert_eq!(clock.pump_logic(0.001), 0);
        assert!((clock.presentation_alpha() - 0.52).abs() < 1e-12);
        assert_eq!(clock.pump_logic(0.024), 1);
        assert!(clock.presentation_alpha() < 1e-12);

        clock.reset();
        assert_eq!(clock.pump_logic(2.0 - 1e-9), 8);
        assert!(clock.presentation_alpha() > 0.999_999);
        assert_eq!(clock.pump_logic(0.001), 1);
    }

    #[test]
    fn large_hitches_keep_a_defined_fraction_without_overflowing_the_quotient() {
        let mut clock = clock();
        assert_eq!(clock.pump_logic(2.0f64.powi(40) + 0.125), 2);
        assert_eq!(clock.presentation_alpha(), 0.25);
        assert_eq!(clock.pump_logic(0.125), 0);
        assert_eq!(clock.presentation_alpha(), 0.5);

        let mut tiny = PresentationClock::new(f64::MIN_POSITIVE, 8).expect("clock");
        assert_eq!(tiny.pump_logic(f64::MAX), 8);
        assert!((0.0..1.0).contains(&tiny.presentation_alpha()));
        assert_eq!(tiny.pump_logic(f64::MIN_POSITIVE), 1);
    }

    #[test]
    fn twenty_hz_logic_is_independent_of_presentation_rate() {
        for fps in [30, 60, 144, 1000] {
            let mut clock = PresentationClock::new(0.05, 8).expect("clock");
            let ticks: u32 = (0..fps)
                .map(|_| clock.pump_logic(1.0 / f64::from(fps)))
                .sum();
            assert_eq!(ticks, 20, "presentation rate {fps}");
            assert!(clock.presentation_alpha() < 1e-12);
        }
    }

    #[test]
    fn finite_intervals_that_overflow_the_sum_still_count_steps() {
        let mut clock = PresentationClock::new(1.5e308, 8).expect("clock");
        assert_eq!(clock.pump_logic(1e308), 0);
        assert_eq!(clock.pump_logic(1e308), 1);
        assert!((clock.presentation_alpha() - 1.0 / 3.0).abs() < 1e-12);
    }
}
