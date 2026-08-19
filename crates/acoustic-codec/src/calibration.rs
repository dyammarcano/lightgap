//! Working out whether the acoustic channel is usable, and on what terms.
//!
//! The application never assumes audio works. It measures, and it is prepared
//! for the answer to be no — which on a lot of hardware it is. Operating system
//! echo cancellation and noise suppression treat anything near the band this
//! modulation uses as garbage to be removed, laptop speakers roll off, and
//! microphones filter. The honest outcome is often [`Viability::Unavailable`],
//! and a calibration that never returns it is not measuring.
//!
//! **Why bands are assigned disjointly per direction.** Every microphone hears
//! its own speaker. The usual answers are echo cancellation, which is hard and
//! which the operating system may already be doing badly, or time division,
//! which halves the rate and needs tight synchronisation over a channel that has
//! none. Frequency division sidesteps both: give each direction its own band and
//! the two can transmit simultaneously without ever competing. Calibration is
//! already measuring per-band quality per direction, so the information needed
//! to do this is free.

use crate::fsk::AcousticProfile;

/// What one band looks like to one microphone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandMeasurement {
    pub start_hz: f32,
    pub end_hz: f32,
    /// Ambient level with nothing transmitting, in decibels relative to full
    /// scale. More negative is quieter.
    pub noise_floor_db: f32,
    /// Level received when the peer transmits a tone in this band.
    pub tone_db: f32,
}

impl BandMeasurement {
    #[must_use]
    pub fn centre_hz(&self) -> f32 {
        (self.start_hz + self.end_hz) / 2.0
    }

    #[must_use]
    pub fn width_hz(&self) -> f32 {
        (self.end_hz - self.start_hz).abs()
    }

    /// How far the tone rises above the ambient level.
    ///
    /// This, not the absolute tone level, is what decides whether the band is
    /// usable. A loud tone in a loud band carries no more information than a
    /// quiet tone in a quiet one.
    #[must_use]
    pub fn snr_db(&self) -> f32 {
        self.tone_db - self.noise_floor_db
    }

    /// Whether the band carries enough signal to modulate over.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.snr_db() >= MIN_BAND_SNR_DB && self.width_hz() >= MIN_BAND_WIDTH_HZ
    }
}

/// Signal-to-noise floor below which a band is not worth using.
///
/// Ten decibels. Below that the error rate climbs fast enough that the
/// retransmissions cost more than the channel delivers, and the acoustic channel
/// exists to save round trips rather than to add them.
pub const MIN_BAND_SNR_DB: f32 = 10.0;

/// Narrower than this and there is no room for two separable tones.
pub const MIN_BAND_WIDTH_HZ: f32 = 600.0;

/// Guard between the two directions' bands.
///
/// Not zero, because filters are not brick walls and a tone sitting right at the
/// edge of the neighbouring band leaks into it. Leakage between directions is
/// worse than leakage from the room: it is correlated with what the other side
/// is doing, so it appears exactly when both directions are busy.
pub const DIRECTION_GUARD_HZ: f32 = 400.0;

/// How useful the acoustic channel turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Viability {
    /// Both directions work on disjoint bands and can run simultaneously.
    FullDuplex,
    /// Both directions work but must take turns, because no disjoint pair of
    /// usable bands was found.
    HalfDuplex,
    /// Too slow or too unreliable for data, but adequate for acknowledgements
    /// and status. This is the outcome the design actually hopes for.
    ControlOnly,
    /// The hardware or the room will not support it. Visual only.
    Unavailable,
}

impl Viability {
    /// Whether anything can be sent over audio at all.
    #[must_use]
    pub const fn usable(self) -> bool {
        !matches!(self, Self::Unavailable)
    }

    /// Whether both directions can transmit at the same time.
    #[must_use]
    pub const fn simultaneous(self) -> bool {
        matches!(self, Self::FullDuplex)
    }
}

/// The result of assigning bands to directions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandPlan {
    /// Band the leader transmits in.
    pub leader_tx: BandMeasurement,
    /// Band the follower transmits in.
    pub follower_tx: BandMeasurement,
    pub viability: Viability,
}

impl BandPlan {
    /// Whether the two bands are genuinely separated.
    #[must_use]
    pub fn disjoint(&self) -> bool {
        let (lo, hi) = if self.leader_tx.centre_hz() <= self.follower_tx.centre_hz() {
            (&self.leader_tx, &self.follower_tx)
        } else {
            (&self.follower_tx, &self.leader_tx)
        };
        hi.start_hz - lo.end_hz >= DIRECTION_GUARD_HZ
    }

    /// A modulation profile for one direction, derived from its band.
    ///
    /// The two tones are placed a quarter and three quarters of the way across,
    /// which keeps both away from the edges where filter roll-off bites and
    /// where the neighbouring direction leaks in.
    #[must_use]
    pub fn profile_for(
        &self,
        band: &BandMeasurement,
        sample_rate: u32,
        symbol_rate: f32,
    ) -> AcousticProfile {
        let w = band.width_hz();
        AcousticProfile {
            sample_rate,
            f0: band.start_hz + w * 0.25,
            f1: band.start_hz + w * 0.75,
            symbol_rate,
        }
    }
}

