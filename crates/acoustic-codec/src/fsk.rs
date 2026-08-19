//! Two-tone frequency-shift keying.
//!
//! A bit becomes a burst of one of two tones. Detection is by Goertzel filter at
//! both frequencies over each symbol window, taking whichever is stronger. This
//! is the oldest trick in digital modulation and it is chosen here precisely
//! because it degrades gently: with a poor signal-to-noise ratio it makes more
//! errors, rather than losing lock and producing nothing.

/// Parameters both ends must agree on for a direction.
///
/// They are per direction, not per session: calibration routinely finds that
/// audio works from A to B in one band and from B to A in another, because the
/// two machines' speakers and microphones have no reason to match.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AcousticProfile {
    pub sample_rate: u32,
    /// Tone for a zero bit, in hertz.
    pub f0: f32,
    /// Tone for a one bit, in hertz.
    pub f1: f32,
    /// Symbols per second.
    pub symbol_rate: f32,
}

impl Default for AcousticProfile {
    fn default() -> Self {
        Self::conservative()
    }
}

impl AcousticProfile {
    /// A starting point that most hardware manages.
    ///
    /// The tones sit at 17.4 and 18.2 kHz: high enough that most adults do not
    /// notice them, low enough that laptop speakers still produce output and
    /// microphones still capture it. The 800 Hz separation is generous on
    /// purpose — closer tones need finer frequency resolution, which means
    /// longer symbols, which means less throughput and more exposure to drift.
    #[must_use]
    pub fn conservative() -> Self {
        Self {
            sample_rate: 48_000,
            f0: 17_400.0,
            f1: 18_200.0,
            symbol_rate: 100.0,
        }
    }

    /// Samples per symbol.
    #[must_use]
    pub fn samples_per_symbol(&self) -> usize {
        ((self.sample_rate as f32) / self.symbol_rate)
            .round()
            .max(1.0) as usize
    }

    /// Frequency resolution a symbol-length window can achieve, in hertz.
    ///
    /// Roughly the reciprocal of the window duration. If the tone separation is
    /// not comfortably larger than this, the two tones are not distinguishable no
    /// matter how clean the signal is — which is a configuration error, not a
    /// channel problem, and worth catching before anyone blames the room.
    #[must_use]
    pub fn frequency_resolution(&self) -> f32 {
        self.symbol_rate
    }

    /// Whether the tone separation is resolvable at this symbol rate.
    #[must_use]
    pub fn is_resolvable(&self) -> bool {
        (self.f1 - self.f0).abs() >= self.frequency_resolution() * 2.0
    }

    /// Whether both tones stay below the Nyquist limit, with margin.
    #[must_use]
    pub fn within_nyquist(&self) -> bool {
        let nyquist = self.sample_rate as f32 / 2.0;
        self.f0.max(self.f1) < nyquist * 0.95
    }
}

/// The preamble that precedes every frame.
///
/// Alternating bits, which produce alternating tones: the most distinctive
/// pattern this modulation can make, and therefore the easiest to find in noise.
/// It serves two jobs at once — announcing that a frame is starting, and
/// revealing where the symbol boundaries fall.
pub const PREAMBLE: [bool; 16] = [
    true, false, true, false, true, false, true, false, true, false, true, false, true, false,
    true, false,
];

/// Turns bits into samples.
///
/// Phase is carried across symbol boundaries rather than reset. A phase
/// discontinuity at every symbol edge would splatter energy across the spectrum
/// — audible as clicks, and leaking into whatever band the other direction is
/// using, which matters because the two directions share the air by frequency
/// division.
#[must_use]
pub fn modulate(bits: &[bool], profile: &AcousticProfile) -> Vec<f32> {
    let sps = profile.samples_per_symbol();
    let mut out = Vec::with_capacity(bits.len() * sps);
    let mut phase = 0.0f32;
    let two_pi = std::f32::consts::TAU;

    for &bit in bits {
        let freq = if bit { profile.f1 } else { profile.f0 };
        let step = two_pi * freq / profile.sample_rate as f32;
        for _ in 0..sps {
            out.push(phase.sin());
            phase += step;
            if phase > two_pi {
                phase -= two_pi;
            }
        }
    }

    out
}

