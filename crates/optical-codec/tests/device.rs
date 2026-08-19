//! Tests for form-factor-aware profile selection.
//!
//! The point of these is that mixed pairings — laptop with phone, phone with
//! phone — get a profile matched to the hardware actually present, rather than
//! one number chosen for the average case and wrong for all of them.

use optical_codec::device::{
    capacity, modules_for_version, pair_profile, suggest_best_profile, suggest_profile, FormFactor,
    VisualCapabilities, EXPECTED_FRAME_FILL,
};
use optical_codec::encode::{encode, Ecc};
use optical_codec::geometry::MIN_PIXELS_PER_MODULE;

fn laptop() -> VisualCapabilities {
    VisualCapabilities::typical(FormFactor::Laptop)
}

fn phone() -> VisualCapabilities {
    VisualCapabilities::typical(FormFactor::Phone)
}

fn desktop() -> VisualCapabilities {
    VisualCapabilities::typical(FormFactor::Desktop)
}

#[test]
fn no_device_can_see_its_own_display() {
    // Stated as a test because it is the constraint people keep trying to design
    // around, and because it is why every pairing needs two physical devices.
    for ff in [
        FormFactor::Desktop,
        FormFactor::Laptop,
        FormFactor::Tablet,
        FormFactor::Phone,
    ] {
        assert!(!ff.can_see_own_display());
    }
}

#[test]
fn the_profile_is_set_by_my_display_and_the_peers_camera() {
    // Same sender, two different receivers: only the receiver's camera changes,
    // and the profile must change with it.
    let good_camera = VisualCapabilities {
        camera_px: (3840, 2160),
        ..laptop()
    };
    let poor_camera = VisualCapabilities {
        camera_px: (640, 480),
        ..laptop()
    };

    let to_good = suggest_profile(&laptop(), &good_camera, Ecc::Q).expect("should resolve");
    let to_poor = suggest_profile(&laptop(), &poor_camera, Ecc::Q).expect("should resolve");

    assert!(
        to_good.modules > to_poor.modules,
        "a better peer camera must allow a denser code: {} vs {}",
        to_good.modules,
        to_poor.modules
    );
    assert!(to_good.payload_bytes > to_poor.payload_bytes);
}

#[test]
fn my_own_camera_does_not_affect_what_i_transmit() {
    // The mistake this guards against produces a link that is mysteriously worse
    // in one direction, and it is an easy one to make.
    let with_good_camera = VisualCapabilities {
        camera_px: (3840, 2160),
        ..laptop()
    };
    let with_poor_camera = VisualCapabilities {
        camera_px: (640, 480),
        ..laptop()
    };

    let a = suggest_profile(&with_good_camera, &phone(), Ecc::Q);
    let b = suggest_profile(&with_poor_camera, &phone(), Ecc::Q);

    assert_eq!(
        a, b,
        "my own camera constrains what I can receive, not what I send"
    );
}

#[test]
fn a_bigger_display_allows_more_only_up_to_what_the_peer_resolves() {
    // Past the point where the peer's camera is the binding constraint, a bigger
    // display buys nothing. Believing otherwise would have a desktop pushing a
    // density its peer cannot read.
    let small_display = VisualCapabilities {
        display_px: (400, 400),
        ..laptop()
    };
    let big_display = VisualCapabilities {
        display_px: (3840, 2160),
        ..laptop()
    };

    let from_small = suggest_profile(&small_display, &phone(), Ecc::Q).expect("resolves");
    let from_big = suggest_profile(&big_display, &phone(), Ecc::Q).expect("resolves");

    assert!(from_big.modules >= from_small.modules);
    assert_eq!(
        from_big.modules,
        suggest_profile(&desktop(), &phone(), Ecc::Q)
            .unwrap()
            .modules,
        "once the peer's camera binds, extra display area changes nothing"
    );
}

#[test]
fn a_camera_too_poor_to_resolve_anything_yields_no_profile() {
    // A tiny camera cannot resolve even the smallest code at the expected
    // framing. Answering with a profile anyway would start a session that can
    // never work.
    let tiny_camera = VisualCapabilities {
        camera_px: (120, 90),
        ..laptop()
    };
    assert_eq!(
        suggest_profile(&laptop(), &tiny_camera, Ecc::Q),
        None,
        "a link that cannot carry the smallest code must say so"
    );
}

