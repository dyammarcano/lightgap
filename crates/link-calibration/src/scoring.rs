//! Scoring a profile by what it **delivers**, not by what fits.
//!
//! The natural mistake is to keep the largest payload that reads. But a larger
//! frame takes longer to display and longer to decode, so it can deliver less
//! than a medium one that goes faster:
//!
//! ```text
//! 1500 B x  5 frames/s x 0.95 =  7,125 B/s
//!  900 B x 12 frames/s x 0.98 = 10,584 B/s
//! ```
//!
//! So what gets compared is goodput, penalised for what raw goodput does not
//! see: retries and latency.

/// What was measured while probing a profile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measurement {
    /// Useful bytes per frame, excluding headers.
    pub payload_bytes: u32,
    /// Frames per second the link actually sustains.
    pub frames_per_second: f32,
    /// Fraction of frames that read correctly, in 0..=1.
    pub success_rate: f32,
    /// Fraction of frames that have to be repeated, in 0..=1.
    pub retry_rate: f32,
    /// Cost of decoding one frame, in milliseconds.
    pub decode_ms: f32,
}

impl Measurement {
    /// Useful bytes per second. The raw figure.
    #[must_use]
    pub fn goodput_bps(&self) -> f64 {
        f64::from(self.payload_bytes)
            * f64::from(self.frames_per_second.max(0.0))
            * f64::from(self.success_rate.clamp(0.0, 1.0))
    }

    /// A score comparable across profiles.
    ///
    /// Two penalties are applied on top of goodput:
    ///
    /// - **Retries.** They cost bandwidth that raw goodput already deducts, but
    ///   they also occupy the window and add latency, so they weigh more than
    ///   their fraction suggests.
    /// - **Decode latency.** A profile that delivers the same but responds twice
    ///   as late delays all feedback and makes the session feel broken.
    #[must_use]
    pub fn score(&self) -> f64 {
        let base = self.goodput_bps();
        let retries = 1.0 - f64::from(self.retry_rate.clamp(0.0, 1.0));
        // A hundred milliseconds of decoding halves the score; that is the order
        // of magnitude of one optical frame, so going beyond it means decoding
        // costs more than transmitting.
        let latency = 1.0 / (1.0 + f64::from(self.decode_ms.max(0.0)) / 100.0);
        base * retries * latency
    }
}

/// Picks the best of several measured profiles.
///
/// Returns `None` for an empty list, or if none of them delivers anything: a
/// profile with zero goodput is not "the least bad", it is a link that does not
/// work, and returning it would have the session start out doomed.
#[must_use]
pub fn best<T: Copy>(candidates: &[(T, Measurement)]) -> Option<(T, Measurement)> {
    candidates
        .iter()
        .filter(|(_, m)| m.score() > 0.0)
        .max_by(|a, b| {
            a.1.score()
                .partial_cmp(&b.1.score())
                .unwrap_or(core::cmp::Ordering::Equal)
        })
        .copied()
}
