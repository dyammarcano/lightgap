//! Wire format for the protocol data unit (PDU).
//!
//! Hand-rolled with a fixed layout rather than `bincode`: bincode makes no
//! stability promise about its representation across versions, and this is a
//! format that two different machines — potentially running binaries built
//! months apart — have to interpret identically.
//!
//! Little-endian throughout.
//!
//! ```text
//! off  size  field
//!   0     1  version
//!   1     8  session_id
//!   9     1  kind
//!  10     2  flags
//!  12     4  seq
//!  16     4  ack
//!  20     2  payload_len
//!  22     N  payload
//! 22+N     4  crc32   (over everything before it)
//! ```
//!
//! On the size of `session_id`: 8 bytes per PDU over a ~900 byte payload is
//! 0.9%. Trimming it to u32 would save 0.4% in exchange for maintaining two
//! distinct identifiers — the full one in the session, the truncated one on the
//! wire. This design saves bytes when it is free (the ChaCha20 nonce is derived
//! and never transmitted) but not when it costs clarity.

use core::fmt;

/// Protocol version this binary speaks.
pub const PROTOCOL_VERSION: u8 = 1;

/// Header bytes preceding the payload.
pub const HEADER_LEN: usize = 22;
/// CRC bytes following the payload.
pub const TRAILER_LEN: usize = 4;
/// Fixed cost of framing a payload.
pub const OVERHEAD: usize = HEADER_LEN + TRAILER_LEN;

/// The largest value the `payload_len` field can express.
///
/// The real per-link limit is far smaller and comes from the channel MTU (a QR
/// code tops out around 2 KB). The wire format deliberately knows nothing about
/// QR codes: that separation is exactly what lets the acoustic channel be added
/// without touching this layer.
pub const MAX_PAYLOAD: usize = u16::MAX as usize;

/// What a PDU is. One byte, with explicit values because they travel the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PduKind {
    /// Presence announcement, carrying the `peer_id` used to elect a leader.
    Hello = 1,
    /// Capabilities and offered profiles.
    Capabilities = 2,
    /// Transfer data.
    Data = 3,
    /// Acknowledgement, cumulative or selective depending on `flags`.
    Ack = 4,
    /// Calibration probe carrying a known pattern.
    Probe = 5,
    /// Metrics measured over a probe.
    ProbeResult = 6,
    /// The transfer finished and verified.
    Complete = 7,
    /// Explicit abort.
    Cancel = 8,
    /// Bare presence: the peer identifier and nothing else.
    ///
    /// Its whole purpose is to be readable when nothing else is. A code's
    /// module count grows with the bytes in it, and its modules shrink to fit
    /// the same screen — so the smallest frame the protocol can express is the
    /// one that survives the worst camera, the longest distance and the dimmest
    /// display. That is exactly the situation at the start, before either end
    /// knows anything about what the other can read.
    ///
    /// Everything that costs bytes — key material, measurements, capabilities —
    /// waits for `Hello`, which is only sent once a peer has been found and the
    /// link is therefore known to carry something.
    Beacon = 9,
}

impl PduKind {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::Hello,
            2 => Self::Capabilities,
            3 => Self::Data,
            4 => Self::Ack,
            5 => Self::Probe,
            6 => Self::ProbeResult,
            7 => Self::Complete,
            8 => Self::Cancel,
            9 => Self::Beacon,
            _ => return None,
        })
    }
}

/// Control bits. A newtype rather than the `bitflags` crate, to avoid taking a
/// dependency for six constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Flags(pub u16);

impl Flags {
    pub const NONE: Self = Self(0);
    /// Opens the session.
    pub const SYN: Self = Self(1 << 0);
    /// Closes the session.
    pub const FIN: Self = Self(1 << 1);
    /// The `ack` field carries meaningful data.
    pub const ACK_VALID: Self = Self(1 << 2);
    /// The payload is encrypted with the session key.
    pub const ENCRYPTED: Self = Self(1 << 3);
    /// The payload is a fountain-coded symbol, not an ordered chunk.
    pub const FOUNTAIN: Self = Self(1 << 4);
    /// A copy of a critical PDU also sent over the other channel.
    pub const DUPLICATED: Self = Self(1 << 5);

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl core::ops::BitOr for Flags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// A decoded protocol data unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pdu {
    pub session_id: u64,
    pub kind: PduKind,
    pub flags: Flags,
    pub seq: u32,
    pub ack: u32,
    pub payload: Vec<u8>,
}

