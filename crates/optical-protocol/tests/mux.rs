//! Multiplexer tests.
//!
//! The behaviour worth defending here is that the scheduler routes by *fitness
//! for the job* rather than by a single quality number. An acknowledgement and a
//! data symbol want opposite things from a channel, and a scheduler that ranked
//! channels once and used that ranking for both would get one of them wrong.

use std::time::Duration;

use optical_protocol::channel::{ChannelCaps, ChannelHealth, ChannelId, Direction};
use optical_protocol::mux::{ChannelSlot, Dedup, Priority, Scheduler, DEDUP_WINDOW};
use optical_protocol::wire::{Flags, Pdu, PduKind};

/// The visual channel: high throughput, high latency.
fn visual() -> ChannelSlot {
    ChannelSlot {
        caps: ChannelCaps {
            id: ChannelId::Visual,
            mtu: 900,
            direction: Direction::Bidirectional,
            nominal_bps: 5_000,
            nominal_latency: Duration::from_millis(250),
        },
        health: ChannelHealth::default(),
        usable: true,
    }
}

/// The acoustic channel: low throughput, low latency.
fn acoustic() -> ChannelSlot {
    ChannelSlot {
        caps: ChannelCaps {
            id: ChannelId::Acoustic,
            mtu: 32,
            direction: Direction::Bidirectional,
            nominal_bps: 12,
            nominal_latency: Duration::from_millis(30),
        },
        health: ChannelHealth::default(),
        usable: true,
    }
}

fn pdu(kind: PduKind, seq: u32) -> Pdu {
    Pdu {
        session_id: 42,
        kind,
        flags: Flags::NONE,
        seq,
        ack: 0,
        payload: Vec::new(),
    }
}

fn both() -> Scheduler {
    let mut s = Scheduler::new();
    s.upsert(visual());
    s.upsert(acoustic());
    s
}

#[test]
fn priority_is_ordered_by_how_much_losing_it_hurts() {
    assert!(Priority::Control > Priority::Metadata);
    assert!(Priority::Metadata > Priority::Data);
}

#[test]
fn every_pdu_kind_has_a_class() {
    for kind in [
        PduKind::Hello,
        PduKind::Capabilities,
        PduKind::Data,
        PduKind::Ack,
        PduKind::Probe,
        PduKind::ProbeResult,
        PduKind::Complete,
        PduKind::Cancel,
    ] {
        // The point is that classification is total: a kind added later without
        // a class would silently fall into whatever the catch-all arm is.
        let _ = Priority::of(&pdu(kind, 0));
    }
    assert_eq!(Priority::of(&pdu(PduKind::Data, 0)), Priority::Data);
    assert_eq!(Priority::of(&pdu(PduKind::Ack, 0)), Priority::Control);
    assert_eq!(Priority::of(&pdu(PduKind::Probe, 0)), Priority::Metadata);
}

/// The routing decision the whole multimodal design exists for.
#[test]
fn bulk_goes_visual_and_control_goes_acoustic() {
    let s = both();
    assert_eq!(
        s.route(Priority::Data),
        Some(ChannelId::Visual),
        "the visual channel carries volume"
    );
    assert_eq!(
        s.route(Priority::Control),
        Some(ChannelId::Acoustic),
        "acknowledgements want low latency, not throughput; sending them over \
         the visual channel costs a full optical round trip"
    );
}

#[test]
fn with_only_one_channel_everything_goes_there() {
    let mut s = Scheduler::new();
    s.upsert(visual());

    assert_eq!(s.route(Priority::Data), Some(ChannelId::Visual));
    assert_eq!(
        s.route(Priority::Control),
        Some(ChannelId::Visual),
        "visual-only is the fallback the design promises, not a failure"
    );
}

#[test]
fn control_falls_back_to_visual_when_audio_goes_down() {
    let mut s = both();
    assert_eq!(s.route(Priority::Control), Some(ChannelId::Acoustic));

    s.upsert(ChannelSlot {
        usable: false,
        ..acoustic()
    });
    assert_eq!(
        s.route(Priority::Control),
        Some(ChannelId::Visual),
        "losing audio must degrade to visual acknowledgements, not stop the \
         session"
    );
    assert_eq!(s.route(Priority::Data), Some(ChannelId::Visual));
}

#[test]
fn a_channel_producing_only_garbage_is_not_chosen() {
    // The bug this guards against is the one that was already found once in
    // `rejection_rate`: a metric that under-reports lets the scheduler keep
    // feeding a dead channel.
    let mut s = Scheduler::new();
    s.upsert(ChannelSlot {
        health: ChannelHealth {
            frames_received: 100,
            frames_rejected: 100,
            ..ChannelHealth::default()
        },
        ..visual()
    });
    s.upsert(acoustic());

    assert_eq!(
        s.route(Priority::Data),
        Some(ChannelId::Acoustic),
        "a channel rejecting everything must lose even on its strong suit"
    );
}

