//! The whole optical loop, without a camera: bytes to QR to synthetic camera to
//! bytes.
//!
//! This is what turns "hold two laptops face to face" into something that runs
//! in CI. It also allows sweeping conditions — focus, angle, distance, light —
//! systematically, which never happens by hand.

use optical_codec::decode::scan_pdus;
use optical_codec::distort::{capture, Conditions};
use optical_codec::encode::{encode, max_payload, Ecc};
use optical_codec::geometry::{advise, Advice, MIN_PIXELS_PER_MODULE};
use optical_codec::scan_greyscale;
use optical_protocol::wire::{Flags, Pdu, PduKind};

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

fn pdu(n: usize) -> Pdu {
    Pdu {
        session_id: 0x0123_4567_89ab_cdef,
        kind: PduKind::Data,
        flags: Flags::FOUNTAIN,
        seq: 4242,
        ack: 0,
        payload: payload(n),
    }
}

/// Round trip through the optical loop. Returns the payload read, if any.
fn round_trip(data: &[u8], ecc: Ecc, cond: &Conditions) -> Option<Vec<u8>> {
    let modules = encode(data, ecc).ok()?;
    let (w, h, px) = capture(&modules, cond);
    let scan = scan_greyscale(w, h, &px);
    scan.detections.first().map(|d| d.payload.clone())
}

#[test]
fn the_loop_closes_under_ideal_conditions() {
    // 200 B at correction Q is 65 modules; at fill 0.7 over 720p that is about
    // 7 px/module. At 400 B it would be 89 modules and 5.2 px/module, below the
    // measured threshold — and it would fail for good reason, not because of a
    // codec defect.
    let data = payload(200);
    assert_eq!(
        round_trip(&data, Ecc::Q, &Conditions::ideal()).as_deref(),
        Some(&data[..]),
        "head-on, focused and noise-free it must read exactly"
    );
}

#[test]
fn the_loop_closes_under_typical_conditions() {
    let data = payload(200);
    assert_eq!(
        round_trip(&data, Ecc::Q, &Conditions::typical()).as_deref(),
        Some(&data[..]),
        "a decent webcam on a desk must read without trouble"
    );
}

/// The case that genuinely matters: bad but plausible conditions.
///
/// No specific payload is asserted, only that **usable capacity remains**. How
/// much exactly depends on the pixels-per-module threshold, and pinning a number
/// here would make it a magic constant that breaks whenever the codec is
/// touched. What must not happen is that nothing fits at all in harsh
/// conditions.
#[test]
fn the_loop_survives_harsh_conditions() {
    let mut largest = 0usize;
    for size in [50usize, 100, 150, 200, 300, 400] {
        let data = payload(size);
        if round_trip(&data, Ecc::H, &Conditions::harsh()).as_deref() == Some(&data[..]) {
            largest = size;
        }
    }
    assert!(
        largest >= 50,
        "with high correction something should fit even with unsteady hands and \
         poor light"
    );
    println!("payload readable in harsh conditions (Ecc::H): {largest} B");
}

#[test]
fn a_whole_pdu_survives_the_loop() {
    let original = pdu(150);
    let bytes = original.to_vec().unwrap();
    let modules = encode(&bytes, Ecc::Q).unwrap();
    let (w, h, px) = capture(&modules, &Conditions::typical());

    let (pdus, scan) = scan_pdus(w, h, &px);
    assert_eq!(scan.detections.len(), 1, "exactly one code should be seen");
    assert_eq!(pdus, vec![original], "the PDU must arrive intact");
}

/// The PDU's CRC has to catch whatever the QR error correction cannot fix. If an
/// unreadable frame slipped through as a valid PDU, the file would come out
/// corrupt.
#[test]
fn no_misread_ever_passes_as_a_valid_pdu() {
    let original = pdu(200);
    let bytes = original.to_vec().unwrap();
    let modules = encode(&bytes, Ecc::L).unwrap();

    let mut read = 0;
    let mut seen = 0;

    // Sweep until the link breaks: what is asserted is not that it reads, but
    // that whatever it reads is correct, or nothing is read at all.
    for step in 0..25 {
        let cond = Conditions {
            blur: step as f32 * 0.35,
            noise: step as f32 * 2.0,
            contrast: 1.0 - step as f32 * 0.03,
            fill: 0.6,
            tilt_x: 0.05,
            seed: 1000 + step,
            ..Conditions::default()
        };
        let (w, h, px) = capture(&modules, &cond);
        let (pdus, scan) = scan_pdus(w, h, &px);
        seen += scan.grids_seen();
        for p in &pdus {
            read += 1;
            assert_eq!(
                p, &original,
                "a PDU that passes validation has to be the original (step {step})"
            );
        }
    }

    assert!(seen > 0, "the sweep should have seen codes");
    assert!(read > 0, "and read at least one correctly");
}

