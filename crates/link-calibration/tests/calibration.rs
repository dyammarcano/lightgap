//! Tests for profile negotiation and adjustment.

use std::time::Duration;

use link_calibration::adaptive::{Adaptation, Aimd, GOOD_STREAK_TO_INCREASE};
use link_calibration::ladder::{Ladder, Phase, DEFAULT_MARGIN_PCT};
use link_calibration::lifecycle::{
    Lifecycle, LinkState, Transition, DEGRADE_DEBOUNCE, SILENCE_TO_DOWN,
};
use link_calibration::scoring::{best, Measurement};

/// A fake link with a known ceiling: above `ceiling` it reads nothing.
fn rate(value: u32, ceiling: u32) -> f32 {
    if value <= ceiling {
        1.0
    } else {
        0.0
    }
}

fn resolve(ceiling: u32, min: u32, max: u32, start: u32) -> Option<u32> {
    let mut l = Ladder::new(min, max, start);
    let mut rounds = 0;
    while l.phase() != Phase::Settled {
        rounds += 1;
        assert!(rounds < 200, "the ladder does not converge");
        let t = rate(l.current(), ceiling);
        l.record(t);
    }
    l.settled()
}

#[test]
fn the_ladder_finds_the_ceiling_with_a_margin() {
    // Ceiling 1000 with a 15% margin: expect something near 850, and never above
    // the ceiling.
    let r = resolve(1000, 64, 4096, 128).expect("should find something");
    assert!(r <= 1000, "result {r} cannot exceed the ceiling");
    assert!(r >= 500, "result {r} is needlessly conservative");
}

#[test]
fn the_margin_leaves_room_for_the_link_to_worsen() {
    let mut l = Ladder::new(64, 4096, 128);
    while l.phase() != Phase::Settled {
        l.record(rate(l.current(), 1000));
    }
    let chosen = l.settled().unwrap();
    // The margin has to be real: operating at the exact limit means falling over
    // the moment somebody shifts a laptop.
    let without_margin = 1000;
    let expected_max = without_margin * u32::from(100 - DEFAULT_MARGIN_PCT) / 100;
    assert!(
        chosen <= expected_max,
        "{chosen} does not leave the {DEFAULT_MARGIN_PCT}% margin"
    );
}

#[test]
fn it_converges_for_many_different_ceilings() {
    for ceiling in [70u32, 100, 250, 640, 1000, 2000, 4096] {
        let r = resolve(ceiling, 64, 4096, 128);
        let r = r.unwrap_or_else(|| panic!("did not converge with ceiling {ceiling}"));
        assert!(r <= ceiling, "with ceiling {ceiling} it chose {r}");
        assert!(
            r >= 64,
            "with ceiling {ceiling} it chose {r}, below the minimum"
        );
    }
}

#[test]
fn a_link_that_cannot_manage_the_minimum_returns_no_profile() {
    // Ceiling below the range minimum: there is no viable profile.
    let mut l = Ladder::new(64, 4096, 128);
    while l.phase() != Phase::Settled {
        l.record(rate(l.current(), 10));
    }
    assert_eq!(
        l.settled(),
        None,
        "with no working value, no profile should be invented; the framing is \
         what needs fixing"
    );
}

#[test]
fn a_link_that_sustains_the_maximum_stays_there() {
    let r = resolve(u32::MAX, 64, 4096, 128).expect("should converge");
    let with_margin = 4096 * u32::from(100 - DEFAULT_MARGIN_PCT) / 100;
    assert_eq!(r, with_margin);
}

#[test]
fn the_ladder_does_not_probe_forever() {
    let mut l = Ladder::new(64, 4096, 128);
    while l.phase() != Phase::Settled {
        l.record(rate(l.current(), 977));
    }
    // Doubling to 4096 is about six steps and bisecting another twelve. Nobody
    // holds two laptops face to face for a hundred probes.
    assert!(
        l.probes() <= 20,
        "spent {} probes; calibration would take forever",
        l.probes()
    );
}

#[test]
fn giving_up_keeps_the_best_known_value() {
    let mut l = Ladder::new(64, 4096, 128);
    l.record(1.0); // 128 works
    l.record(1.0); // 256 works
    l.give_up();
    assert_eq!(l.phase(), Phase::Settled);
    let r = l.settled().expect("should keep what it knows");
    assert!((64..=256).contains(&r));
}

// --- scoring ----------------------------------------------------------------

