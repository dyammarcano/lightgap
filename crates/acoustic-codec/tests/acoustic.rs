//! The whole acoustic loop, with no speaker: bytes to tones to impaired signal
//! to bytes.
//!
//! Same purpose as the optical channel's synthetic camera. Without it, testing
//! this channel means two machines and a quiet room, and there is no way to sweep
//! conditions systematically.

use acoustic_codec::framing::{Framer, FramingError, MAX_ACOUSTIC_PAYLOAD};
use acoustic_codec::fsk::{
    bits_to_bytes, bytes_to_bits, demodulate, modulate_frame, AcousticProfile, PREAMBLE,
};
use acoustic_codec::impair::{impair, with_leading_silence, Impairment};

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i.wrapping_mul(37) % 251) as u8).collect()
}

/// Full round trip: frame, modulate, impair, demodulate, deframe.
fn round_trip(
    data: &[u8],
    profile: &AcousticProfile,
    imp: &Impairment,
    silence: usize,
) -> Option<Vec<u8>> {
    let bits = Framer::encode(data).ok()?;
    let clean = modulate_frame(&bits, profile);
    let signal = with_leading_silence(&clean, silence);
    let dirty = impair(&signal, profile.sample_rate, imp);

    let expect = Framer::encoded_bits(data.len());
    let demod = demodulate(&dirty, profile, expect)?;
    Framer::decode(&demod.bits).ok()
}

#[test]
fn the_default_profile_is_actually_usable() {
    let p = AcousticProfile::conservative();
    assert!(
        p.is_resolvable(),
        "the tone separation must exceed the frequency resolution of a symbol \
         window, otherwise no amount of signal quality helps"
    );
    assert!(p.within_nyquist(), "both tones must sit below Nyquist");
    assert_eq!(p.samples_per_symbol(), 480, "48 kHz at 100 baud");
}

#[test]
fn a_configuration_that_cannot_resolve_its_own_tones_says_so() {
    // Tones 50 Hz apart at 200 baud: the symbol window cannot tell them apart.
    // Catching this here matters, because in the field it looks exactly like a
    // bad room and people go looking for the wrong problem.
    let bad = AcousticProfile {
        sample_rate: 48_000,
        f0: 18_000.0,
        f1: 18_050.0,
        symbol_rate: 200.0,
    };
    assert!(!bad.is_resolvable());
}

#[test]
fn a_profile_above_nyquist_says_so() {
    let bad = AcousticProfile {
        sample_rate: 8_000,
        f0: 17_400.0,
        f1: 18_200.0,
        symbol_rate: 100.0,
    };
    assert!(
        !bad.within_nyquist(),
        "17 kHz cannot exist at 8 kHz sampling"
    );
}

#[test]
fn bits_and_bytes_round_trip() {
    let data = payload(64);
    assert_eq!(bits_to_bytes(&bytes_to_bits(&data)), data);
}

#[test]
fn a_trailing_partial_byte_is_dropped() {
    // A truncated transmission must not produce a byte made partly of silence.
    let bits = vec![true; 20];
    assert_eq!(bits_to_bytes(&bits).len(), 2, "20 bits is two whole bytes");
}

#[test]
fn the_loop_closes_on_a_clean_path() {
    let data = payload(16);
    let p = AcousticProfile::conservative();
    assert_eq!(
        round_trip(&data, &p, &Impairment::clean(), 0).as_deref(),
        Some(&data[..])
    );
}

#[test]
fn the_loop_closes_with_leading_silence() {
    // A receiver always starts listening before the sender starts sending, so
    // the preamble search has to find a frame that does not begin at sample zero.
    let data = payload(16);
    let p = AcousticProfile::conservative();
    for silence in [1usize, 100, 480, 1000, 4800] {
        assert_eq!(
            round_trip(&data, &p, &Impairment::clean(), silence).as_deref(),
            Some(&data[..]),
            "failed with {silence} samples of leading silence"
        );
    }
}

#[test]
fn the_loop_closes_under_typical_conditions() {
    let data = payload(16);
    let p = AcousticProfile::conservative();
    assert_eq!(
        round_trip(&data, &p, &Impairment::typical(), 500).as_deref(),
        Some(&data[..]),
        "two laptops on a desk should manage a 16 byte frame"
    );
}

/// The Phase 4 criterion: the channel has to work at 10 dB signal-to-noise,
/// which is what a normal room with the speaker at a sensible volume gives.
#[test]
fn the_loop_survives_ten_decibels_of_signal_to_noise() {
    let data = payload(8);
    let p = AcousticProfile::conservative();
    let imp = Impairment::typical().with_snr(10.0);

    let mut ok = 0;
    const RUNS: u64 = 10;
    for seed in 0..RUNS {
        if round_trip(&data, &p, &imp.with_seed(seed + 1), 300).as_deref() == Some(&data[..]) {
            ok += 1;
        }
    }
    assert!(
        ok >= 8,
        "only {ok}/{RUNS} frames survived at 10 dB; the acoustic channel would \
         be useless for acknowledgements"
    );
}

/// Where the channel actually gives out. Not a pass/fail line — the figure that
/// tells calibration what to expect.
#[test]
fn signal_to_noise_sweep() {
    let data = payload(8);
    let p = AcousticProfile::conservative();
    let mut summary = Vec::new();

    for snr in [30.0f32, 20.0, 15.0, 10.0, 6.0, 3.0, 0.0] {
        let imp = Impairment::typical().with_snr(snr);
        let mut ok = 0;
        const RUNS: u64 = 10;
        for seed in 0..RUNS {
            if round_trip(&data, &p, &imp.with_seed(seed + 1), 300).as_deref() == Some(&data[..]) {
                ok += 1;
            }
        }
        summary.push((snr, ok));
    }

    println!("frames delivered out of 10, by signal-to-noise ratio: {summary:?}");

    // Monotonicity is the real invariant: a channel that got better as noise
    // increased would mean the model is wrong somewhere.
    let clean = summary.first().unwrap().1;
    let noisy = summary.last().unwrap().1;
    assert!(
        clean >= noisy,
        "delivery must not improve as noise rises: {summary:?}"
    );
    assert!(
        clean >= 9,
        "a clean channel should deliver nearly everything"
    );
}