#[test]
fn nothing_usable_means_no_route() {
    let mut s = Scheduler::new();
    s.upsert(ChannelSlot {
        usable: false,
        ..visual()
    });
    s.upsert(ChannelSlot {
        usable: false,
        ..acoustic()
    });

    assert_eq!(
        s.route(Priority::Control),
        None,
        "with everything down the session must learn it cannot send, rather \
         than be handed a channel that will silently swallow frames"
    );
    assert_eq!(s.usable_count(), 0);
}

#[test]
fn control_is_duplicated_across_channels_and_data_is_not() {
    let s = both();

    let control = s.route_all(Priority::Control);
    assert_eq!(
        control.len(),
        2,
        "a lost Cancel or Complete can leave both sides waiting forever; the \
         duplicate costs a handful of bytes"
    );

    let data = s.route_all(Priority::Data);
    assert_eq!(
        data.len(),
        1,
        "duplicating data would halve throughput to buy redundancy the \
         reliability layer already provides more cheaply"
    );
}

#[test]
fn upsert_replaces_rather_than_accumulates() {
    let mut s = Scheduler::new();
    s.upsert(visual());
    s.upsert(visual());
    assert_eq!(s.slots().len(), 1, "the same channel must not appear twice");
}

#[test]
fn removing_a_channel_takes_it_out_of_routing() {
    let mut s = both();
    s.remove(ChannelId::Acoustic);
    assert_eq!(s.route(Priority::Control), Some(ChannelId::Visual));
    assert_eq!(s.usable_count(), 1);
}

#[test]
fn the_mtu_of_the_chosen_route_is_available() {
    let s = both();
    assert_eq!(s.mtu_for(ChannelId::Visual), Some(900));
    assert_eq!(s.mtu_for(ChannelId::Acoustic), Some(32));
    assert_eq!(s.mtu_for(ChannelId::Loopback), None);
}

// --- deduplication ----------------------------------------------------------

#[test]
fn the_second_copy_of_a_duplicated_message_is_suppressed() {
    let mut d = Dedup::default();
    let p = pdu(PduKind::Ack, 7);

    assert!(d.accept(&p), "the first copy is new");
    assert!(
        !d.accept(&p),
        "counting an Ack twice would corrupt the progress estimate"
    );
}

#[test]
fn messages_of_different_kinds_can_share_a_sequence_number() {
    // Hello and Ack travel independently and may legitimately collide on seq.
    // Keying on session and seq alone would silently drop one of them.
    let mut d = Dedup::default();
    assert!(d.accept(&pdu(PduKind::Hello, 3)));
    assert!(
        d.accept(&pdu(PduKind::Ack, 3)),
        "a different kind at the same sequence is a different message"
    );
}

#[test]
fn different_sessions_do_not_collide() {
    let mut d = Dedup::default();
    let a = pdu(PduKind::Ack, 1);
    let b = Pdu {
        session_id: 99,
        ..a.clone()
    };
    assert!(d.accept(&a));
    assert!(d.accept(&b), "a different session is a different message");
}

#[test]
fn the_window_is_bounded() {
    // A transfer runs for tens of thousands of frames; an unbounded set would
    // grow for the whole session.
    let mut d = Dedup::new(8);
    for seq in 0..100 {
        d.accept(&pdu(PduKind::Data, seq));
    }
    assert_eq!(d.len(), 8, "the window must not grow without limit");
}

#[test]
fn an_evicted_message_is_accepted_again() {
    // The consequence of bounding, stated so nobody is surprised by it: the
    // window only has to outlast the difference in arrival time between two
    // copies, which is one channel's latency, not the whole transfer.
    let mut d = Dedup::new(4);
    let old = pdu(PduKind::Data, 0);
    assert!(d.accept(&old));
    for seq in 1..=4 {
        d.accept(&pdu(PduKind::Data, seq));
    }
    assert!(
        d.accept(&old),
        "once evicted it is seen as new again; the window is sized for channel \
         latency, not for the session"
    );
}

#[test]
fn the_default_window_is_large_enough_to_be_useful() {
    // A compile-time check rather than a runtime assertion: the value is a
    // constant, so a runtime assert would be checking something the compiler
    // already knows, and clippy rightly objects.
    const _: () = assert!(
        DEDUP_WINDOW >= 64,
        "a window smaller than the in-flight count would let duplicates through"
    );
    let d = Dedup::default();
    assert!(d.is_empty());
}

#[test]
fn clearing_forgets_everything() {
    let mut d = Dedup::default();
    let p = pdu(PduKind::Ack, 1);
    d.accept(&p);
    d.clear();
    assert!(d.accept(&p), "after a clear it is new again");
    assert_eq!(d.len(), 1);
}