#[test]
fn goodput_is_not_capacity() {
    let large = Measurement {
        payload_bytes: 1500,
        frames_per_second: 5.0,
        success_rate: 0.95,
        retry_rate: 0.05,
        decode_ms: 20.0,
    };
    let medium = Measurement {
        payload_bytes: 900,
        frames_per_second: 12.0,
        success_rate: 0.98,
        retry_rate: 0.02,
        decode_ms: 12.0,
    };

    assert!(
        medium.goodput_bps() > large.goodput_bps(),
        "the medium, faster frame should deliver more than the large, slow one"
    );
    assert!(medium.score() > large.score());
}

#[test]
fn decode_latency_is_penalised() {
    let fast = Measurement {
        payload_bytes: 900,
        frames_per_second: 10.0,
        success_rate: 1.0,
        retry_rate: 0.0,
        decode_ms: 5.0,
    };
    let slow = Measurement {
        decode_ms: 200.0,
        ..fast
    };

    assert_eq!(fast.goodput_bps(), slow.goodput_bps(), "same goodput");
    assert!(
        fast.score() > slow.score() * 2.0,
        "decoding twice as slow as a whole frame must be penalised heavily"
    );
}

#[test]
fn retries_are_penalised_beyond_their_fraction() {
    let clean = Measurement {
        payload_bytes: 900,
        frames_per_second: 10.0,
        success_rate: 1.0,
        retry_rate: 0.0,
        decode_ms: 10.0,
    };
    let with_retries = Measurement {
        retry_rate: 0.3,
        ..clean
    };
    assert!(with_retries.score() < clean.score() * 0.75);
}

#[test]
fn choosing_the_best_discards_anything_delivering_nothing() {
    let dead = Measurement {
        payload_bytes: 2000,
        frames_per_second: 10.0,
        success_rate: 0.0,
        retry_rate: 1.0,
        decode_ms: 5.0,
    };
    let alive = Measurement {
        payload_bytes: 300,
        frames_per_second: 8.0,
        success_rate: 0.99,
        retry_rate: 0.01,
        decode_ms: 8.0,
    };

    let chosen = best(&[("dead", dead), ("alive", alive)]).expect("one is alive");
    assert_eq!(chosen.0, "alive");

    assert_eq!(
        best::<&str>(&[]),
        None,
        "with no candidates none should be invented"
    );
    assert_eq!(
        best(&[("dead", dead)]).map(|(n, _)| n),
        None,
        "a profile delivering nothing is not the least bad, it is a broken link"
    );
}

// --- continuous adjustment --------------------------------------------------

#[test]
fn it_does_not_climb_on_the_first_good_observation() {
    let mut a = Aimd::new(1000, 100, 2000, 64);
    for _ in 0..(GOOD_STREAK_TO_INCREASE - 1) {
        assert_eq!(a.observe(1.0), Adaptation::Hold);
    }
    assert_eq!(a.current(), 1000, "should not have climbed yet");
    assert_eq!(a.observe(1.0), Adaptation::Increase);
    assert_eq!(a.current(), 1064);
}

#[test]
fn a_bad_observation_breaks_the_good_streak() {
    let mut a = Aimd::new(1000, 100, 2000, 64);
    a.observe(1.0);
    a.observe(1.0);
    a.observe(0.96); // acceptable but not excellent
    assert_eq!(a.good_streak(), 0, "the streak must reset");
    assert_eq!(a.observe(1.0), Adaptation::Hold, "counting starts over");
}

#[test]
fn it_backs_off_multiplicatively_when_struggling() {
    let mut a = Aimd::new(1000, 100, 2000, 64);
    assert_eq!(a.observe(0.90), Adaptation::Reduce);
    assert_eq!(a.current(), 700, "times 0.7");
}

#[test]
fn it_collapses_when_the_link_breaks() {
    let mut a = Aimd::new(1000, 100, 2000, 64);
    assert_eq!(a.observe(0.5), Adaptation::Recover);
    assert_eq!(
        a.current(),
        490,
        "below the distress threshold it backs off twice as hard: times 0.49"
    );
}

#[test]
fn climbing_is_slow_and_backing_off_is_fast() {
    // The asymmetry is the whole point of the controller; if it were symmetric,
    // recovering from a degradation would cost as much as causing it.
    let mut a = Aimd::new(1000, 100, 4000, 64);
    for _ in 0..(GOOD_STREAK_TO_INCREASE * 3) {
        a.observe(1.0);
    }
    let climbed = a.current();
    assert_eq!(climbed, 1000 + 64 * 3);

    a.observe(0.5);
    assert!(
        a.current() < 1000,
        "one bad observation must more than undo three climbs"
    );
}

