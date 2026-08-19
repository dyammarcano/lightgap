//! Fountain coding tests, with no channel in between.

use optical_protocol::reliability::fountain::{
    symbol_size_for, FountainReceiver, FountainSender, PACKET_ID_LEN,
};
use optical_protocol::reliability::{Feedback, Receiver, RecvError, Sender, Symbol};

const SS: u16 = 200;
/// Channel payload that leaves room for the symbol and its identifier.
const MAX: usize = SS as usize + PACKET_ID_LEN;

fn object(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Emits `n` symbols without feedback. Returns what came out.
fn emit(tx: &mut FountainSender, n: usize) -> Vec<Symbol> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        match tx.next_symbol(MAX) {
            Some(s) => out.push(s),
            None => break,
        }
    }
    out
}

#[test]
fn reconstructs_from_all_the_source_symbols() {
    let original = object(10_000);
    let mut tx = FountainSender::new(&original, SS);
    let mut rx = FountainReceiver::new(original.len() as u64, SS);

    let k = tx.source_symbols() as usize;
    for sym in emit(&mut tx, k * 2) {
        rx.on_symbol(&sym).unwrap();
        if rx.is_complete() {
            break;
        }
    }

    assert!(rx.is_complete());
    assert_eq!(rx.take_object().unwrap(), original);
}

/// The property that justifies choosing fountain coding: it does not matter
/// *which* symbols are lost as long as enough arrive. Without this, the whole
/// design argument for fountain coding collapses.
#[test]
fn reconstructs_after_losing_forty_percent_of_the_symbols() {
    let original = object(10_000);
    let mut tx = FountainSender::new(&original, SS);
    let mut rx = FountainReceiver::new(original.len() as u64, SS);

    // Generate plenty and throw away 40% with a deterministic pattern.
    let k = tx.source_symbols() as usize;
    let all = emit(&mut tx, k * 3);
    let mut discarded = 0;
    for (i, sym) in all.iter().enumerate() {
        if i % 5 < 2 {
            discarded += 1;
            continue;
        }
        rx.on_symbol(sym).unwrap();
        if rx.is_complete() {
            break;
        }
    }

    assert!(discarded > 0, "the test must actually discard something");
    assert!(
        rx.is_complete(),
        "60% of a 3x surplus should be more than enough to reconstruct"
    );
    assert_eq!(rx.take_object().unwrap(), original);
}

#[test]
fn arrival_order_is_irrelevant() {
    let original = object(5_000);
    let mut tx = FountainSender::new(&original, SS);
    let k = tx.source_symbols() as usize;
    let mut all = emit(&mut tx, k * 2);

    // Deterministic shuffle: reverse, then interleave the two halves.
    all.reverse();
    let (a, b) = all.split_at(all.len() / 2);
    let interleaved: Vec<_> = a.iter().zip(b.iter()).flat_map(|(x, y)| [x, y]).collect();

    let mut rx = FountainReceiver::new(original.len() as u64, SS);
    for sym in interleaved {
        rx.on_symbol(sym).unwrap();
        if rx.is_complete() {
            break;
        }
    }

    assert_eq!(rx.take_object().unwrap(), original);
}

#[test]
fn an_empty_object_is_complete_from_the_start() {
    let mut tx = FountainSender::new(&[], SS);
    let mut rx = FountainReceiver::new(0, SS);

    assert!(tx.is_complete(), "nothing to emit");
    assert!(rx.is_complete(), "nothing to wait for");
    assert!(
        tx.next_symbol(MAX).is_none(),
        "an empty object must produce no symbols and must not spin in refill"
    );
    assert_eq!(rx.take_object(), Some(Vec::new()));
}

/// `EncodingPacket::deserialize` indexes the first four bytes without checking
/// them. A truncated symbol has to die in validation, not in a panic inside the
/// library.
#[test]
fn a_truncated_symbol_does_not_panic() {
    let mut rx = FountainReceiver::new(10_000, SS);

    for len in [0usize, 1, 2, 3, 4, 5, MAX - 1, MAX + 1] {
        let err = rx
            .on_symbol(&Symbol {
                id: 0,
                bytes: vec![0; len],
            })
            .unwrap_err();
        assert_eq!(
            err,
            RecvError::SymbolSize {
                got: len,
                expected: MAX
            },
            "a {len} B symbol should be rejected cleanly"
        );
    }
}

/// The bug this implementation actually had: `take_object` emptied the `Option`
/// and the receiver started declaring itself incomplete right after handing over
/// the result, which would have made its feedback ask the sender to keep
/// emitting forever.
#[test]
fn stays_complete_after_handing_over_the_object() {
    let original = object(3_000);
    let mut tx = FountainSender::new(&original, SS);
    let mut rx = FountainReceiver::new(original.len() as u64, SS);

    let k = tx.source_symbols() as usize;
    for sym in emit(&mut tx, k * 3) {
        rx.on_symbol(&sym).unwrap();
        if rx.is_complete() {
            break;
        }
    }

    assert_eq!(rx.take_object().unwrap(), original);
    assert!(rx.is_complete(), "still complete after handing over");
    assert_eq!(
        rx.feedback(),
        Feedback::Fountain {
            complete: true,
            received: rx.received()
        },
        "feedback must keep telling the sender to stop"
    );
    assert_eq!(rx.take_object(), None, "handed over exactly once");
}

#[test]
fn the_sender_stops_when_told_to() {
    let original = object(10_000);
    let mut tx = FountainSender::new(&original, SS);

    assert!(tx.next_symbol(MAX).is_some());
    tx.on_feedback(&Feedback::Fountain {
        complete: true,
        received: 999,
    });

    assert!(tx.is_complete());
    assert!(
        tx.next_symbol(MAX).is_none(),
        "once the end is confirmed it must emit no more"
    );
}

