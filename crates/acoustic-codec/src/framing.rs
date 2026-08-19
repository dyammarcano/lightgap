//! Delimiting acoustic frames.
//!
//! The optical channel gets framing for free: a QR code either decodes to a
//! complete payload or it does not. Audio has no such boundary — it is a
//! continuous stream, and the receiver has to be told where a frame starts and
//! how long it runs.
//!
//! The preamble handles "where it starts" (see [`crate::fsk::PREAMBLE`]). This
//! module handles "how long", with a length field and its own checksum.
//!
//! **Why a separate checksum from the PDU's.** The length is read before
//! anything else and decides how much is read at all. A corrupt length would
//! have the receiver consume the wrong number of symbols and desynchronise for
//! the rest of the stream, which the PDU's CRC — validated afterwards, on data
//! that was already framed wrongly — cannot protect against.

use crate::fsk::{bits_to_bytes, bytes_to_bits};

/// Bytes of framing overhead: two for the length, one for its checksum.
pub const FRAME_HEADER_LEN: usize = 3;

/// Largest payload a frame can carry.
///
/// Small on purpose. The acoustic channel runs at roughly 100 bits per second,
/// so a 256-byte payload already takes twenty seconds. Anything larger is not a
/// frame, it is an outage waiting to happen — and this channel exists to carry
/// acknowledgements, not files.
pub const MAX_ACOUSTIC_PAYLOAD: usize = 256;

/// Why a frame could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FramingError {
    #[error("{got} bits is not enough for a header")]
    TooShort { got: usize },

    #[error("length checksum failed: header says {declared} B")]
    BadLengthChecksum { declared: usize },

    #[error("declares {declared} B, over the {max} B limit")]
    TooLong { declared: usize, max: usize },

    #[error("declares {declared} B but only {available} arrived")]
    Truncated { declared: usize, available: usize },

    #[error("payload of {got} B is over the {max} B limit")]
    PayloadTooLarge { got: usize, max: usize },
}

/// Checksum over the two length bytes.
///
/// Deliberately not a CRC: over two bytes a CRC buys almost nothing that a
/// well-chosen mix does not, and this has to be cheap enough to run on every
/// candidate offset during synchronisation.
fn length_checksum(lo: u8, hi: u8) -> u8 {
    // Two different odd multipliers so that swapping the bytes changes the
    // result. A plain XOR would not notice.
    lo.wrapping_mul(31).wrapping_add(hi.wrapping_mul(131)) ^ 0xa5
}

/// Wraps a payload for acoustic transmission.
pub struct Framer;

impl Framer {
    /// Builds the bit sequence for a payload, header included.
    ///
    /// The preamble is not included: [`crate::fsk::modulate_frame`] adds it, so
    /// that framing and modulation stay separable and each can be tested alone.
    pub fn encode(payload: &[u8]) -> Result<Vec<bool>, FramingError> {
        if payload.len() > MAX_ACOUSTIC_PAYLOAD {
            return Err(FramingError::PayloadTooLarge {
                got: payload.len(),
                max: MAX_ACOUSTIC_PAYLOAD,
            });
        }

        let len = payload.len() as u16;
        let [lo, hi] = len.to_le_bytes();
        let mut bytes = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
        bytes.push(lo);
        bytes.push(hi);
        bytes.push(length_checksum(lo, hi));
        bytes.extend_from_slice(payload);

        Ok(bytes_to_bits(&bytes))
    }

    /// How many bits a payload of this size will occupy, header included.
    ///
    /// Needed before demodulating, because the receiver has to know how many
    /// symbols to ask for. In practice the receiver demodulates a header's worth
    /// first, reads the length, then asks for the rest.
    #[must_use]
    pub fn encoded_bits(payload_len: usize) -> usize {
        (FRAME_HEADER_LEN + payload_len) * 8
    }

    /// Bits needed to read just the header.
    #[must_use]
    pub const fn header_bits() -> usize {
        FRAME_HEADER_LEN * 8
    }

    /// Reads the declared payload length from a header's worth of bits.
    ///
    /// Split out from [`Framer::decode`] because the receiver genuinely needs it
    /// first: it cannot know how long to keep listening until it has read this.
    pub fn decode_length(bits: &[bool]) -> Result<usize, FramingError> {
        if bits.len() < Self::header_bits() {
            return Err(FramingError::TooShort { got: bits.len() });
        }
        let header = bits_to_bytes(&bits[..Self::header_bits()]);
        let (lo, hi, check) = (header[0], header[1], header[2]);
        let declared = u16::from_le_bytes([lo, hi]) as usize;

        if check != length_checksum(lo, hi) {
            return Err(FramingError::BadLengthChecksum { declared });
        }
        if declared > MAX_ACOUSTIC_PAYLOAD {
            return Err(FramingError::TooLong {
                declared,
                max: MAX_ACOUSTIC_PAYLOAD,
            });
        }
        Ok(declared)
    }

    /// Reads a whole frame.
    pub fn decode(bits: &[bool]) -> Result<Vec<u8>, FramingError> {
        let declared = Self::decode_length(bits)?;
        let needed = Self::encoded_bits(declared);
        if bits.len() < needed {
            return Err(FramingError::Truncated {
                declared,
                available: bits.len().saturating_sub(Self::header_bits()) / 8,
            });
        }
        let bytes = bits_to_bytes(&bits[Self::header_bits()..needed]);
        Ok(bytes)
    }
}
