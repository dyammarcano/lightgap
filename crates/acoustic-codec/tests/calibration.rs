//! Acoustic calibration tests.
//!
//! The behaviour worth defending: the calibration is willing to say no. On a lot
//! of real hardware the honest answer is that audio does not work, and a
//! calibration that never returns [`Viability::Unavailable`] is not measuring,
//! it is hoping.

use acoustic_codec::calibration::{
    assign_bands, best_modulation, decide_viability, BandMeasurement, ModulationTest, Viability,
    DIRECTION_GUARD_HZ, GOOD_FRAME_ERROR, MIN_BAND_SNR_DB, MIN_BAND_WIDTH_HZ,
};

fn band(start: f32, end: f32, snr: f32) -> BandMeasurement {
    BandMeasurement {
        start_hz: start,
        end_hz: end,
        noise_floor_db: -60.0,
        tone_db: -60.0 + snr,
    }
}

fn good_test(rate: f32) -> ModulationTest {
    ModulationTest {
        symbol_rate: rate,
        bit_error_rate: 0.001,
        frame_error_rate: 0.01,
        latency_ms: 80.0,
    }
}

// --- band measurement -------------------------------------------------------

#[test]
fn what_makes_a_band_usable_is_the_margin_not_the_level() {
    // A loud tone in a loud band carries no more information than a quiet tone
    // in a quiet one, so the absolute level must not be what decides.
    let quiet = BandMeasurement {
        start_hz: 17_000.0,
        end_hz: 18_000.0,
        noise_floor_db: -80.0,
        tone_db: -60.0,
    };
    let loud = BandMeasurement {
        start_hz: 17_000.0,
        end_hz: 18_000.0,
        noise_floor_db: -30.0,
        tone_db: -10.0,
    };

    assert_eq!(quiet.snr_db(), loud.snr_db());
    assert!(quiet.is_usable() && loud.is_usable());
}

#[test]
fn a_band_below_the_signal_to_noise_floor_is_refused() {
    let marginal = band(17_000.0, 18_000.0, MIN_BAND_SNR_DB - 0.1);
    assert!(!marginal.is_usable());

    let adequate = band(17_000.0, 18_000.0, MIN_BAND_SNR_DB);
    assert!(adequate.is_usable());
}

#[test]
fn a_band_too_narrow_for_two_tones_is_refused() {
    // Two tones need room to be separable; a narrow band with a great signal is
    // still unusable, and saying otherwise would produce a profile whose tones
    // cannot be told apart.
    let narrow = band(17_000.0, 17_000.0 + MIN_BAND_WIDTH_HZ - 1.0, 40.0);
    assert!(!narrow.is_usable(), "excellent signal, no room to modulate");
}

// --- band assignment --------------------------------------------------------

#[test]
fn disjoint_bands_give_full_duplex() {
    let heard_by_follower = vec![band(16_500.0, 17_300.0, 20.0)];
    let heard_by_leader = vec![band(18_000.0, 18_800.0, 20.0)];

    let plan = assign_bands(&heard_by_follower, &heard_by_leader).expect("both directions work");
    assert_eq!(plan.viability, Viability::FullDuplex);
    assert!(plan.disjoint());
}

/// The leak that frequency division exists to avoid. Bands that touch let each
/// side's own transmission bleed into the band it is trying to receive on, and
/// that interference is correlated with what the other side is doing — so it
/// appears precisely when both directions are busy.
#[test]
fn bands_that_touch_are_not_treated_as_disjoint() {
    let heard_by_follower = vec![band(17_000.0, 17_800.0, 20.0)];
    // Starts only 100 Hz above, well inside the guard.
    let heard_by_leader = vec![band(17_900.0, 18_700.0, 20.0)];

    let plan = assign_bands(&heard_by_follower, &heard_by_leader).expect("both work");
    assert_eq!(
        plan.viability,
        Viability::HalfDuplex,
        "without a real guard between them the two directions must take turns"
    );
}

