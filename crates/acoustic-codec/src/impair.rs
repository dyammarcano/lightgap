//! Synthetic impairment of an audio signal.
//!
//! The acoustic counterpart to the optical channel's synthetic camera, and it
//! exists for the same reason: without it, testing the acoustic channel means
//! two machines, a quiet room, and no way to sweep conditions systematically.
//!
//! What is modelled, and why each piece:
//!
//! - **Noise.** Room noise and microphone self-noise. The dominant impairment,
//!   and the one signal-to-noise ratio is defined against.
//! - **Band-pass filtering.** Laptop speakers and microphones roll off hard near
//!   the band this modulation uses. Ignoring it would make the channel look far
//!   healthier than it is at 18 kHz.
//! - **Clipping.** A signal driven too hard, which happens whenever the user
//!   turns the volume up to improve range. It generates harmonics rather than
//!   simply losing amplitude, so it is not the same as attenuation.
//! - **Sample-rate drift.** Two devices' clocks are never identical, so the
//!   receiver's idea of a symbol boundary slides against the sender's over time.
//!   On a long frame this is what breaks synchronisation, and it is invisible in
//!   any test that generates and consumes samples with the same clock.

/// How the acoustic path degrades the signal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Impairment {
    /// Signal-to-noise ratio in decibels. Lower is worse.
    pub snr_db: f32,
    /// Amplitude scaling before noise, modelling distance and volume.
    pub gain: f32,
    /// Clip threshold as a fraction of full scale; 1.0 means no clipping.
    pub clip: f32,
    /// Low and high edges of the passband, in hertz. Anything outside is
    /// attenuated.
    pub band: (f32, f32),
    /// Relative clock error, as a fraction. 1e-4 means the receiver's clock runs
    /// 0.01% fast.
    pub clock_drift: f32,
    pub seed: u64,
}

impl Default for Impairment {
    fn default() -> Self {
        Self::clean()
    }
}

impl Impairment {
    /// A perfect path. The control case.
    #[must_use]
    pub fn clean() -> Self {
        Self {
            snr_db: f32::INFINITY,
            gain: 1.0,
            clip: 1.0,
            band: (0.0, f32::INFINITY),
            clock_drift: 0.0,
            seed: 1,
        }
    }

    /// Two laptops on a desk in a normal room.
    #[must_use]
    pub fn typical() -> Self {
        Self {
            snr_db: 20.0,
            gain: 0.6,
            clip: 1.0,
            band: (300.0, 19_000.0),
            clock_drift: 5e-5,
            seed: 1,
        }
    }

    /// A noisy room, a speaker driven hard, and hardware that rolls off early.
    #[must_use]
    pub fn harsh() -> Self {
        Self {
            snr_db: 8.0,
            gain: 0.9,
            clip: 0.7,
            band: (300.0, 18_500.0),
            clock_drift: 2e-4,
            seed: 1,
        }
    }

    #[must_use]
    pub fn with_snr(mut self, snr_db: f32) -> Self {
        self.snr_db = snr_db;
        self
    }

    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }
}

/// Reproducible noise. Same seed, same signal.
struct Noise(u64);

impl Noise {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 32) as u32) as f32 / u32::MAX as f32
    }

    /// Gaussian by summing uniforms: twelve minus six gives mean 0, variance 1.
    fn gaussian(&mut self) -> f32 {
        let mut acc = 0.0f32;
        for _ in 0..12 {
            acc += self.next_f32();
        }
        acc - 6.0
    }
}

/// A one-pole high-pass followed by a one-pole low-pass.
///
/// Not a sharp filter, and deliberately so: real speaker and microphone roll-off
/// is gentle, and modelling it as a brick wall would make the channel look
/// cliff-edged in a way it is not.
fn band_pass(samples: &mut [f32], sample_rate: u32, band: (f32, f32)) {
    let (lo, hi) = band;
    let fs = sample_rate as f32;

    if lo > 0.0 && lo < fs / 2.0 {
        let rc = 1.0 / (std::f32::consts::TAU * lo);
        let dt = 1.0 / fs;
        let alpha = rc / (rc + dt);
        let mut prev_in = 0.0f32;
        let mut prev_out = 0.0f32;
        for s in samples.iter_mut() {
            let out = alpha * (prev_out + *s - prev_in);
            prev_in = *s;
            prev_out = out;
            *s = out;
        }
    }

    if hi.is_finite() && hi < fs / 2.0 {
        let rc = 1.0 / (std::f32::consts::TAU * hi);
        let dt = 1.0 / fs;
        let alpha = dt / (rc + dt);
        let mut prev = 0.0f32;
        for s in samples.iter_mut() {
            prev += alpha * (*s - prev);
            *s = prev;
        }
    }
}

/// Resamples to model a clock that runs fast or slow.
///
/// Linear interpolation is enough: what is being modelled is a slow slide of the
/// symbol boundary, not a change in the signal's content.
fn drift(samples: &[f32], rate: f32) -> Vec<f32> {
    if rate.abs() < f32::EPSILON {
        return samples.to_vec();
    }
    let step = 1.0 + rate;
    let out_len = ((samples.len() as f32) / step).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f32 * step;
        let idx = pos.floor() as usize;
        let frac = pos - idx as f32;
        let a = samples.get(idx).copied().unwrap_or(0.0);
        let b = samples.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

/// Runs a signal through the acoustic path.
#[must_use]
pub fn impair(samples: &[f32], sample_rate: u32, imp: &Impairment) -> Vec<f32> {
    let mut out: Vec<f32> = samples.iter().map(|s| s * imp.gain).collect();

    // Clipping comes before filtering: in reality the amplifier saturates and
    // then the transducer's response shapes what comes out, not the other way
    // round. Doing it backwards would hide the harmonics clipping generates.
    if imp.clip < 1.0 {
        for s in out.iter_mut() {
            *s = s.clamp(-imp.clip, imp.clip);
        }
    }

    band_pass(&mut out, sample_rate, imp.band);

    if imp.snr_db.is_finite() {
        // Signal power is measured after filtering, so the ratio describes what
        // actually reaches the microphone rather than what left the speaker.
        let power: f32 = out.iter().map(|s| s * s).sum::<f32>() / out.len().max(1) as f32;
        let noise_power = power / 10f32.powf(imp.snr_db / 10.0);
        let sigma = noise_power.max(0.0).sqrt();
        let mut rng = Noise(imp.seed | 1);
        for s in out.iter_mut() {
            *s += rng.gaussian() * sigma;
        }
    }

    if imp.clock_drift.abs() > f32::EPSILON {
        out = drift(&out, imp.clock_drift);
    }

    out
}

/// Prepends silence, modelling a receiver that started listening early.
///
/// Without this, every test would hand the demodulator a signal starting exactly
/// at sample zero, which is the one case the preamble search never has to
/// handle.
#[must_use]
pub fn with_leading_silence(samples: &[f32], silence_samples: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; silence_samples];
    out.extend_from_slice(samples);
    out
}
