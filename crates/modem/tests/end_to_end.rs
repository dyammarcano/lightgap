//! Two modems transferring a real file, end to end, with no hardware.
//!
//! This is the test the whole sans-io discipline was for. Every layer is
//! exercised — discovery, leader election, metadata exchange, fountain coding,
//! feedback, hash verification — and the frames travel through either a lossy
//! simulated channel or, in the optical tests, an actual QR encode/decode round
//! trip through a synthetic camera.
//!
//! A failure here is reproducible on a laptop in a second. The same failure
//! found by holding two devices up would take an afternoon to characterise.

use std::time::Duration;

use channel_sim::{LinkConfig, SimPair};
use modem::{Event, Modem};
use optical_codec::distort::{capture, Conditions};
use optical_codec::encode::{encode, Ecc};
use optical_codec::scan_greyscale;
use optical_protocol::channel::Channel;
use optical_protocol::session::{PeerId, Role, State};

/// A frame's worth of bytes. Chosen to match a QR code that a 1080p camera
/// resolves reliably at a sensible framing.
const MTU: usize = 900;
/// Virtual milliseconds per frame, roughly one optical frame.
const TICK_MS: u64 = 80;

fn peer(n: u8) -> PeerId {
    let mut b = [0u8; 16];
    b[0] = n;
    PeerId::from_bytes(b)
}

