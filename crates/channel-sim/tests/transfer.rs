//! Full end-to-end transfer over the simulated medium.
//!
//! This is the Phase 1 exit criterion: move a real object, whole and byte-for-
//! byte identical, across a channel that loses, delays, reorders and duplicates
//! — without turning on a camera.
//!
//! The driver models how an optical link actually works: **feedback is emitted
//! periodically, not in response to each data frame**. The receiver's display is
//! always showing some QR code, so its state is being broadcast continuously.
//! With reactive acknowledgement, losing one would leave the sender blocked
//! waiting for something nobody is going to repeat.

use std::time::Duration;

use channel_sim::{LinkConfig, SimPair};
use optical_protocol::channel::Channel;
use optical_protocol::reliability::arq::{ArqReceiver, ArqSender};
use optical_protocol::reliability::fountain::{symbol_size_for, FountainReceiver, FountainSender};
use optical_protocol::reliability::{Feedback, Receiver, Sender, Symbol};
use optical_protocol::wire::{Flags, Pdu, PduKind, OVERHEAD};

/// Channel MTU, in bytes per frame. A medium-density, comfortably readable QR.
const MTU: usize = 900;
/// Usable payload once the PDU header and CRC are deducted.
const PAYLOAD: usize = MTU - OVERHEAD;
/// How often, in ticks, the receiver refreshes its feedback.
const FEEDBACK_EVERY: u64 = 4;
/// Virtual duration of one tick: roughly one optical frame.
const TICK_MS: u64 = 80;

const SESSION: u64 = 0xfeed_face_dead_beef;

struct Outcome {
    object: Vec<u8>,
    ticks: u64,
    symbols_sent: u64,
    feedbacks: u64,
}

