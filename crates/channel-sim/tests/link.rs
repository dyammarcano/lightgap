//! Tests of the simulator itself.
//!
//! If the simulator lies, every test built on it is worthless: the protocol core
//! would pass at 40% loss because the simulator was not actually losing
//! anything. So this checks that it does exactly what it claims before anything
//! trusts it.

use std::time::Duration;

use channel_sim::{LinkConfig, SimPair};
use optical_protocol::channel::{Channel, ChannelError};
use optical_protocol::wire::{Flags, Pdu, PduKind};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn pdu(seq: u32) -> Pdu {
    Pdu {
        session_id: 7,
        kind: PduKind::Data,
        flags: Flags::NONE,
        seq,
        ack: 0,
        payload: vec![0xab; 64],
    }
}

#[test]
fn a_perfect_link_delivers_everything_in_order_and_intact() {
    let mut link = SimPair::new(LinkConfig::perfect(1024), 1);

    for seq in 0..50u32 {
        link.a.send_frame(&pdu(seq).to_vec().unwrap()).unwrap();
    }
    link.advance(ms(1));

    for seq in 0..50u32 {
        let frame = link.b.recv_frame().expect("frame should arrive");
        let received = Pdu::decode(&frame).expect("should decode");
        assert_eq!(received, pdu(seq), "frame {seq} arrived altered");
    }
    assert!(link.b.recv_frame().is_none(), "nothing should be left over");
}

#[test]
fn the_same_seed_produces_the_same_sequence() {
    let run = |seed: u64| {
        let mut link = SimPair::new(LinkConfig::optical(1024, 0.4), seed);
        let mut arrived = Vec::new();
        for seq in 0..200u32 {
            link.advance(ms(seq as u64));
            link.a.send_frame(&pdu(seq).to_vec().unwrap()).unwrap();
        }
        link.advance(ms(10_000));
        while let Some(f) = link.b.recv_frame() {
            arrived.push(Pdu::decode(&f).unwrap().seq);
        }
        arrived
    };

    assert_eq!(run(42), run(42), "the same seed must repeat");
    assert_ne!(run(42), run(43), "different seeds should not coincide");
}

#[test]
fn the_loss_rate_is_the_one_declared() {
    const N: u32 = 10_000;
    let mut link = SimPair::new(LinkConfig::optical(1024, 0.4), 7);

    for seq in 0..N {
        link.a.send_frame(&pdu(seq).to_vec().unwrap()).unwrap();
    }

    let stats = link.a.tx_stats();
    assert_eq!(stats.offered, u64::from(N));

    let observed = stats.dropped as f64 / f64::from(N);
    assert!(
        (0.375..0.425).contains(&observed),
        "observed loss {observed:.4}, expected ~0.40 (5 sigma is about 0.025)"
    );
}

#[test]
fn the_delay_is_respected_to_the_millisecond() {
    let cfg = LinkConfig {
        mtu: 1024,
        latency: ms(100),
        ..LinkConfig::default()
    };
    let mut link = SimPair::new(cfg, 1);

    link.a.send_frame(&pdu(0).to_vec().unwrap()).unwrap();

    link.advance(ms(99));
    assert!(
        link.b.recv_frame().is_none(),
        "at 99 ms the frame should not have arrived"
    );

    link.advance(ms(100));
    assert!(
        link.b.recv_frame().is_some(),
        "at 100 ms the frame should be available"
    );
}

#[test]
fn jitter_produces_reordering() {
    let cfg = LinkConfig {
        mtu: 1024,
        latency: ms(100),
        jitter: ms(60),
        ..LinkConfig::default()
    };
    let mut link = SimPair::new(cfg, 5);

    // Frames one millisecond apart with up to 60 ms of jitter: overtaking is the
    // norm, not the exception.
    for seq in 0..200u32 {
        link.advance(ms(seq as u64));
        link.a.send_frame(&pdu(seq).to_vec().unwrap()).unwrap();
    }
    link.advance(ms(10_000));
    while link.b.recv_frame().is_some() {}

    assert!(
        link.b.rx_reorders() > 10,
        "with 60 ms jitter and frames every 1 ms there should be reordering, got {}",
        link.b.rx_reorders()
    );
}

