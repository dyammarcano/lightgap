//! Sliding-window tests, with no channel in between.
//!
//! This exercises the pure logic: what the sender emits, what the receiver
//! accepts, and what they tell each other. End-to-end transfer over a lossy
//! medium lives in `channel-sim`, which owns the simulator.

use optical_protocol::reliability::arq::{
    ArqReceiver, ArqSender, DEFAULT_WINDOW, MAX_MISSING_REPORTED,
};
use optical_protocol::reliability::{Feedback, Receiver, RecvError, Sender, Symbol};

const CS: usize = 100;

fn object(len: usize) -> Vec<u8> {
    // Non-repeating pattern: if the receiver places a chunk at the wrong offset,
    // constant filler would hide it.
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Wires sender and receiver together losslessly, feeding back every round.
fn transfer(sender: &mut ArqSender, receiver: &mut ArqReceiver, max_payload: usize) -> usize {
    let mut rounds = 0;
    while !sender.is_complete() {
        rounds += 1;
        assert!(rounds < 100_000, "does not converge");

        if let Some(sym) = sender.next_symbol(max_payload) {
            receiver.on_symbol(&sym).expect("valid symbol");
        }
        sender.on_feedback(&receiver.feedback());
    }
    rounds
}

#[test]
fn a_clean_transfer_reconstructs_the_object_exactly() {
    let original = object(1000);
    let mut tx = ArqSender::new(original.clone(), CS);
    let mut rx = ArqReceiver::new(original.len(), CS);

    transfer(&mut tx, &mut rx, CS);

    assert_eq!(rx.take_object().expect("complete"), original);
}

#[test]
fn a_short_final_chunk_is_handled() {
    // 1050 = ten chunks of 100 plus one of 50.
    let original = object(1050);
    let mut tx = ArqSender::new(original.clone(), CS);
    let mut rx = ArqReceiver::new(original.len(), CS);

    assert_eq!(tx.total_chunks(), 11);
    transfer(&mut tx, &mut rx, CS);

    assert_eq!(rx.take_object().expect("complete"), original);
}

#[test]
fn an_exact_multiple_does_not_produce_a_spare_chunk() {
    let original = object(1000);
    let tx = ArqSender::new(original, CS);
    assert_eq!(tx.total_chunks(), 10, "1000/100 is ten chunks, not eleven");
}

#[test]
fn an_empty_object_is_complete_from_the_start() {
    let tx = ArqSender::new(Vec::new(), CS);
    let mut rx = ArqReceiver::new(0, CS);

    assert!(tx.is_complete(), "nothing to send");
    assert!(rx.is_complete(), "nothing to wait for");
    assert_eq!(rx.take_object(), Some(Vec::new()));
    assert_eq!(rx.take_object(), None, "handed over exactly once");
}

#[test]
fn the_window_caps_symbols_in_flight() {
    let original = object(100 * 100);
    let mut tx = ArqSender::new(original, CS);

    // With no feedback at all, the sender must not exceed the window.
    let mut emitted = 0;
    while tx.next_symbol(CS).is_some() {
        emitted += 1;
        assert!(
            emitted <= DEFAULT_WINDOW as usize,
            "emitted {emitted} without acknowledgement, window is {DEFAULT_WINDOW}"
        );
    }
    assert_eq!(emitted, DEFAULT_WINDOW as usize);
}

#[test]
fn gaps_are_retransmitted_before_new_data() {
    let original = object(1000);
    let mut tx = ArqSender::new(original, CS);

    // Send the first five, then report that 1 and 3 are missing.
    for _ in 0..5 {
        tx.next_symbol(CS).unwrap();
    }
    tx.on_feedback(&Feedback::Selective {
        cumulative: 1,
        missing: vec![1, 3],
        window: DEFAULT_WINDOW as u16,
    });

    assert_eq!(tx.next_symbol(CS).unwrap().id, 1, "oldest gap first");
    assert_eq!(tx.next_symbol(CS).unwrap().id, 3, "then the next gap");
    assert_eq!(tx.next_symbol(CS).unwrap().id, 5, "and only then new data");
}

#[test]
fn a_gap_already_covered_is_not_retransmitted() {
    let original = object(1000);
    let mut tx = ArqSender::new(original, CS);
    for _ in 0..5 {
        tx.next_symbol(CS).unwrap();
    }

    tx.on_feedback(&Feedback::Selective {
        cumulative: 0,
        missing: vec![2],
        window: DEFAULT_WINDOW as u16,
    });
    // The cumulative point moves past the gap: it is no longer needed.
    tx.on_feedback(&Feedback::Selective {
        cumulative: 5,
        missing: vec![],
        window: DEFAULT_WINDOW as u16,
    });

    assert_eq!(
        tx.next_symbol(CS).unwrap().id,
        5,
        "gap 2 was covered by the cumulative acknowledgement"
    );
}

#[test]
fn the_cumulative_point_never_moves_backwards() {
    let mut tx = ArqSender::new(object(1000), CS);
    tx.on_feedback(&Feedback::Selective {
        cumulative: 7,
        missing: vec![],
        window: 0,
    });
    // A stale acknowledgement arriving late must not undo progress: on a channel
    // that reorders, this genuinely happens.
    tx.on_feedback(&Feedback::Selective {
        cumulative: 3,
        missing: vec![],
        window: 0,
    });
    assert_eq!(tx.progress().have, 7);
}

#[test]
fn feedback_from_another_mode_is_ignored_without_breaking() {
    let mut tx = ArqSender::new(object(1000), CS);
    tx.on_feedback(&Feedback::Fountain {
        complete: true,
        received: 999,
    });
    assert!(
        !tx.is_complete(),
        "fountain feedback must not complete an ARQ transfer"
    );
}

#[test]
fn a_duplicate_is_not_an_error() {
    let mut rx = ArqReceiver::new(1000, CS);
    let sym = Symbol {
        id: 0,
        bytes: vec![7; CS],
    };

    rx.on_symbol(&sym).expect("first time");
    rx.on_symbol(&sym)
        .expect("the medium duplicates on its own; not an error");
    assert_eq!(rx.progress().have, 1, "must not count twice");
}

#[test]
fn a_wrongly_sized_symbol_is_rejected() {
    let mut rx = ArqReceiver::new(1000, CS);
    let err = rx
        .on_symbol(&Symbol {
            id: 0,
            bytes: vec![0; CS - 1],
        })
        .unwrap_err();
    assert_eq!(
        err,
        RecvError::SymbolSize {
            got: CS - 1,
            expected: CS
        }
    );
}

#[test]
fn an_out_of_range_identifier_is_rejected() {
    let mut rx = ArqReceiver::new(1000, CS);
    let err = rx
        .on_symbol(&Symbol {
            id: 10,
            bytes: vec![0; CS],
        })
        .unwrap_err();
    assert_eq!(err, RecvError::OutOfRange { id: 10, chunks: 10 });
}

#[test]
fn the_gap_list_is_bounded() {
    // Far more gaps than the limit: the feedback has to fit in a frame, so it
    // gets trimmed.
    let total = (MAX_MISSING_REPORTED + 50) * CS;
    let mut rx = ArqReceiver::new(total, CS);

    // Receive only the last chunk: everything before it is a gap.
    let last = (total / CS - 1) as u32;
    rx.on_symbol(&Symbol {
        id: last,
        bytes: vec![0; CS],
    })
    .unwrap();

    let Feedback::Selective { missing, .. } = rx.feedback() else {
        panic!("ARQ must produce selective feedback");
    };
    assert_eq!(missing.len(), MAX_MISSING_REPORTED);
    assert_eq!(
        missing[0], 0,
        "the oldest are reported, since they are what blocks progress"
    );
}

#[test]
fn a_symbol_that_does_not_fit_is_not_emitted() {
    let mut tx = ArqSender::new(object(1000), CS);
    assert!(
        tx.next_symbol(CS - 1).is_none(),
        "with less room than a chunk it must emit nothing"
    );
    assert_eq!(tx.progress().have, 0, "and state must not have advanced");
    assert!(tx.next_symbol(CS).is_some(), "with room it does emit");
}

#[test]
fn the_object_is_handed_over_exactly_once() {
    let original = object(300);
    let mut tx = ArqSender::new(original.clone(), CS);
    let mut rx = ArqReceiver::new(original.len(), CS);
    transfer(&mut tx, &mut rx, CS);

    assert_eq!(rx.take_object(), Some(original));
    assert_eq!(rx.take_object(), None);
}

#[test]
fn nothing_is_handed_over_until_every_chunk_arrives() {
    let mut rx = ArqReceiver::new(300, CS);
    rx.on_symbol(&Symbol {
        id: 0,
        bytes: vec![1; CS],
    })
    .unwrap();
    rx.on_symbol(&Symbol {
        id: 2,
        bytes: vec![3; CS],
    })
    .unwrap();

    assert!(!rx.is_complete());
    assert_eq!(rx.take_object(), None, "chunk 1 is missing");
}

#[test]
fn arrival_order_does_not_change_the_result() {
    let original = object(1000);

    let rebuild = |ids: Vec<u32>| {
        let mut rx = ArqReceiver::new(original.len(), CS);
        for id in ids {
            let start = id as usize * CS;
            rx.on_symbol(&Symbol {
                id,
                bytes: original[start..start + CS].to_vec(),
            })
            .unwrap();
        }
        rx.take_object()
    };

    let forward: Vec<u32> = (0..10).collect();
    let backward: Vec<u32> = (0..10).rev().collect();
    let shuffled = vec![3, 7, 0, 9, 1, 5, 8, 2, 6, 4];

    assert_eq!(rebuild(forward), Some(original.clone()));
    assert_eq!(rebuild(backward), Some(original.clone()));
    assert_eq!(rebuild(shuffled), Some(original));
}