/// Why a buffer is not a valid PDU.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("buffer of {got} B, at least {need} needed")]
    TooShort { got: usize, need: usize },

    #[error("protocol version {got}, this binary speaks {expected}")]
    Version { got: u8, expected: u8 },

    #[error("unknown PDU kind: {0}")]
    UnknownKind(u8),

    #[error("declares {declared} B of payload but the buffer only holds {available}")]
    PayloadLen { declared: usize, available: usize },

    #[error("{0} B left over after the CRC")]
    TrailingBytes(usize),

    #[error("CRC mismatch: computed {computed:08x}, received {received:08x}")]
    Crc { computed: u32, received: u32 },

    #[error("payload of {got} B, the format maximum is {max}")]
    PayloadTooLarge { got: usize, max: usize },
}

impl Pdu {
    /// How many bytes this PDU occupies once encoded.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        OVERHEAD + self.payload.len()
    }

    /// Serializes onto the end of `out`.
    ///
    /// Only fails if the payload does not fit the length field; everything else
    /// is infallible by construction.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        if self.payload.len() > MAX_PAYLOAD {
            return Err(WireError::PayloadTooLarge {
                got: self.payload.len(),
                max: MAX_PAYLOAD,
            });
        }

        let start = out.len();
        out.reserve(self.encoded_len());

        out.push(PROTOCOL_VERSION);
        out.extend_from_slice(&self.session_id.to_le_bytes());
        out.push(self.kind as u8);
        out.extend_from_slice(&self.flags.0.to_le_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.ack.to_le_bytes());
        out.extend_from_slice(&(self.payload.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.payload);

        let crc = crc32fast::hash(&out[start..]);
        out.extend_from_slice(&crc.to_le_bytes());
        Ok(())
    }

    /// Serializes into a fresh `Vec`. Convenience for tests and for callers that
    /// are not reusing a buffer.
    pub fn to_vec(&self) -> Result<Vec<u8>, WireError> {
        let mut out = Vec::with_capacity(self.encoded_len());
        self.encode(&mut out)?;
        Ok(out)
    }

    /// Interprets `buf` as exactly one PDU.
    ///
    /// Strict about leftover bytes: a framed channel — a QR code hands back
    /// exactly the bytes that were encoded — has no reason to produce them, so
    /// their presence signals corruption rather than padding.
    pub fn decode(buf: &[u8]) -> Result<Self, WireError> {
        if buf.len() < OVERHEAD {
            return Err(WireError::TooShort {
                got: buf.len(),
                need: OVERHEAD,
            });
        }

        let version = buf[0];
        if version != PROTOCOL_VERSION {
            return Err(WireError::Version {
                got: version,
                expected: PROTOCOL_VERSION,
            });
        }

        let kind = PduKind::from_u8(buf[9]).ok_or(WireError::UnknownKind(buf[9]))?;
        let payload_len = u16::from_le_bytes([buf[20], buf[21]]) as usize;

        let total = OVERHEAD + payload_len;
        if buf.len() < total {
            return Err(WireError::PayloadLen {
                declared: payload_len,
                available: buf.len() - OVERHEAD,
            });
        }
        if buf.len() > total {
            return Err(WireError::TrailingBytes(buf.len() - total));
        }

        // The CRC is verified before trusting any field beyond the minimum
        // needed to know where the PDU ends.
        let crc_at = HEADER_LEN + payload_len;
        let received = u32::from_le_bytes([
            buf[crc_at],
            buf[crc_at + 1],
            buf[crc_at + 2],
            buf[crc_at + 3],
        ]);
        let computed = crc32fast::hash(&buf[..crc_at]);
        if computed != received {
            return Err(WireError::Crc { computed, received });
        }

        Ok(Self {
            session_id: u64::from_le_bytes(buf[1..9].try_into().expect("8 B")),
            kind,
            flags: Flags(u16::from_le_bytes([buf[10], buf[11]])),
            seq: u32::from_le_bytes(buf[12..16].try_into().expect("4 B")),
            ack: u32::from_le_bytes(buf[16..20].try_into().expect("4 B")),
            payload: buf[HEADER_LEN..crc_at].to_vec(),
        })
    }
}

impl fmt::Display for Pdu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} seq={} ack={} flags={:#06x} payload={}B",
            self.kind,
            self.seq,
            self.ack,
            self.flags.0,
            self.payload.len()
        )
    }
}
