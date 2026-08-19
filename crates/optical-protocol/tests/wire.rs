//! Wire format tests.
//!
//! The optical channel delivers corrupt frames routinely — blur, reflections,
//! motion — so what is asserted here is not merely that the round trip works,
//! but that corruption is **always detected**. One bad byte passing for good
//! becomes a corrupt file on the other side.

use optical_protocol::wire::{
    Flags, Pdu, PduKind, WireError, HEADER_LEN, MAX_PAYLOAD, OVERHEAD, PROTOCOL_VERSION,
};
use proptest::prelude::*;

fn sample() -> Pdu {
    Pdu {
        session_id: 0x0123_4567_89ab_cdef,
        kind: PduKind::Data,
        flags: Flags::ACK_VALID | Flags::FOUNTAIN,
        seq: 42,
        ack: 41,
        payload: b"test payload".to_vec(),
    }
}

#[test]
fn round_trip_preserves_every_field() {
    let pdu = sample();
    let bytes = pdu.to_vec().expect("encodes");
    assert_eq!(Pdu::decode(&bytes).expect("decodes"), pdu);
}

#[test]
fn encoded_len_matches_what_is_produced() {
    let pdu = sample();
    assert_eq!(pdu.to_vec().unwrap().len(), pdu.encoded_len());
    assert_eq!(pdu.encoded_len(), OVERHEAD + pdu.payload.len());
}

#[test]
fn an_empty_payload_is_valid() {
    let pdu = Pdu {
        payload: Vec::new(),
        ..sample()
    };
    let bytes = pdu.to_vec().unwrap();
    assert_eq!(bytes.len(), OVERHEAD);
    assert_eq!(Pdu::decode(&bytes).unwrap(), pdu);
}

#[test]
fn a_short_buffer_is_rejected_without_reading_fields() {
    for n in 0..OVERHEAD {
        let buf = vec![0u8; n];
        assert_eq!(
            Pdu::decode(&buf),
            Err(WireError::TooShort {
                got: n,
                need: OVERHEAD
            }),
            "a {n} B buffer should be rejected as too short"
        );
    }
}

#[test]
fn a_different_version_is_rejected() {
    let mut bytes = sample().to_vec().unwrap();
    bytes[0] = PROTOCOL_VERSION.wrapping_add(1);
    assert_eq!(
        Pdu::decode(&bytes),
        Err(WireError::Version {
            got: PROTOCOL_VERSION.wrapping_add(1),
            expected: PROTOCOL_VERSION,
        })
    );
}

#[test]
fn an_unknown_kind_is_rejected() {
    let mut bytes = sample().to_vec().unwrap();
    bytes[9] = 0xff;
    assert_eq!(Pdu::decode(&bytes), Err(WireError::UnknownKind(0xff)));
}

#[test]
fn a_declared_length_beyond_the_buffer_is_rejected() {
    let pdu = sample();
    let mut bytes = pdu.to_vec().unwrap();
    let inflated = (pdu.payload.len() as u16) + 10;
    bytes[20..22].copy_from_slice(&inflated.to_le_bytes());
    assert_eq!(
        Pdu::decode(&bytes),
        Err(WireError::PayloadLen {
            declared: inflated as usize,
            available: bytes.len() - OVERHEAD,
        })
    );
}

#[test]
fn trailing_bytes_are_rejected() {
    let mut bytes = sample().to_vec().unwrap();
    bytes.extend_from_slice(&[0, 0, 0]);
    assert_eq!(Pdu::decode(&bytes), Err(WireError::TrailingBytes(3)));
}

#[test]
fn a_payload_beyond_the_length_field_does_not_encode() {
    let pdu = Pdu {
        payload: vec![0u8; MAX_PAYLOAD + 1],
        ..sample()
    };
    assert_eq!(
        pdu.to_vec(),
        Err(WireError::PayloadTooLarge {
            got: MAX_PAYLOAD + 1,
            max: MAX_PAYLOAD,
        })
    );
}