fn file(len: usize) -> Vec<u8> {
    // Deterministic pseudo-random with no short period: a repeating pattern
    // could hide a chunk landing at the wrong offset.
    let mut out = Vec::with_capacity(len);
    let mut x: u64 = 0x2545_f491_4f6c_dd1d;
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// What a completed run produced.
struct Outcome {
    received: Option<(String, Vec<u8>)>,
    corrupt: bool,
    ticks: u64,
    sender_saw_completion: bool,
}

/// Drives two modems over the simulated channel until the file lands.
fn run_over_channel(a: &mut Modem, b: &mut Modem, link: &mut SimPair, max_ticks: u64) -> Outcome {
    let mut now = Duration::ZERO;
    let mut ticks = 0u64;
    let mut received = None;
    let mut corrupt = false;
    let mut sender_saw_completion = false;

    loop {
        ticks += 1;
        assert!(
            ticks <= max_ticks,
            "did not converge in {max_ticks} ticks (a={:?} b={:?}, rx {:.3})",
            a.state(),
            b.state(),
            b.receive_progress()
        );
        now += Duration::from_millis(TICK_MS);
        link.advance(now);

        for e in a.tick(now) {
            if e == Event::SendComplete {
                sender_saw_completion = true;
            }
        }
        b.tick(now);

        if let Some(frame) = a.poll_frame() {
            link.a.send_frame(&frame).expect("fits the MTU");
        }
        if let Some(frame) = b.poll_frame() {
            link.b.send_frame(&frame).expect("fits the MTU");
        }

        while let Some(frame) = link.b.recv_frame() {
            for e in b.handle_frame(&frame) {
                match e {
                    Event::FileReceived { name, bytes } => received = Some((name, bytes)),
                    Event::FileCorrupt { .. } => corrupt = true,
                    _ => {}
                }
            }
        }
        while let Some(frame) = link.a.recv_frame() {
            for e in a.handle_frame(&frame) {
                if e == Event::SendComplete {
                    sender_saw_completion = true;
                }
            }
        }

        if received.is_some() || corrupt {
            // Keep running briefly so the sender learns it can stop.
            for _ in 0..20 {
                now += Duration::from_millis(TICK_MS);
                link.advance(now);
                if let Some(frame) = b.poll_frame() {
                    let _ = link.b.send_frame(&frame);
                }
                while let Some(frame) = link.a.recv_frame() {
                    for e in a.handle_frame(&frame) {
                        if e == Event::SendComplete {
                            sender_saw_completion = true;
                        }
                    }
                }
            }
            return Outcome {
                received,
                corrupt,
                ticks,
                sender_saw_completion,
            };
        }
    }
}

#[test]
fn two_modems_transfer_a_file_over_a_clean_link() {
    let original = file(20_000);
    let mut a = Modem::new(peer(1), MTU);
    let mut b = Modem::new(peer(9), MTU);
    let mut link = SimPair::new(LinkConfig::perfect(MTU), 1);

    a.send_file("secret.key", original.clone());
    let out = run_over_channel(&mut a, &mut b, &mut link, 20_000);

    let (name, bytes) = out.received.expect("the file should arrive");
    assert_eq!(name, "secret.key", "the name travels with the file");
    assert_eq!(bytes, original, "byte for byte");
    assert!(!out.corrupt);
    assert!(
        out.sender_saw_completion,
        "the sender has to learn it can stop, or it emits forever"
    );
}

/// The criterion that matters: a real optical link loses frames constantly.
#[test]
fn two_modems_transfer_a_file_at_forty_percent_loss() {
    let original = file(20_000);
    let mut a = Modem::new(peer(1), MTU);
    let mut b = Modem::new(peer(9), MTU);
    let mut link = SimPair::new(LinkConfig::optical(MTU, 0.40), 20_260_819);

    let out = {
        a.send_file("config.toml", original.clone());
        run_over_channel(&mut a, &mut b, &mut link, 50_000)
    };

    let (name, bytes) = out.received.expect("the file should still arrive");
    assert_eq!(name, "config.toml");
    assert_eq!(bytes, original);
    println!("40% loss: converged in {} ticks", out.ticks);
}

/// Corruption that the CRC catches must not reach reassembly, and the transfer
/// has to survive it. This is the blurry-QR case.
#[test]
fn a_transfer_survives_corruption_on_top_of_loss() {
    let original = file(20_000);
    let mut a = Modem::new(peer(1), MTU);
    let mut b = Modem::new(peer(9), MTU);
    let cfg = LinkConfig::optical(MTU, 0.15).with_corruption(0.15);
    let mut link = SimPair::new(cfg, 99);

    a.send_file("notes.txt", original.clone());
    let out = run_over_channel(&mut a, &mut b, &mut link, 50_000);

    let (_, bytes) = out.received.expect("the file should arrive");
    assert_eq!(bytes, original);
    // Asserted against the modem's own counter rather than the channel's: the
    // channel hands over bytes and only the wire layer can say whether they were
    // a valid PDU, so the modem is the only party that knows.
    assert!(
        b.stats().frames_rejected > 0,
        "the test should have exercised CRC rejection, saw {:?}",
        b.stats()
    );
}

/// The metadata frame is the one the whole transfer depends on. Losing it once
/// must not lose the transfer, which is why it repeats until acknowledged.
#[test]
fn a_transfer_survives_losing_the_announcement() {
    let original = file(10_000);
    let mut a = Modem::new(peer(1), MTU);
    let mut b = Modem::new(peer(9), MTU);
    // Brutal loss in the direction the announcement travels.
    let cfg = LinkConfig::optical(MTU, 0.70);
    let mut link = SimPair::new(cfg, 7);

    a.send_file("key.pem", original.clone());
    let out = run_over_channel(&mut a, &mut b, &mut link, 80_000);

    assert_eq!(out.received.expect("arrives eventually").1, original);
}

#[test]
fn an_empty_file_transfers() {
    // Zero-length files are legitimate, and they are the case that panicked
    // RaptorQ before it was guarded.
    let mut a = Modem::new(peer(1), MTU);
    let mut b = Modem::new(peer(9), MTU);
    let mut link = SimPair::new(LinkConfig::perfect(MTU), 1);

    a.send_file("empty", Vec::new());
    let out = run_over_channel(&mut a, &mut b, &mut link, 5_000);

    let (name, bytes) = out.received.expect("an empty file still arrives");
    assert_eq!(name, "empty");
    assert!(bytes.is_empty());
}

#[test]
fn the_peers_find_each_other_and_agree_on_roles() {
    let mut a = Modem::new(peer(1), MTU);
    let mut b = Modem::new(peer(9), MTU);
    let mut link = SimPair::new(LinkConfig::perfect(MTU), 1);

    let mut now = Duration::ZERO;
    let mut a_role = None;
    let mut b_role = None;

    for _ in 0..50 {
        now += Duration::from_millis(TICK_MS);
        link.advance(now);
        a.tick(now);
        b.tick(now);

        if let Some(f) = a.poll_frame() {
            let _ = link.a.send_frame(&f);
        }
        if let Some(f) = b.poll_frame() {
            let _ = link.b.send_frame(&f);
        }
        while let Some(f) = link.b.recv_frame() {
            for e in b.handle_frame(&f) {
                if let Event::PeerFound { role, .. } = e {
                    b_role = Some(role);
                }
            }
        }
        while let Some(f) = link.a.recv_frame() {
            for e in a.handle_frame(&f) {
                if let Event::PeerFound { role, .. } = e {
                    a_role = Some(role);
                }
            }
        }
    }

    assert_eq!(a_role, Some(Role::Leader), "the lower peer id leads");
    assert_eq!(b_role, Some(Role::Follower));
    assert_eq!(a.state(), State::Peered);
    assert_eq!(b.state(), State::Peered);
}

/// The full optical loop: no simulated channel at all. Frames become real QR
/// codes, get photographed by the synthetic camera, and are decoded back.
///
/// Slower than the channel-simulator tests, and worth it: this is the only test
/// that exercises the actual codec in the actual transfer, so it catches the
/// class of bug where the protocol and the codec each work alone and disagree
/// about frame sizes together.
#[test]
fn a_file_transfers_through_real_qr_codes_and_a_synthetic_camera() {
    // Small, because every frame is rendered and photographed. The point is
    // that the loop closes, not how fast.
    let original = file(2_000);
    // Sized from the MEASURED threshold rather than the theoretical MTU.
    //
    // A 100 B payload plus the 26 B PDU header encodes to about 53 modules at
    // correction Q, which at fill 0.75 over 720p gives roughly 8.8 px/module —
    // just above the 8.5 the sweep found necessary under typical capture.
    //
    // The first version of this test used 200 B, which is 69 modules and 7.0
    // px/module. That is above the 6.0 threshold measured under IDEAL capture,
    // and it never decoded once: the ideal figure does not describe a real
    // camera, and sizing a link from it produces a link that transmits nothing.
    let optical_mtu = 100 + optical_protocol::wire::OVERHEAD;

    let mut a = Modem::new(peer(1), optical_mtu);
    let mut b = Modem::new(peer(9), optical_mtu);
    a.send_file("through-the-air", original.clone());

    let conditions = Conditions {
        fill: 0.75,
        ..Conditions::typical()
    };

    let mut now = Duration::ZERO;
    let mut received = None;
    let mut photographed = 0u32;
    let mut decoded = 0u32;

    // One "photograph" per direction per tick.
    let shoot = |frame: &[u8]| -> Option<Vec<u8>> {
        let modules = encode(frame, Ecc::Q).ok()?;
        let (w, h, px) = capture(&modules, &conditions);
        let scan = scan_greyscale(w, h, &px);
        scan.detections.first().map(|d| d.payload.clone())
    };

    for _ in 0..4_000 {
        now += Duration::from_millis(TICK_MS);
        a.tick(now);
        b.tick(now);

        if let Some(frame) = a.poll_frame() {
            photographed += 1;
            if let Some(seen) = shoot(&frame) {
                decoded += 1;
                for e in b.handle_frame(&seen) {
                    if let Event::FileReceived { name, bytes } = e {
                        received = Some((name, bytes));
                    }
                }
            }
        }
        if let Some(frame) = b.poll_frame() {
            if let Some(seen) = shoot(&frame) {
                a.handle_frame(&seen);
            }
        }

        if received.is_some() {
            break;
        }
    }

    let (name, bytes) = received.expect("the file should arrive through the camera");
    assert_eq!(name, "through-the-air");
    assert_eq!(bytes, original, "byte for byte, through real QR codes");
    println!(
        "optical loop: {photographed} frames displayed, {decoded} decoded ({:.0}%)",
        100.0 * f64::from(decoded) / f64::from(photographed.max(1))
    );
}