/// Assigns each direction a band, keeping them disjoint where possible.
///
/// Takes what each side measured while listening to the other. The leader gets
/// the lower band by convention — an arbitrary but fixed choice, so both sides
/// reach the same plan without another exchange.
///
/// Returns `None` when neither direction has a usable band, which is
/// [`Viability::Unavailable`] and a perfectly normal outcome.
#[must_use]
pub fn assign_bands(
    heard_by_follower: &[BandMeasurement],
    heard_by_leader: &[BandMeasurement],
) -> Option<BandPlan> {
    // What the follower heard constrains what the leader may transmit, and vice
    // versa. Getting this the wrong way round is the same mistake as sizing a QR
    // code from your own camera.
    let mut leader_options: Vec<_> = heard_by_follower
        .iter()
        .copied()
        .filter(BandMeasurement::is_usable)
        .collect();
    let mut follower_options: Vec<_> = heard_by_leader
        .iter()
        .copied()
        .filter(BandMeasurement::is_usable)
        .collect();

    if leader_options.is_empty() && follower_options.is_empty() {
        return None;
    }

    // Best signal-to-noise first, so a compromise is only made when it buys
    // simultaneity.
    let by_snr = |a: &BandMeasurement, b: &BandMeasurement| {
        b.snr_db()
            .partial_cmp(&a.snr_db())
            .unwrap_or(core::cmp::Ordering::Equal)
    };
    leader_options.sort_by(by_snr);
    follower_options.sort_by(by_snr);

    // Look for a disjoint pair with the leader below the follower. Preferring
    // disjointness over raw quality is deliberate: full duplex roughly doubles
    // the useful rate, which beats a few decibels on one direction.
    for l in &leader_options {
        for f in &follower_options {
            if l.centre_hz() < f.centre_hz() && f.start_hz - l.end_hz >= DIRECTION_GUARD_HZ {
                return Some(BandPlan {
                    leader_tx: *l,
                    follower_tx: *f,
                    viability: Viability::FullDuplex,
                });
            }
        }
    }

    // No disjoint pair. Both directions can still work, but only by taking
    // turns, since each would otherwise hear its own speaker in the band it is
    // trying to receive on.
    if let (Some(l), Some(f)) = (leader_options.first(), follower_options.first()) {
        return Some(BandPlan {
            leader_tx: *l,
            follower_tx: *f,
            viability: Viability::HalfDuplex,
        });
    }

    // Only one direction works. Still useful — acknowledgements mostly flow one
    // way — but it cannot be called duplex.
    let only = leader_options
        .first()
        .or_else(|| follower_options.first())
        .copied()?;
    Some(BandPlan {
        leader_tx: only,
        follower_tx: only,
        viability: Viability::ControlOnly,
    })
}

/// What a modulation test measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModulationTest {
    pub symbol_rate: f32,
    /// Fraction of bits received wrongly, in 0..=1.
    pub bit_error_rate: f32,
    /// Fraction of frames that failed entirely, in 0..=1.
    pub frame_error_rate: f32,
    /// Round trip in milliseconds.
    pub latency_ms: f32,
}

/// Frame error rate above which the channel is not worth using for anything.
pub const MAX_USABLE_FRAME_ERROR: f32 = 0.30;
/// Frame error rate below which the channel is comfortable.
pub const GOOD_FRAME_ERROR: f32 = 0.05;

impl ModulationTest {
    /// Useful bits per second after errors.
    #[must_use]
    pub fn goodput_bps(&self) -> f32 {
        self.symbol_rate * (1.0 - self.frame_error_rate.clamp(0.0, 1.0))
    }

    /// Whether this configuration is worth using at all.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.frame_error_rate <= MAX_USABLE_FRAME_ERROR
    }
}

/// Picks the fastest symbol rate that still delivers.
///
/// Fastest rather than most reliable, because the reliability floor is already
/// enforced by [`ModulationTest::is_usable`]. Optimising past that point trades
/// throughput for a robustness that the layer above does not need — it already
/// retries.
#[must_use]
pub fn best_modulation(tests: &[ModulationTest]) -> Option<ModulationTest> {
    tests
        .iter()
        .filter(|t| t.is_usable())
        .max_by(|a, b| {
            a.goodput_bps()
                .partial_cmp(&b.goodput_bps())
                .unwrap_or(core::cmp::Ordering::Equal)
        })
        .copied()
}

/// Decides the final verdict from the band plan and the modulation results.
#[must_use]
pub fn decide_viability(plan: Option<&BandPlan>, best: Option<&ModulationTest>) -> Viability {
    let (Some(plan), Some(best)) = (plan, best) else {
        return Viability::Unavailable;
    };
    if !best.is_usable() {
        return Viability::Unavailable;
    }

    // A channel that works but only just is demoted to carrying control traffic.
    // Sending data over a link that drops a fifth of its frames would cost more
    // in retransmission than it delivers.
    if best.frame_error_rate > GOOD_FRAME_ERROR {
        return Viability::ControlOnly;
    }

    match plan.viability {
        Viability::FullDuplex if plan.disjoint() => Viability::FullDuplex,
        Viability::FullDuplex | Viability::HalfDuplex => Viability::HalfDuplex,
        other => other,
    }
}