/// The property that genuinely matters in this domain.
///
/// CRC32 detects every single-bit error, but the PDU has structural fields
/// (version, kind, length) that are read *before* the CRC is checked. This test
/// walks every bit of an encoded frame exhaustively and demands that flipping it
/// produces an error — whether from the CRC or from structure. It is the
/// guarantee that no corrupt frame is ever accepted as good.
#[test]
fn no_flipped_bit_passes_for_good() {
    let pdu = sample();
    let clean = pdu.to_vec().unwrap();

    for byte_idx in 0..clean.len() {
        for bit in 0..8 {
            let mut corrupt = clean.clone();
            corrupt[byte_idx] ^= 1 << bit;

            match Pdu::decode(&corrupt) {
                Err(_) => {}
                Ok(recovered) => panic!(
                    "bit {bit} of byte {byte_idx} was flipped and decode() accepted it: {recovered}"
                ),
            }
        }
    }
}

/// Flipping two bits should not slip through either. CRC32 does not guarantee
/// this for arbitrary distances in general, but it does within its span; this
/// test empirically bounds the risk over a representative frame.
#[test]
fn no_pair_of_flipped_bits_passes_for_good() {
    let pdu = Pdu {
        payload: b"abcd".to_vec(),
        ..sample()
    };
    let clean = pdu.to_vec().unwrap();
    let total_bits = clean.len() * 8;

    for a in 0..total_bits {
        for b in (a + 1)..total_bits {
            let mut corrupt = clean.clone();
            corrupt[a / 8] ^= 1 << (a % 8);
            corrupt[b / 8] ^= 1 << (b % 8);

            assert!(
                Pdu::decode(&corrupt).is_err(),
                "bits {a} and {b} flipped together passed for good"
            );
        }
    }
}

fn kind_arb() -> impl Strategy<Value = PduKind> {
    prop_oneof![
        Just(PduKind::Hello),
        Just(PduKind::Capabilities),
        Just(PduKind::Data),
        Just(PduKind::Ack),
        Just(PduKind::Probe),
        Just(PduKind::ProbeResult),
        Just(PduKind::Complete),
        Just(PduKind::Cancel),
    ]
}

prop_compose! {
    fn pdu_arb()(
        session_id in any::<u64>(),
        kind in kind_arb(),
        flags in any::<u16>(),
        seq in any::<u32>(),
        ack in any::<u32>(),
        payload in prop::collection::vec(any::<u8>(), 0..3000),
    ) -> Pdu {
        Pdu { session_id, kind, flags: Flags(flags), seq, ack, payload }
    }
}

proptest! {
    /// Any representable PDU survives the round trip.
    #[test]
    fn arbitrary_round_trip(pdu in pdu_arb()) {
        let bytes = pdu.to_vec().unwrap();
        prop_assert_eq!(bytes.len(), pdu.encoded_len());
        prop_assert_eq!(Pdu::decode(&bytes).unwrap(), pdu);
    }

    /// Truncation anywhere is detected. A partially read optical frame is a real
    /// case, not a hypothetical one.
    #[test]
    fn truncation_is_always_detected(pdu in pdu_arb(), cut in 0usize..4096) {
        let bytes = pdu.to_vec().unwrap();
        let cut = cut % bytes.len().max(1);
        prop_assert!(Pdu::decode(&bytes[..cut]).is_err());
    }

    /// The payload starts exactly where the header says. Guards against a layout
    /// change shifting the payload without anyone noticing.
    #[test]
    fn the_payload_sits_where_the_header_says(pdu in pdu_arb()) {
        let bytes = pdu.to_vec().unwrap();
        let end = HEADER_LEN + pdu.payload.len();
        prop_assert_eq!(&bytes[HEADER_LEN..end], &pdu.payload[..]);
    }
}