#[test]
fn the_guard_is_what_separates_full_from_half_duplex() {
    let lower = band(17_000.0, 17_800.0, 20.0);
    let just_too_close = band(
        17_800.0 + DIRECTION_GUARD_HZ - 1.0,
        18_600.0 + DIRECTION_GUARD_HZ,
        20.0,
    );
    let just_far_enough = band(
        17_800.0 + DIRECTION_GUARD_HZ,
        18_600.0 + DIRECTION_GUARD_HZ,
        20.0,
    );

    assert_eq!(
        assign_bands(&[lower], &[just_too_close]).unwrap().viability,
        Viability::HalfDuplex
    );
    assert_eq!(
        assign_bands(&[lower], &[just_far_enough])
            .unwrap()
            .viability,
        Viability::FullDuplex
    );
}

/// Simultaneity roughly doubles the useful rate, which is worth more than a few
/// decibels on one direction. So a disjoint pair is preferred even when a
/// higher-quality overlapping pair exists.
#[test]
fn disjointness_is_preferred_over_raw_quality() {
    let heard_by_follower = vec![
        band(17_500.0, 18_300.0, 35.0), // excellent, but overlaps the other side
        band(16_400.0, 17_200.0, 15.0), // mediocre, but clear of it
    ];
    let heard_by_leader = vec![band(18_000.0, 18_800.0, 30.0)];

    let plan = assign_bands(&heard_by_follower, &heard_by_leader).expect("works");
    assert_eq!(plan.viability, Viability::FullDuplex);
    assert_eq!(
        plan.leader_tx.start_hz, 16_400.0,
        "the mediocre but clear band should win, because simultaneity is worth \
         more than the decibels given up"
    );
}

#[test]
fn one_direction_only_is_control_only() {
    let heard_by_follower = vec![band(17_000.0, 17_800.0, 20.0)];
    let heard_by_leader: Vec<BandMeasurement> = vec![band(17_000.0, 17_800.0, 2.0)];

    let plan = assign_bands(&heard_by_follower, &heard_by_leader).expect("one direction works");
    assert_eq!(plan.viability, Viability::ControlOnly);
}

/// The outcome the design has to be comfortable with. On plenty of hardware the
/// operating system's echo cancellation and noise suppression remove everything
/// near this band, and a calibration that cannot say so is not measuring.
#[test]
fn no_usable_band_anywhere_yields_nothing() {
    let dead = vec![band(17_000.0, 17_800.0, 1.0), band(18_000.0, 18_800.0, 0.5)];
    assert_eq!(
        assign_bands(&dead, &dead),
        None,
        "the calibration has to be willing to say audio does not work here"
    );
}

#[test]
fn no_bands_measured_at_all_yields_nothing() {
    assert_eq!(assign_bands(&[], &[]), None);
}

/// The same mistake as sizing a QR code from your own camera: what constrains
/// what I may transmit is what the PEER heard, not what I heard.
#[test]
fn each_direction_is_constrained_by_what_the_peer_heard() {
    // The follower hears the leader well in the low band; the leader hears the
    // follower well in the high band.
    let heard_by_follower = vec![
        band(16_500.0, 17_300.0, 25.0),
        band(18_500.0, 19_300.0, 2.0),
    ];
    let heard_by_leader = vec![
        band(16_500.0, 17_300.0, 2.0),
        band(18_500.0, 19_300.0, 25.0),
    ];

    let plan = assign_bands(&heard_by_follower, &heard_by_leader).expect("works");
    assert_eq!(
        plan.leader_tx.start_hz, 16_500.0,
        "the leader must transmit where the FOLLOWER can hear"
    );
    assert_eq!(
        plan.follower_tx.start_hz, 18_500.0,
        "the follower must transmit where the LEADER can hear"
    );
}

