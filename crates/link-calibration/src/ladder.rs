//! Search for the largest parameter the link will sustain.
//!
//! Double until it breaks, then bisect, then back off with a margin. Deliberately
//! medium-agnostic: it works just as well negotiating bytes per QR code as
//! symbols per second over audio. All it needs to know is "at this value, what
//! success rate comes out?".
//!
//! The final margin is not decorative caution. An optical link degrades on its
//! own — someone shifts a laptop, the light changes, autofocus hunts — and
//! operating at the exact limit means falling over seconds after negotiating.

/// Where the search stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Doubling while it still works.
    Doubling,
    /// Bisecting between the last good and the first bad value.
    Bisecting,
    /// There is a result.
    Settled,
}

/// A probe ladder over an integer parameter.
#[derive(Debug, Clone)]
pub struct Ladder {
    min: u32,
    max: u32,
    current: u32,
    best_ok: Option<u32>,
    first_bad: Option<u32>,
    phase: Phase,
    margin_pct: u8,
    /// Success rate at or above which a value counts as good.
    threshold: f32,
    probes: u32,
}

/// Default margin deducted from the largest value that worked.
pub const DEFAULT_MARGIN_PCT: u8 = 15;

/// Success rate at which a value is considered sustainable.
///
/// High on purpose. A profile that hits 90% forces a resend of one frame in ten,
/// and in a medium where each frame costs a hundred milliseconds that hurts more
/// than having chosen a slightly smaller payload.
pub const DEFAULT_THRESHOLD: f32 = 0.97;

impl Ladder {
    /// # Panics
    /// If the range is empty or the starting value falls outside it.
    #[must_use]
    pub fn new(min: u32, max: u32, start: u32) -> Self {
        assert!(min > 0 && min <= max, "invalid range: {min}..={max}");
        assert!(
            (min..=max).contains(&start),
            "start {start} falls outside {min}..={max}"
        );
        Self {
            min,
            max,
            current: start,
            best_ok: None,
            first_bad: None,
            phase: Phase::Doubling,
            margin_pct: DEFAULT_MARGIN_PCT,
            threshold: DEFAULT_THRESHOLD,
            probes: 0,
        }
    }

    #[must_use]
    pub fn with_margin(mut self, pct: u8) -> Self {
        self.margin_pct = pct.min(90);
        self
    }

    #[must_use]
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// The value to probe right now.
    #[must_use]
    pub fn current(&self) -> u32 {
        self.current
    }

    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// How many probes have been issued. Useful for bounding how long
    /// calibration takes: nobody holds two laptops face to face indefinitely.
    #[must_use]
    pub fn probes(&self) -> u32 {
        self.probes
    }

    /// Takes in the result of probing [`Ladder::current`].
    pub fn record(&mut self, success_rate: f32) {
        if self.phase == Phase::Settled {
            return;
        }
        self.probes += 1;
        let ok = success_rate >= self.threshold;

        if ok {
            self.best_ok = Some(self.best_ok.map_or(self.current, |b| b.max(self.current)));
        } else {
            self.first_bad = Some(self.first_bad.map_or(self.current, |b| b.min(self.current)));
        }

        match self.phase {
            Phase::Doubling => {
                if !ok {
                    if self.best_ok.is_none() {
                        // The starting value failed and nothing has worked yet.
                        // Before giving up, probe the floor: giving up here would
                        // discard a link that does manage the minimum, merely
                        // because probing started too high.
                        if self.current > self.min {
                            self.current = self.min;
                        } else {
                            self.phase = Phase::Settled;
                        }
                        return;
                    }
                    self.phase = Phase::Bisecting;
                    self.step_bisect();
                    return;
                }

                if self.first_bad.is_some() {
                    // A failure above is already known: doubling further would
                    // overshoot it, so bisect instead.
                    self.phase = Phase::Bisecting;
                    self.step_bisect();
                    return;
                }

                if self.current >= self.max {
                    // Reached the ceiling while still working: nothing more to
                    // search for.
                    self.phase = Phase::Settled;
                } else {
                    self.current = self.current.saturating_mul(2).min(self.max);
                }
            }
            Phase::Bisecting => self.step_bisect(),
            Phase::Settled => {}
        }
    }

    fn step_bisect(&mut self) {
        let (lo, hi) = (
            self.best_ok.unwrap_or(self.min),
            self.first_bad.unwrap_or(self.max),
        );
        // With the ends adjacent there is nothing in between left to probe.
        if hi <= lo + 1 {
            self.phase = Phase::Settled;
            return;
        }
        let mid = lo + (hi - lo) / 2;
        if mid == self.current {
            self.phase = Phase::Settled;
            return;
        }
        self.current = mid;
    }

    /// Cuts the search short and keeps the best known value.
    ///
    /// Needed because calibration has a time budget: a conservative profile now
    /// beats an optimal one a minute from now.
    pub fn give_up(&mut self) {
        self.phase = Phase::Settled;
    }

    /// Recommended value, with the margin already deducted.
    ///
    /// `None` if no value was found that worked: in that case the link cannot
    /// even manage the minimum, and what needs fixing is the framing, not the
    /// negotiation.
    #[must_use]
    pub fn settled(&self) -> Option<u32> {
        if self.phase != Phase::Settled {
            return None;
        }
        let best = self.best_ok?;
        let with_margin =
            (u64::from(best) * u64::from(100 - u16::from(self.margin_pct)) / 100) as u32;
        Some(with_margin.max(self.min))
    }
}