#[test]
fn the_expected_pixels_per_module_clears_the_measured_threshold() {
    // The whole point of sizing from the peer's camera is landing above the
    // threshold that was measured, not the one the standard quotes.
    for (mine, peer) in [
        (laptop(), phone()),
        (phone(), laptop()),
        (phone(), phone()),
        (laptop(), laptop()),
        (desktop(), phone()),
    ] {
        let p = suggest_best_profile(&mine, &peer).expect("these pairings should resolve");
        assert!(
            p.expected_pixels_per_module >= MIN_PIXELS_PER_MODULE,
            "{:?} to {:?} expects {:.2} px/module, below the measured threshold of {MIN_PIXELS_PER_MODULE}",
            mine.form_factor,
            peer.form_factor,
            p.expected_pixels_per_module
        );
    }
}

#[test]
fn every_pairing_is_usable() {
    let factors = [
        FormFactor::Desktop,
        FormFactor::Laptop,
        FormFactor::Tablet,
        FormFactor::Phone,
    ];
    for a in factors {
        for b in factors {
            let pp = pair_profile(
                &VisualCapabilities::typical(a),
                &VisualCapabilities::typical(b),
            );
            assert!(
                pp.bidirectional(),
                "{a:?} paired with {b:?} should work in both directions"
            );
        }
    }
}

/// The design claim that motivated mobile support at all, checked numerically.
#[test]
fn a_phone_camera_beats_a_laptop_webcam_for_receiving() {
    let laptop_to_phone = suggest_best_profile(&laptop(), &phone()).expect("resolves");
    let laptop_to_laptop = suggest_best_profile(&laptop(), &laptop()).expect("resolves");

    assert!(
        laptop_to_phone.payload_bytes > laptop_to_laptop.payload_bytes,
        "the same laptop display should push more to a phone ({} B) than to \
         another laptop ({} B), because the phone camera resolves more",
        laptop_to_phone.payload_bytes,
        laptop_to_laptop.payload_bytes
    );
}

#[test]
fn a_pairing_can_be_asymmetric_in_capacity() {
    // A laptop and a phone are unequal in opposite ways: big display versus good
    // camera. The two directions should not come out the same, and pretending
    // they do would waste whichever advantage each side has.
    let pp = pair_profile(&laptop(), &phone());
    let a = pp.a_to_b.expect("laptop to phone");
    let b = pp.b_to_a.expect("phone to laptop");

    assert_ne!(
        a.payload_bytes, b.payload_bytes,
        "the two directions of a laptop-phone pairing should differ"
    );
}

#[test]
fn capacity_never_exceeds_what_that_many_modules_can_hold() {
    // The capacity probe has to reject payloads that only encoded by silently
    // bumping to a larger version than we budgeted for.
    for version in [1u8, 5, 10, 20, 40] {
        let modules = modules_for_version(version);
        for ecc in Ecc::all() {
            let cap = capacity(version, ecc);
            if cap == 0 {
                continue;
            }
            let m = encode(&vec![0u8; cap], ecc).expect("the measured capacity must encode");
            assert!(
                m.size() as u32 <= modules,
                "version {version} at {ecc:?}: {cap} B produced {} modules, over the \
                 {modules} budgeted",
                m.size()
            );
        }
    }
}

#[test]
fn version_and_module_count_agree() {
    assert_eq!(modules_for_version(1), 21);
    assert_eq!(modules_for_version(40), 177);
    // Out-of-range versions clamp rather than panic: they come from the wire and
    // must not be able to bring the session down.
    assert_eq!(modules_for_version(0), 21);
    assert_eq!(modules_for_version(255), 177);
}

#[test]
fn the_expected_frame_fill_is_a_plausible_framing() {
    // If this drifted to 1.0 every profile would assume perfect framing, and the
    // profiles would all be too dense to read in practice.
    assert!(
        (0.5..=0.85).contains(&EXPECTED_FRAME_FILL),
        "an expected fill of {EXPECTED_FRAME_FILL} does not describe a framing \
         people actually achieve"
    );
}

/// Prints the table for every pairing. Not an assertion — it is the figure that
/// tells a user what to expect before they start holding two devices up.
#[test]
fn pairing_capacity_table() {
    let factors = [
        FormFactor::Desktop,
        FormFactor::Laptop,
        FormFactor::Tablet,
        FormFactor::Phone,
    ];
    println!("sender -> receiver : modules, ecc, bytes/frame, expected px/module");
    for a in factors {
        for b in factors {
            let ca = VisualCapabilities::typical(a);
            let cb = VisualCapabilities::typical(b);
            match suggest_best_profile(&ca, &cb) {
                Some(p) => println!(
                    "{a:?} -> {b:?}: {} modules, {:?}, {} B, {:.1} px/mod",
                    p.modules, p.ecc, p.payload_bytes, p.expected_pixels_per_module
                ),
                None => println!("{a:?} -> {b:?}: not viable"),
            }
        }
    }
}