#[test]
fn the_derived_profile_keeps_its_tones_away_from_the_band_edges() {
    let heard_by_follower = vec![band(16_500.0, 17_300.0, 20.0)];
    let heard_by_leader = vec![band(18_000.0, 18_800.0, 20.0)];
    let plan = assign_bands(&heard_by_follower, &heard_by_leader).unwrap();

    let p = plan.profile_for(&plan.leader_tx, 48_000, 100.0);
    assert!(
        p.f0 > plan.leader_tx.start_hz && p.f1 < plan.leader_tx.end_hz,
        "both tones must sit inside the band, clear of the edges where filter \
         roll-off bites and the neighbouring direction leaks in"
    );
    assert!(p.is_resolvable(), "the derived profile must be usable");
    assert!(p.within_nyquist());
}

// --- modulation selection ---------------------------------------------------

#[test]
fn the_fastest_rate_that_still_delivers_wins() {
    // Optimising for reliability past the usability floor trades throughput for
    // robustness the layer above does not need, since it already retries.
    let tests = vec![
        ModulationTest {
            symbol_rate: 50.0,
            frame_error_rate: 0.001,
            ..good_test(50.0)
        },
        ModulationTest {
            symbol_rate: 100.0,
            frame_error_rate: 0.02,
            ..good_test(100.0)
        },
        ModulationTest {
            symbol_rate: 200.0,
            frame_error_rate: 0.60,
            ..good_test(200.0)
        },
    ];

    let best = best_modulation(&tests).expect("two of them are usable");
    assert_eq!(
        best.symbol_rate, 100.0,
        "the 200 baud option is unusable and the 50 baud one is slower for no \
         benefit the layer above can use"
    );
}

#[test]
fn no_usable_modulation_yields_nothing() {
    let hopeless = vec![ModulationTest {
        frame_error_rate: 0.9,
        ..good_test(100.0)
    }];
    assert_eq!(best_modulation(&hopeless), None);
    assert_eq!(best_modulation(&[]), None);
}

// --- final verdict ----------------------------------------------------------

#[test]
fn a_good_disjoint_link_is_full_duplex() {
    let plan = assign_bands(
        &[band(16_500.0, 17_300.0, 25.0)],
        &[band(18_000.0, 18_800.0, 25.0)],
    )
    .unwrap();
    assert_eq!(
        decide_viability(Some(&plan), Some(&good_test(100.0))),
        Viability::FullDuplex
    );
}

/// A link that works but only just gets demoted rather than trusted with data.
/// Sending bulk over a channel dropping a fifth of its frames costs more in
/// retransmission than it delivers.
#[test]
fn a_marginal_link_is_demoted_to_control_only() {
    let plan = assign_bands(
        &[band(16_500.0, 17_300.0, 25.0)],
        &[band(18_000.0, 18_800.0, 25.0)],
    )
    .unwrap();
    let marginal = ModulationTest {
        frame_error_rate: GOOD_FRAME_ERROR + 0.05,
        ..good_test(100.0)
    };

    assert_eq!(
        decide_viability(Some(&plan), Some(&marginal)),
        Viability::ControlOnly
    );
}

#[test]
fn an_unusable_modulation_makes_the_whole_channel_unavailable() {
    let plan = assign_bands(
        &[band(16_500.0, 17_300.0, 25.0)],
        &[band(18_000.0, 18_800.0, 25.0)],
    )
    .unwrap();
    let hopeless = ModulationTest {
        frame_error_rate: 0.8,
        ..good_test(100.0)
    };

    assert_eq!(
        decide_viability(Some(&plan), Some(&hopeless)),
        Viability::Unavailable,
        "good bands do not rescue a modulation that cannot deliver"
    );
}

#[test]
fn missing_measurements_mean_unavailable_rather_than_optimistic() {
    assert_eq!(decide_viability(None, None), Viability::Unavailable);
    assert_eq!(
        decide_viability(None, Some(&good_test(100.0))),
        Viability::Unavailable
    );
}

#[test]
fn viability_reports_what_the_caller_needs_to_branch_on() {
    assert!(Viability::FullDuplex.simultaneous());
    assert!(!Viability::HalfDuplex.simultaneous());

    assert!(Viability::ControlOnly.usable());
    assert!(!Viability::Unavailable.usable());
}