#[test]
fn a_code_too_far_away_is_not_read_but_is_flagged() {
    let data = payload(600);
    let modules = encode(&data, Ecc::Q).unwrap();

    // Filling 12% of the frame, a dense code falls below the pixels-per-module
    // minimum.
    let cond = Conditions {
        fill: 0.12,
        ..Conditions::ideal()
    };
    let (w, h, px) = capture(&modules, &cond);
    let scan = scan_greyscale(w, h, &px);

    assert!(
        scan.detections.is_empty(),
        "at this distance it should not be readable"
    );
    if let Some(g) = scan.best_geometry() {
        assert!(
            g.pixels_per_module < MIN_PIXELS_PER_MODULE,
            "if the grid was seen, it must fall below the minimum"
        );
    }
}

#[test]
fn the_geometry_reflects_the_framing() {
    let data = payload(200);
    let modules = encode(&data, Ecc::Q).unwrap();

    let near = Conditions {
        fill: 0.8,
        ..Conditions::ideal()
    };
    let far = Conditions {
        fill: 0.5,
        ..Conditions::ideal()
    };

    let (w1, h1, p1) = capture(&modules, &near);
    let (w2, h2, p2) = capture(&modules, &far);

    let g1 = scan_greyscale(w1, h1, &p1).best_geometry().expect("seen");
    let g2 = scan_greyscale(w2, h2, &p2).best_geometry().expect("seen");

    assert!(
        g1.frame_coverage > g2.frame_coverage,
        "closer must occupy more of the frame"
    );
    assert!(
        g1.pixels_per_module > g2.pixels_per_module,
        "closer must give more pixels per module"
    );
    assert!(
        g1.side_px > g2.side_px,
        "closer must measure a longer side in pixels"
    );
}

#[test]
fn being_off_centre_is_detected() {
    let data = payload(200);
    let modules = encode(&data, Ecc::Q).unwrap();

    let centred = Conditions {
        fill: 0.7,
        ..Conditions::ideal()
    };
    let skewed = Conditions {
        fill: 0.7,
        offset_x: 0.2,
        ..Conditions::ideal()
    };

    let (w1, h1, p1) = capture(&modules, &centred);
    let (w2, h2, p2) = capture(&modules, &skewed);

    let g1 = scan_greyscale(w1, h1, &p1).best_geometry().unwrap();
    let g2 = scan_greyscale(w2, h2, &p2).best_geometry().unwrap();

    assert!(g1.offset < 0.1, "centred should give a low displacement");
    assert!(g2.offset > g1.offset, "being off centre should show up");
}

#[test]
fn blur_lowers_the_measured_sharpness() {
    let data = payload(200);
    let modules = encode(&data, Ecc::Q).unwrap();

    let sharp = Conditions {
        fill: 0.6,
        ..Conditions::ideal()
    };
    let blurry = Conditions {
        fill: 0.6,
        blur: 3.0,
        ..Conditions::ideal()
    };

    let (w1, h1, p1) = capture(&modules, &sharp);
    let (w2, h2, p2) = capture(&modules, &blurry);

    let s1 = scan_greyscale(w1, h1, &p1).sharpness;
    let s2 = scan_greyscale(w2, h2, &p2).sharpness;

    assert!(
        s1 > s2 * 2.0,
        "the Laplacian variance must collapse when defocused: {s1:.1} vs {s2:.1}"
    );
}

#[test]
fn the_advice_prioritises_what_actually_blocks() {
    let data = payload(200);
    let modules = encode(&data, Ecc::Q).unwrap();

    // Far away: however well centred and focused, without pixels per module
    // there is nothing to be done, and that must be the advice given.
    let far = Conditions {
        fill: 0.15,
        ..Conditions::ideal()
    };
    let (w, h, px) = capture(&modules, &far);
    let scan = scan_greyscale(w, h, &px);
    if let Some(g) = scan.best_geometry() {
        assert_eq!(advise(&g, scan.sharpness), Advice::MoveCloser);
    }

    // Good framing: no complaints.
    let good = Conditions {
        fill: 0.7,
        ..Conditions::ideal()
    };
    let (w, h, px) = capture(&modules, &good);
    let scan = scan_greyscale(w, h, &px);
    let g = scan.best_geometry().expect("seen");
    assert_eq!(advise(&g, scan.sharpness), Advice::Ok);
}

