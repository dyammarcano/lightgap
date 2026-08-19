//! Continuous adjustment during the transfer.
//!
//! The profile negotiated at the start expires: someone shifts a laptop, the
//! room light changes, autofocus hunts again. Without continuous adjustment, the
//! initial calibration only describes the first minute.
//!
//! Progression is **additive on the way up and multiplicative on the way down**,
//! like TCP congestion control and for the same reason: overshooting costs
//! little if you climb slowly, whereas backing off slowly once the link has
//! already broken prolongs the outage. When in doubt, retreat fast.

/// What to do with the profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adaptation {
    /// Raise the parameter: the link has room to spare.
    Increase,
    /// Leave it alone.
    Hold,
    /// Lower it: the link is struggling.
    Reduce,
    /// Lower it sharply and raise the alarm: the link is broken.
    Recover,
}

/// Success rate above which the link has room to spare.
pub const EXCELLENT: f32 = 0.99;
/// Above this, nothing needs touching.
pub const ACCEPTABLE: f32 = 0.95;
/// Below this the link is not degraded, it is broken.
pub const STRUGGLING: f32 = 0.85;

/// How many consecutive good observations are needed before daring to climb.
///
/// Several, not one: a lucky spike is not an improvement in the link, and
/// climbing on the first one produces an oscillation between two profiles that
/// costs more than staying at the lower one.
pub const GOOD_STREAK_TO_INCREASE: u32 = 3;

/// Additive-increase, multiplicative-decrease controller over an integer
/// parameter.
#[derive(Debug, Clone)]
pub struct Aimd {
    current: u32,
    min: u32,
    max: u32,
    increment: u32,
    decrease_factor: f32,
    good_streak: u32,
}

impl Aimd {
    /// # Panics
    /// If the range is empty.
    #[must_use]
    pub fn new(current: u32, min: u32, max: u32, increment: u32) -> Self {
        assert!(min > 0 && min <= max, "invalid range: {min}..={max}");
        Self {
            current: current.clamp(min, max),
            min,
            max,
            increment: increment.max(1),
            decrease_factor: 0.7,
            good_streak: 0,
        }
    }

    #[must_use]
    pub fn current(&self) -> u32 {
        self.current
    }

    #[must_use]
    pub fn good_streak(&self) -> u32 {
        self.good_streak
    }

    /// Takes in a success-rate observation and adjusts the parameter.
    pub fn observe(&mut self, success_rate: f32) -> Adaptation {
        let rate = success_rate.clamp(0.0, 1.0);

        if rate >= EXCELLENT {
            self.good_streak += 1;
            if self.good_streak >= GOOD_STREAK_TO_INCREASE && self.current < self.max {
                self.good_streak = 0;
                self.current = self.current.saturating_add(self.increment).min(self.max);
                return Adaptation::Increase;
            }
            return Adaptation::Hold;
        }

        self.good_streak = 0;

        if rate >= ACCEPTABLE {
            return Adaptation::Hold;
        }

        let factor = if rate >= STRUGGLING {
            self.decrease_factor
        } else {
            // Below the distress threshold, back off twice as hard: the link is
            // not degraded, it is broken, and stepping down one notch only
            // prolongs the outage.
            self.decrease_factor * self.decrease_factor
        };

        // Rounding, not truncation: 1000 x 0.7 comes out as 699.999... in
        // floating point, and truncating turns a clean factor into an off-by-one
        // that later shows up as a magic constant in the tests.
        let next = ((f64::from(self.current) * f64::from(factor)).round() as u32).max(self.min);
        let changed = next != self.current;
        self.current = next;

        if rate < STRUGGLING {
            Adaptation::Recover
        } else if changed {
            Adaptation::Reduce
        } else {
            // Already at the minimum: there is nothing left to cut, and saying
            // "Reduce" when nothing can be reduced would mislead the caller.
            Adaptation::Hold
        }
    }
}