/// Symbol periods of silence appended after every frame.
///
/// A guard interval, as every real modem has. The receiver's idea of where a
/// symbol starts is never exactly the sender's — noise shifts the correlation
/// peak by a few samples — so the final symbol's window extends slightly past
/// the last sample the sender emitted. Without a guard, that window runs off the
/// end of the recording and the last bit is lost, which surfaces as a truncated
/// frame and looks exactly like corruption.
pub const GUARD_SYMBOLS: usize = 2;

/// Prepends the preamble, modulates, and appends the guard interval.
#[must_use]
pub fn modulate_frame(bits: &[bool], profile: &AcousticProfile) -> Vec<f32> {
    let mut all = PREAMBLE.to_vec();
    all.extend_from_slice(bits);
    let mut out = modulate(&all, profile);
    out.resize(
        out.len() + GUARD_SYMBOLS * profile.samples_per_symbol(),
        0.0,
    );
    out
}

/// Goertzel filter: the energy at one frequency over one window.
///
/// Cheaper than an FFT when only a couple of frequencies matter, which is
/// exactly this case. Two bins per symbol instead of a whole transform.
fn goertzel(samples: &[f32], freq: f32, sample_rate: u32) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let w = std::f32::consts::TAU * freq / sample_rate as f32;
    let coeff = 2.0 * w.cos();

    let mut s_prev = 0.0f32;
    let mut s_prev2 = 0.0f32;
    for &x in samples {
        let s = x + coeff * s_prev - s_prev2;
        s_prev2 = s_prev;
        s_prev = s;
    }

    (s_prev * s_prev + s_prev2 * s_prev2 - coeff * s_prev * s_prev2).max(0.0)
}

/// What came out of a demodulation attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct Demodulated {
    pub bits: Vec<bool>,
    /// Sample offset where the preamble was found.
    pub offset: usize,
    /// Mean confidence per symbol, in 0..=1, where 0 means the two tones were
    /// indistinguishable and 1 means only one was present.
    pub confidence: f32,
}

/// Decides one symbol, returning the bit and how sure it is.
fn decide(window: &[f32], profile: &AcousticProfile) -> (bool, f32) {
    let e0 = goertzel(window, profile.f0, profile.sample_rate);
    let e1 = goertzel(window, profile.f1, profile.sample_rate);
    let total = e0 + e1;
    if total <= f32::EPSILON {
        return (false, 0.0);
    }
    ((e1 > e0), ((e1 - e0).abs() / total))
}

/// How far into the signal to search for the preamble, in symbols.
///
/// Bounded because the search is quadratic in the window: an unbounded search
/// over a long recording would cost more than decoding it.
const MAX_SEARCH_SYMBOLS: usize = 64;