#[test]
fn it_does_not_go_below_the_minimum_nor_lie_about_it() {
    let mut a = Aimd::new(100, 100, 2000, 64);
    assert_eq!(
        a.observe(0.1),
        Adaptation::Recover,
        "a broken link is flagged even when nothing can be cut"
    );
    assert_eq!(a.current(), 100);

    // Mild degradation while already at the minimum: there is nothing to reduce,
    // and saying "Reduce" would mislead the caller.
    assert_eq!(a.observe(0.90), Adaptation::Hold);
    assert_eq!(a.current(), 100);
}

#[test]
fn it_does_not_climb_above_the_maximum() {
    let mut a = Aimd::new(1990, 100, 2000, 64);
    for _ in 0..(GOOD_STREAK_TO_INCREASE * 2) {
        a.observe(1.0);
    }
    assert_eq!(a.current(), 2000);
}

// --- channel lifecycle ------------------------------------------------------

#[test]
fn a_channel_is_born_down() {
    let l = Lifecycle::new();
    assert_eq!(l.state(), LinkState::Down);
    assert!(!l.usable());
}

#[test]
fn the_normal_path_of_a_channel() {
    let mut l = Lifecycle::new();
    assert_eq!(l.start_probing(), Some(Transition::ProbingStarted));
    assert_eq!(l.state(), LinkState::Probing);
    assert!(!l.usable(), "while probing it carries nothing");

    assert_eq!(l.bring_up(), Some(Transition::CameUp));
    assert!(l.usable());
    assert_eq!(
        l.bring_up(),
        None,
        "bringing up twice does not repeat the event"
    );
}

#[test]
fn a_short_stumble_does_not_degrade() {
    let mut l = Lifecycle::new();
    l.start_probing();
    l.bring_up();

    let t0 = Duration::from_secs(1);
    assert_eq!(l.observe(t0, 0.5), None);
    assert_eq!(
        l.observe(t0 + DEGRADE_DEBOUNCE / 2, 0.5),
        None,
        "a patch of bad luck is not degradation"
    );
    assert_eq!(l.state(), LinkState::Up);
}

#[test]
fn sustained_degradation_is_declared() {
    let mut l = Lifecycle::new();
    l.start_probing();
    l.bring_up();

    let t0 = Duration::from_secs(1);
    l.observe(t0, 0.5);
    assert_eq!(
        l.observe(t0 + DEGRADE_DEBOUNCE, 0.5),
        Some(Transition::Degraded)
    );
    assert_eq!(l.state(), LinkState::Degraded);
    assert!(
        l.usable(),
        "degraded still serves while it delivers anything"
    );
}

#[test]
fn the_channel_recovers_on_its_own() {
    let mut l = Lifecycle::new();
    l.start_probing();
    l.bring_up();

    let t0 = Duration::from_secs(1);
    l.observe(t0, 0.5);
    l.observe(t0 + DEGRADE_DEBOUNCE, 0.5);
    assert_eq!(l.state(), LinkState::Degraded);

    assert_eq!(
        l.observe(t0 + DEGRADE_DEBOUNCE * 2, 0.99),
        Some(Transition::Recovered)
    );
    assert_eq!(l.state(), LinkState::Up);
}

#[test]
fn prolonged_silence_takes_the_channel_down() {
    let mut l = Lifecycle::new();
    l.start_probing();
    l.bring_up();
    l.observe(Duration::from_secs(1), 1.0);

    assert_eq!(l.tick(Duration::from_secs(2)), None);
    assert_eq!(
        l.tick(Duration::from_secs(1) + SILENCE_TO_DOWN),
        Some(Transition::WentDown)
    );
    assert_eq!(l.state(), LinkState::Down);
    assert!(!l.usable());
}

#[test]
fn a_down_channel_reacts_to_no_observations() {
    let mut l = Lifecycle::new();
    assert_eq!(l.observe(Duration::from_secs(1), 1.0), None);
    assert_eq!(l.tick(Duration::from_secs(60)), None);
    assert_eq!(l.state(), LinkState::Down);
}

#[test]
fn forcing_down_works_from_any_state() {
    let mut l = Lifecycle::new();
    l.start_probing();
    l.bring_up();
    assert_eq!(l.force_down(), Some(Transition::WentDown));
    assert_eq!(l.force_down(), None, "it does not repeat the event");
}