#[test]
fn each_correction_level_reduces_capacity() {
    let caps: Vec<usize> = Ecc::all().iter().map(|e| max_payload(*e)).collect();
    for pair in caps.windows(2) {
        assert!(
            pair[0] > pair[1],
            "more correction must leave less room: {caps:?}"
        );
    }
    // Reference from the standard: version 40 in byte mode at correction L.
    assert_eq!(caps[0], 2953);
}

#[test]
fn a_payload_that_does_not_fit_is_rejected_at_encode_time() {
    // Same kind of filler the bound was measured with: content containing runs
    // of ASCII digits enters numeric mode and fits more, so comparing against
    // other content would prove nothing.
    let too_big = vec![0u8; max_payload(Ecc::H) + 1];
    assert!(encode(&too_big, Ecc::H).is_err());
    assert!(encode(&vec![0u8; max_payload(Ecc::H)], Ecc::H).is_ok());
}

#[test]
fn a_frame_with_no_code_invents_no_detections() {
    let (w, h) = (640usize, 480usize);
    let flat = vec![200u8; w * h];
    let scan = scan_greyscale(w, h, &flat);

    assert_eq!(scan.grids_seen(), 0);
    assert_eq!(scan.best_geometry(), None);
}

#[test]
fn an_empty_or_inconsistent_frame_does_not_panic() {
    assert_eq!(scan_greyscale(0, 0, &[]).grids_seen(), 0);
    assert_eq!(scan_greyscale(10, 10, &[0u8; 5]).grids_seen(), 0);
    assert_eq!(scan_greyscale(100, 0, &[0u8; 10]).grids_seen(), 0);
}

/// Systematic sweep: how much density each correction level tolerates under
/// typical conditions. This is the kind of figure calibration will measure live,
/// and having it in CI catches codec regressions.
#[test]
fn density_sweep_per_correction_level() {
    let mut summary = Vec::new();

    for ecc in Ecc::all() {
        let mut largest = 0usize;
        for size in (100..=1600).step_by(100) {
            let bytes = payload(size);
            if encode(&bytes, ecc).is_err() {
                break;
            }
            let cond = Conditions {
                fill: 0.8,
                ..Conditions::typical()
            };
            if round_trip(&bytes, ecc, &cond).as_deref() == Some(&bytes[..]) {
                largest = size;
            }
        }
        summary.push((ecc, largest));
        assert!(
            largest >= 100,
            "at {ecc:?} at least 100 B should read under typical conditions"
        );
    }

    println!("largest payload readable under typical conditions: {summary:?}");
}

/// How much payload **reliably** fits per frame at 720p.
///
/// "Reliably" rather than "ever": at 3.3 px/module a frame decodes sometimes,
/// and taking that number would mean negotiating a profile that fails one frame
/// in four. Here every repetition has to succeed, which is the criterion
/// calibration has to choose by.
///
/// Having it in CI turns a codec regression — or a change to the
/// pixels-per-module threshold — into a failure with a number attached, rather
/// than into "it got slower".
#[test]
fn reliable_capacity_per_frame_at_720p() {
    const REPEATS: u64 = 4;

    let mut summary = Vec::new();
    for ecc in Ecc::all() {
        let mut largest = 0usize;
        for size in (100..=1400).step_by(100) {
            let data = payload(size);
            if encode(&data, ecc).is_err() {
                break;
            }
            // Vary the seed and nudge the framing: a profile that only works
            // with a perfectly steady hand is not usable.
            let reliable = (0..REPEATS).all(|r| {
                let cond = Conditions {
                    fill: 0.75 - r as f32 * 0.01,
                    noise: 2.0,
                    seed: 7000 + r,
                    ..Conditions::ideal()
                };
                round_trip(&data, ecc, &cond).as_deref() == Some(&data[..])
            });
            if reliable {
                largest = size;
            } else {
                break;
            }
        }
        summary.push((ecc, largest));
    }

    println!("reliable payload per frame at 720p, fill ~0.75: {summary:?}");

    let h = summary.iter().find(|(e, _)| *e == Ecc::H).unwrap().1;
    assert!(h >= 100, "only {h} B fit reliably per frame at Ecc::H");

    let l = summary.iter().find(|(e, _)| *e == Ecc::L).unwrap().1;
    assert!(l >= h, "L={l} should accept at least as much as H={h}");
}