/// Finds the preamble and decodes the symbols after it.
///
/// The offset search matters more than it looks. Sampling a symbol window
/// straddling a boundary mixes both tones and the decision collapses to a coin
/// flip, so getting the alignment right is worth more than any amount of
/// filtering afterwards.
#[must_use]
pub fn demodulate(
    samples: &[f32],
    profile: &AcousticProfile,
    expect_bits: usize,
) -> Option<Demodulated> {
    let sps = profile.samples_per_symbol();
    if samples.len() < sps * PREAMBLE.len() {
        return None;
    }

    let search_limit =
        (sps * MAX_SEARCH_SYMBOLS).min(samples.len().saturating_sub(sps * PREAMBLE.len()));

    // Coarse then fine: step a quarter symbol to find the neighbourhood, then
    // walk it sample by sample. A pure sample-by-sample search over the whole
    // window would be 48000 correlations per second of audio.
    let coarse_step = (sps / 4).max(1);
    let mut best_offset = 0usize;
    let mut best_score = f32::NEG_INFINITY;

    let score_at = |offset: usize| -> f32 {
        let mut score = 0.0f32;
        for (i, &expected) in PREAMBLE.iter().enumerate() {
            let start = offset + i * sps;
            let end = start + sps;
            if end > samples.len() {
                return f32::NEG_INFINITY;
            }
            let (bit, conf) = decide(&samples[start..end], profile);
            score += if bit == expected { conf } else { -conf };
        }
        score
    };

    for offset in (0..=search_limit).step_by(coarse_step) {
        let s = score_at(offset);
        if s > best_score {
            best_score = s;
            best_offset = offset;
        }
    }

    let fine_lo = best_offset.saturating_sub(coarse_step);
    let fine_hi = (best_offset + coarse_step).min(search_limit);
    for offset in fine_lo..=fine_hi {
        let s = score_at(offset);
        if s > best_score {
            best_score = s;
            best_offset = offset;
        }
    }

    // Demand that most of the preamble agreed, and demand it strictly.
    //
    // The subtlety that makes a lax threshold useless: this is a
    // multiple-comparisons problem. Thousands of candidate offsets are scored,
    // and the best of thousands of random walks looks impressive even when there
    // is no signal at all. A threshold picked for a single trial — two standard
    // deviations, say — produces a false lock essentially every time once it is
    // applied thousands of times over.
    //
    // A genuine preamble scores near `PREAMBLE.len()`, since every symbol agrees
    // with high confidence. Requiring 60% of that is far out in the tail for
    // noise while still comfortable for a real signal, which the sweep test
    // confirms holds down to 0 dB.
    //
    // The asymmetry justifying a strict threshold: a missed frame is retried by
    // the layer above and costs a moment, whereas a frame invented from noise
    // hands the protocol confident garbage. Losing a frame is cheap; fabricating
    // one is not.
    let min_score = PREAMBLE.len() as f32 * 0.6;
    if best_score < min_score {
        return None;
    }

    let data_start = best_offset + PREAMBLE.len() * sps;
    let mut bits = Vec::with_capacity(expect_bits);
    let mut confidence_sum = 0.0f32;

    // A scratch buffer for the final, possibly short window. Zero-padding a
    // partial window is far better than dropping the symbol: a dropped symbol
    // produces a frame one bit short, which the framing layer reports as
    // truncation and which looks like corruption rather than like the
    // synchronisation offset it actually is.
    let mut tail = vec![0.0f32; sps];

    for i in 0..expect_bits {
        let start = data_start + i * sps;
        if start >= samples.len() {
            break;
        }
        let end = start + sps;

        let (bit, conf) = if end <= samples.len() {
            decide(&samples[start..end], profile)
        } else {
            let available = samples.len() - start;
            // Below half a symbol there is not enough left to decide on; calling
            // it would be guessing, and a confident guess is worse than a short
            // frame.
            if available * 2 < sps {
                break;
            }
            tail[..available].copy_from_slice(&samples[start..]);
            tail[available..].fill(0.0);
            decide(&tail, profile)
        };

        bits.push(bit);
        confidence_sum += conf;
    }

    let confidence = if bits.is_empty() {
        0.0
    } else {
        confidence_sum / bits.len() as f32
    };

    Some(Demodulated {
        bits,
        offset: best_offset,
        confidence,
    })
}

/// Packs bytes into bits, most significant first.
#[must_use]
pub fn bytes_to_bits(bytes: &[u8]) -> Vec<bool> {
    let mut bits = Vec::with_capacity(bytes.len() * 8);
    for b in bytes {
        for i in (0..8).rev() {
            bits.push((b >> i) & 1 == 1);
        }
    }
    bits
}

/// Packs bits back into bytes, most significant first.
///
/// A trailing partial byte is dropped: a truncated transmission should not
/// produce a byte made partly of silence.
#[must_use]
pub fn bits_to_bytes(bits: &[bool]) -> Vec<u8> {
    bits.chunks_exact(8)
        .map(|c| c.iter().fold(0u8, |acc, &b| (acc << 1) | u8::from(b)))
        .collect()
}