#[test]
fn arq_feedback_is_ignored_without_breaking() {
    let original = object(3_000);
    let mut tx = FountainSender::new(&original, SS);
    tx.on_feedback(&Feedback::Selective {
        cumulative: 9_999,
        missing: vec![],
        window: 16,
    });
    assert!(
        !tx.is_complete(),
        "ARQ feedback must not complete a fountain transfer"
    );
}

#[test]
fn a_symbol_that_does_not_fit_is_not_emitted() {
    let original = object(10_000);
    let mut tx = FountainSender::new(&original, SS);
    assert!(
        tx.next_symbol(MAX - 1).is_none(),
        "without room for symbol plus identifier it must not emit"
    );
    assert!(tx.next_symbol(MAX).is_some());
}

#[test]
fn the_sender_generates_more_symbols_than_the_source_has() {
    let original = object(5_000);
    let mut tx = FountainSender::new(&original, SS);
    let k = tx.source_symbols() as usize;

    // Exceeding K is not a defect, it is the mechanism. Without unbounded repair
    // a loss near the end would leave the transfer stuck.
    let emitted = emit(&mut tx, k * 3).len();
    assert!(
        emitted > k,
        "emitted {emitted} with K={k}; repair must be unbounded"
    );
}

/// Regression for the bug that cost four integration tests.
///
/// Sender and receiver must derive **the same** effective symbol size. When they
/// disagreed, the receiver rejected every symbol and the symptom was
/// "received: 0" — which looks like a transport failure rather than a parameter
/// mismatch. Unit tests missed it because they use 200, already aligned to 8; it
/// took a realistic size to surface.
///
/// What is asserted is the agreement, not a specific number: the number depends
/// on how the OTI is built, the agreement is the invariant.
#[test]
fn an_unaligned_symbol_size_still_works() {
    const REQUESTED: u16 = 870;
    let original = object(20_000);

    let mut tx = FountainSender::new(&original, REQUESTED);
    let mut rx = FountainReceiver::new(original.len() as u64, REQUESTED);

    assert_eq!(
        tx.symbol_size(),
        rx.symbol_size(),
        "both sides must derive the same effective size"
    );
    assert_eq!(tx.wire_len(), rx.wire_len());

    let width = tx.wire_len();
    let k = tx.source_symbols() as usize;
    for _ in 0..k * 3 {
        let Some(sym) = tx.next_symbol(width) else {
            break;
        };
        rx.on_symbol(&sym).expect("the receiver must accept it");
        if rx.is_complete() {
            break;
        }
    }

    assert!(rx.is_complete(), "should have reconstructed");
    assert_eq!(rx.take_object().unwrap(), original);
}

/// Building the receiver from the sender's plan is the preferred path: it rules
/// out the two sides splitting the object differently.
#[test]
fn the_receiver_can_be_built_from_the_senders_plan() {
    const REQUESTED: u16 = 870;
    let original = object(20_000);

    let mut tx = FountainSender::new(&original, REQUESTED);
    let oti = tx.oti_bytes().expect("non-empty object");
    let mut rx = FountainReceiver::from_oti_bytes(&oti);

    assert_eq!(tx.symbol_size(), rx.symbol_size());
    assert_eq!(tx.source_symbols(), rx.source_symbols_expected());

    let width = tx.wire_len();
    let k = tx.source_symbols() as usize;
    for _ in 0..k * 3 {
        let Some(sym) = tx.next_symbol(width) else {
            break;
        };
        rx.on_symbol(&sym).unwrap();
        if rx.is_complete() {
            break;
        }
    }
    assert_eq!(rx.take_object().unwrap(), original);
}

#[test]
fn an_empty_object_has_no_plan_to_send() {
    let tx = FountainSender::new(&[], SS);
    assert_eq!(
        tx.oti_bytes(),
        None,
        "with no object there are no parameters; the receiver resolves it from \
         the length alone"
    );
}

#[test]
fn symbol_size_for_uses_the_whole_payload() {
    assert_eq!(symbol_size_for(874), Some(870), "nothing is trimmed");
    assert_eq!(symbol_size_for(904), Some(900));
    assert_eq!(symbol_size_for(5), Some(1));
    assert_eq!(symbol_size_for(4), None, "no room for data");
    assert_eq!(symbol_size_for(0), None);
}

/// Source blocks are bounded so decoding does not blow up. Without this cap a
/// 5 MB object fell into a single block of ~6000 symbols and reconstructing it
/// cost over nine minutes of CPU.
#[test]
fn source_blocks_are_bounded() {
    use optical_protocol::reliability::fountain::{plan, MAX_SYMBOLS_PER_BLOCK};

    let symbol = 870u16;
    for mb in [1u64, 5, 20] {
        let len = mb * 1024 * 1024;
        let cfg = plan(len, symbol);
        let total = len.div_ceil(u64::from(symbol));
        let per_block = total.div_ceil(u64::from(cfg.source_blocks()));
        assert!(
            per_block <= u64::from(MAX_SYMBOLS_PER_BLOCK),
            "{mb} MB: {per_block} symbols per block exceeds the cap of \
             {MAX_SYMBOLS_PER_BLOCK}"
        );
    }
}

#[test]
fn sender_progress_reflects_the_receiver_not_what_was_emitted() {
    let original = object(10_000);
    let mut tx = FountainSender::new(&original, SS);
    emit(&mut tx, 50);

    assert_eq!(
        tx.progress().have,
        0,
        "having emitted 50 symbols is not progress while the receiver is silent"
    );

    tx.on_feedback(&Feedback::Fountain {
        complete: false,
        received: 30,
    });
    assert_eq!(tx.progress().have, 30);
}