#[test]
fn clock_drift_is_survivable_over_a_short_frame() {
    // Two devices' clocks are never identical. Over a short frame the symbol
    // boundary slides only slightly; this test pins down that it is tolerated
    // rather than assumed away.
    let data = payload(8);
    let p = AcousticProfile::conservative();
    let imp = Impairment {
        clock_drift: 1e-4,
        ..Impairment::typical()
    };
    assert_eq!(
        round_trip(&data, &p, &imp, 300).as_deref(),
        Some(&data[..]),
        "0.01% clock error over a short frame should be tolerable"
    );
}

#[test]
fn pure_noise_does_not_produce_a_frame() {
    // The most dangerous failure mode: inventing a frame out of room noise. Any
    // payload that emerged here would be garbage presented as data.
    let p = AcousticProfile::conservative();
    let silence = vec![0.0f32; p.samples_per_symbol() * 200];
    let noise = impair(&silence, p.sample_rate, &Impairment::clean().with_snr(0.0));

    // With no signal at all, `impair` scales noise off zero power, so add real
    // noise explicitly.
    let mut rng: u64 = 0x1234_5678;
    let noise: Vec<f32> = noise
        .iter()
        .map(|_| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (((rng >> 32) as u32) as f32 / u32::MAX as f32) - 0.5
        })
        .collect();

    let got = demodulate(&noise, &p, 64);
    assert!(
        got.is_none(),
        "a frame was invented out of pure noise; the preamble threshold is too \
         permissive"
    );
}

#[test]
fn confidence_falls_as_the_channel_worsens() {
    let data = payload(8);
    let p = AcousticProfile::conservative();
    let bits = Framer::encode(&data).unwrap();
    let clean_signal = modulate_frame(&bits, &p);
    let expect = Framer::encoded_bits(data.len());

    let good = demodulate(
        &impair(&clean_signal, p.sample_rate, &Impairment::clean()),
        &p,
        expect,
    )
    .expect("clean path decodes");
    let bad = demodulate(
        &impair(
            &clean_signal,
            p.sample_rate,
            &Impairment::typical().with_snr(6.0),
        ),
        &p,
        expect,
    )
    .expect("noisy path still finds the preamble");

    assert!(
        good.confidence > bad.confidence,
        "confidence must reflect channel quality: {:.3} vs {:.3}",
        good.confidence,
        bad.confidence
    );
}

// --- framing ----------------------------------------------------------------

#[test]
fn framing_round_trips() {
    for n in [0usize, 1, 16, 64, MAX_ACOUSTIC_PAYLOAD] {
        let data = payload(n);
        let bits = Framer::encode(&data).unwrap();
        assert_eq!(bits.len(), Framer::encoded_bits(n));
        assert_eq!(Framer::decode(&bits).unwrap(), data);
    }
}

#[test]
fn an_oversized_payload_is_refused() {
    let data = payload(MAX_ACOUSTIC_PAYLOAD + 1);
    assert_eq!(
        Framer::encode(&data),
        Err(FramingError::PayloadTooLarge {
            got: MAX_ACOUSTIC_PAYLOAD + 1,
            max: MAX_ACOUSTIC_PAYLOAD
        })
    );
}

/// The length field is read before anything else and decides how much is read at
/// all. A corrupt length would desynchronise the receiver for the rest of the
/// stream, which the PDU's own CRC cannot protect against because it is checked
/// afterwards, on data that was already framed wrongly.
#[test]
fn a_corrupt_length_is_caught_by_its_own_checksum() {
    let data = payload(16);
    let mut bits = Framer::encode(&data).unwrap();

    // Flip a bit in the length field.
    bits[3] = !bits[3];
    assert!(matches!(
        Framer::decode_length(&bits),
        Err(FramingError::BadLengthChecksum { .. })
    ));
}

#[test]
fn every_single_bit_flip_in_the_header_is_caught() {
    let data = payload(16);
    let clean = Framer::encode(&data).unwrap();

    for i in 0..Framer::header_bits() {
        let mut corrupt = clean.clone();
        corrupt[i] = !corrupt[i];
        match Framer::decode_length(&corrupt) {
            Err(_) => {}
            Ok(len) => assert_eq!(
                len,
                data.len(),
                "flipping header bit {i} changed the length to {len} without \
                 tripping the checksum"
            ),
        }
    }
}

#[test]
fn a_truncated_frame_is_refused_rather_than_padded() {
    let data = payload(32);
    let bits = Framer::encode(&data).unwrap();
    let short = &bits[..bits.len() - 16];
    assert!(matches!(
        Framer::decode(short),
        Err(FramingError::Truncated { .. })
    ));
}

#[test]
fn too_few_bits_for_a_header_is_refused() {
    assert!(matches!(
        Framer::decode_length(&[true; 4]),
        Err(FramingError::TooShort { got: 4 })
    ));
}

#[test]
fn the_preamble_is_alternating() {
    // Alternating bits produce alternating tones, which is the most distinctive
    // pattern this modulation can make and therefore the easiest to find in
    // noise. If someone "simplified" it to all ones it would become
    // indistinguishable from a steady tone.
    for pair in PREAMBLE.windows(2) {
        assert_ne!(pair[0], pair[1], "the preamble must alternate");
    }
}