fn data(len: usize) -> Vec<u8> {
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

fn data_pdu(sym: &Symbol, flags: Flags) -> Vec<u8> {
    Pdu {
        session_id: SESSION,
        kind: PduKind::Data,
        flags,
        seq: sym.id,
        ack: 0,
        payload: sym.bytes.clone(),
    }
    .to_vec()
    .expect("symbol within the format limit")
}

fn ack_pdu(fb: &Feedback) -> Vec<u8> {
    Pdu {
        session_id: SESSION,
        kind: PduKind::Ack,
        flags: Flags::ACK_VALID,
        seq: 0,
        ack: 0,
        payload: fb.encode(),
    }
    .to_vec()
    .expect("feedback is bounded")
}

/// Drives a transfer until the receiver reconstructs or the tick budget runs
/// out.
fn run(
    tx: &mut dyn Sender,
    rx: &mut dyn Receiver,
    link: &mut SimPair,
    max_ticks: u64,
    data_flags: Flags,
) -> Outcome {
    let mut now = Duration::ZERO;
    let mut ticks = 0;
    let mut sent = 0;
    let mut feedbacks = 0;

    loop {
        ticks += 1;
        assert!(
            ticks <= max_ticks,
            "did not converge in {max_ticks} ticks (rx progress {:?})",
            rx.progress()
        );
        now += Duration::from_millis(TICK_MS);
        link.advance(now);

        // --- A emits one symbol per tick, like one QR code per frame ---------
        if !tx.is_complete() {
            if let Some(sym) = tx.next_symbol(PAYLOAD) {
                link.a
                    .send_frame(&data_pdu(&sym, data_flags))
                    .expect("fits the MTU");
                sent += 1;
            }
        }

        // --- B collects whatever arrived -------------------------------------
        while let Some(frame) = link.b.recv_frame() {
            match Pdu::decode(&frame) {
                Ok(pdu) if pdu.kind == PduKind::Data => {
                    let sym = Symbol {
                        id: pdu.seq,
                        bytes: pdu.payload,
                    };
                    // A malformed symbol is discarded: it happens in this medium.
                    let _ = rx.on_symbol(&sym);
                }
                Ok(_) => {}
                Err(_) => link.b.note_rejected(),
            }
        }

        // --- B broadcasts its state every few ticks --------------------------
        if ticks % FEEDBACK_EVERY == 0 {
            link.b
                .send_frame(&ack_pdu(&rx.feedback()))
                .expect("fits the MTU");
            feedbacks += 1;
        }

        // --- A collects the feedback -----------------------------------------
        while let Some(frame) = link.a.recv_frame() {
            match Pdu::decode(&frame) {
                Ok(pdu) if pdu.kind == PduKind::Ack => {
                    if let Some(fb) = Feedback::decode(&pdu.payload) {
                        tx.on_feedback(&fb);
                    }
                }
                Ok(_) => {}
                Err(_) => link.a.note_rejected(),
            }
        }

        if let Some(object) = rx.take_object() {
            return Outcome {
                object,
                ticks,
                symbols_sent: sent,
                feedbacks,
            };
        }
    }
}

/// Phase 1 criterion for fountain coding: 40% loss in both directions.
#[test]
fn fountain_transfers_at_forty_percent_loss() {
    let original = data(5 * 1024 * 1024);
    let ss = symbol_size_for(PAYLOAD).expect("a symbol fits");

    let mut tx = FountainSender::new(&original, ss);
    let mut rx = FountainReceiver::new(original.len() as u64, ss);
    let mut link = SimPair::new(LinkConfig::optical(MTU, 0.40), 20_260_819);

    let r = run(&mut tx, &mut rx, &mut link, 200_000, Flags::FOUNTAIN);

    assert_eq!(
        r.object, original,
        "the object must arrive byte-for-byte identical"
    );
    println!(
        "fountain 40%: {} ticks, {} symbols, {} feedbacks",
        r.ticks, r.symbols_sent, r.feedbacks
    );
}

/// Phase 1 criterion for ARQ: 15% loss in both directions.
#[test]
fn arq_transfers_at_fifteen_percent_loss() {
    let original = data(5 * 1024 * 1024);

    let mut tx = ArqSender::new(original.clone(), PAYLOAD);
    let mut rx = ArqReceiver::new(original.len(), PAYLOAD);
    let mut link = SimPair::new(LinkConfig::optical(MTU, 0.15), 20_260_819);

    let r = run(&mut tx, &mut rx, &mut link, 200_000, Flags::NONE);

    assert_eq!(
        r.object, original,
        "the object must arrive byte-for-byte identical"
    );
    println!(
        "arq 15%: {} ticks, {} symbols, {} feedbacks",
        r.ticks, r.symbols_sent, r.feedbacks
    );
}

/// On a clean channel, fountain coding should not need much more than the source
/// symbols. Needing far more would mean something is wrong in repair generation.
#[test]
fn fountain_wastes_little_on_a_clean_channel() {
    let original = data(128 * 1024);
    let ss = symbol_size_for(PAYLOAD).unwrap();

    let mut tx = FountainSender::new(&original, ss);
    let k = tx.source_symbols() as u64;
    let mut rx = FountainReceiver::new(original.len() as u64, ss);
    let mut link = SimPair::new(LinkConfig::perfect(MTU), 7);

    let r = run(&mut tx, &mut rx, &mut link, 100_000, Flags::FOUNTAIN);

    assert_eq!(r.object, original);
    assert!(
        r.symbols_sent < k * 2,
        "sent {} symbols for K={k}: too much waste on a clean channel",
        r.symbols_sent
    );
}

/// Corruption is caught by the CRC and the frame discarded; the transfer has to
/// survive regardless. This is the real case of a blurry QR code that still
/// decodes, but to the wrong bytes.
#[test]
fn fountain_survives_corruption_on_top_of_loss() {
    let original = data(128 * 1024);
    let ss = symbol_size_for(PAYLOAD).unwrap();

    let cfg = LinkConfig::optical(MTU, 0.10).with_corruption(0.10);
    let mut tx = FountainSender::new(&original, ss);
    let mut rx = FountainReceiver::new(original.len() as u64, ss);
    let mut link = SimPair::new(cfg, 99);

    let r = run(&mut tx, &mut rx, &mut link, 200_000, Flags::FOUNTAIN);

    assert_eq!(r.object, original);
    assert!(
        link.b.health().frames_rejected > 0,
        "the test should have exercised CRC rejection"
    );
}

/// An asymmetric link: the return path is far worse than the forward one. This
/// is the scenario the design has in mind when one webcam is worse than the
/// other.
#[test]
fn fountain_tolerates_a_much_worse_return_path() {
    let original = data(128 * 1024);
    let ss = symbol_size_for(PAYLOAD).unwrap();

    let forward = LinkConfig::optical(MTU, 0.05);
    let back = LinkConfig::optical(MTU, 0.80);
    let mut tx = FountainSender::new(&original, ss);
    let mut rx = FountainReceiver::new(original.len() as u64, ss);
    let mut link = SimPair::asymmetric(forward, back, 5);

    let r = run(&mut tx, &mut rx, &mut link, 200_000, Flags::FOUNTAIN);
    assert_eq!(r.object, original);
}