#[test]
fn without_jitter_there_is_no_reordering() {
    let mut link = SimPair::new(LinkConfig::perfect(1024), 5);
    for seq in 0..200u32 {
        link.advance(ms(seq as u64));
        link.a.send_frame(&pdu(seq).to_vec().unwrap()).unwrap();
    }
    link.advance(ms(10_000));
    while link.b.recv_frame().is_some() {}

    assert_eq!(
        link.b.rx_reorders(),
        0,
        "without jitter the order must be preserved"
    );
}

#[test]
fn duplication_delivers_the_frame_twice() {
    let cfg = LinkConfig::perfect(1024).with_duplication(1.0);
    let mut link = SimPair::new(cfg, 3);

    link.a.send_frame(&pdu(0).to_vec().unwrap()).unwrap();
    link.advance(ms(1));

    let first = link.b.recv_frame().expect("first copy");
    let second = link.b.recv_frame().expect("second copy");
    assert_eq!(first, second, "both copies should be identical");
    assert!(link.b.recv_frame().is_none(), "there should be only two");
}

/// The test that joins simulator and wire format: corruption from the medium has
/// to be caught by the CRC rather than leak through to the application layer.
#[test]
fn corruption_from_the_medium_is_caught_by_the_crc() {
    let cfg = LinkConfig::perfect(1024).with_corruption(1.0);
    let mut link = SimPair::new(cfg, 11);

    const N: u32 = 500;
    for seq in 0..N {
        link.a.send_frame(&pdu(seq).to_vec().unwrap()).unwrap();
    }
    link.advance(ms(1));

    let mut received = 0;
    let mut rejected = 0;
    while let Some(frame) = link.b.recv_frame() {
        received += 1;
        if Pdu::decode(&frame).is_err() {
            rejected += 1;
            link.b.note_rejected();
        }
    }

    assert_eq!(received, N as usize, "every frame should arrive");
    assert_eq!(
        rejected, N as usize,
        "at 100% corruption no frame should pass validation"
    );
    assert_eq!(link.b.health().frames_rejected, u64::from(N));
    assert_eq!(
        link.b.health().rejection_rate(),
        1.0,
        "at 100% corruption the rejection rate is 1, not 0.5: rejected frames \
         are already counted among the received ones"
    );
}

#[test]
fn a_frame_larger_than_the_mtu_is_rejected_at_send_time() {
    let mut link = SimPair::new(LinkConfig::perfect(64), 1);
    let big = vec![0u8; 65];

    assert_eq!(
        link.a.send_frame(&big),
        Err(ChannelError::OverMtu { got: 65, mtu: 64 })
    );
    assert_eq!(
        link.a.health().frames_sent,
        0,
        "a rejected frame must not count as sent"
    );
}

/// The design allows asymmetric links — audio may work from A to B and not the
/// other way — so the simulator has to be able to express that.
#[test]
fn the_two_directions_are_independent() {
    let alive = LinkConfig::perfect(1024);
    let dead = LinkConfig {
        loss: 1.0,
        ..LinkConfig::perfect(1024)
    };
    let mut link = SimPair::asymmetric(alive, dead, 1);

    link.a.send_frame(&pdu(1).to_vec().unwrap()).unwrap();
    link.b.send_frame(&pdu(2).to_vec().unwrap()).unwrap();
    link.advance(ms(1));

    assert!(
        link.b.recv_frame().is_some(),
        "the A to B direction should work"
    );
    assert!(
        link.a.recv_frame().is_none(),
        "the B to A direction should be dead"
    );
}

#[test]
fn rx_idle_distinguishes_empty_from_in_flight() {
    let cfg = LinkConfig {
        mtu: 1024,
        latency: ms(100),
        ..LinkConfig::default()
    };
    let mut link = SimPair::new(cfg, 1);

    assert!(link.b.rx_idle(), "freshly created, nothing is in flight");

    link.a.send_frame(&pdu(0).to_vec().unwrap()).unwrap();
    assert!(!link.b.rx_idle(), "one frame is in flight");

    link.advance(ms(100));
    assert!(!link.b.rx_idle(), "it arrived but nobody collected it");

    link.b.recv_frame().unwrap();
    assert!(link.b.rx_idle(), "everything has been collected");
}
