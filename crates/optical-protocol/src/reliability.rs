//! How delivery of the whole object is guaranteed.
//!
//! There are two strategies, chosen per profile rather than by conviction:
//!
//! - **Fountain (RaptorQ).** The sender emits coded symbols continuously and
//!   waits for nothing. The receiver reconstructs once it has gathered enough,
//!   regardless of *which* ones. This removes the optical round trip, which is
//!   the dominant cost in this medium — showing a QR code, capturing it,
//!   decoding it and answering with another QR code costs hundreds of
//!   milliseconds.
//! - **ARQ.** Sliding window with selective retransmission. Every
//!   acknowledgement costs a full round trip, but it gives fine control and
//!   wastes no bandwidth when the channel is clean.
//!
//! Sender and receiver are separate traits on purpose. They are asymmetric
//! roles — with fountain coding the sender needs to know nothing about the
//! receiver until the very end — and folding them into one trait would leave
//! half the methods empty in each implementation.

pub mod arq;
pub mod fountain;

use crate::wire::Flags;

/// Which strategy a transfer uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// RaptorQ. No ordering, no explicit retransmission, no waiting.
    Fountain,
    /// Sliding window with selective retransmission.
    Arq,
}

impl Mode {
    /// The flag a data PDU of this mode must carry.
    #[must_use]
    pub const fn flag(self) -> Flags {
        match self {
            Self::Fountain => Flags::FOUNTAIN,
            Self::Arq => Flags::NONE,
        }
    }
}

/// A piece of the object ready to travel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// Identifier. Goes in `Pdu::seq`.
    ///
    /// Under ARQ this is the chunk index and is dense. Under fountain coding it
    /// is the coded symbol identifier and grows without bound: the sender can
    /// generate far more symbols than the object has chunks, and that is the
    /// mechanism rather than a defect.
    pub id: u32,
    pub bytes: Vec<u8>,
}

/// How much is left. Feeds the progress bar, and lets the multiplexer judge
/// whether this transfer is still worth spending bandwidth on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    /// Units already resolved: acknowledged chunks, or useful symbols gathered.
    pub have: u64,
    /// Units needed to finish.
    pub need: u64,
}

impl Progress {
    /// Completed fraction in 0..=1. Returns 1 when nothing is needed, so an
    /// empty object does not sit forever at 0%.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        if self.need == 0 {
            return 1.0;
        }
        (self.have as f32 / self.need as f32).min(1.0)
    }
}

/// What the receiver tells the sender.
///
/// Travels in the payload of an `Ack` PDU. The two modes need to say different
/// things, and forcing them into a shared shape would make one of them lie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feedback {
    /// ARQ: everything below `cumulative` is in hand, and of what follows the
    /// listed indices are missing.
    Selective {
        cumulative: u32,
        missing: Vec<u32>,
        /// How many more symbols the receiver's window will accept.
        window: u16,
    },
    /// Fountain: the sender only cares whether it can stop. The count of
    /// gathered symbols is for estimating what is left, not for deciding what to
    /// resend — under fountain coding nothing specific is resent.
    Fountain { complete: bool, received: u32 },
}

/// Wire tags for the two feedback dialects.
const FB_SELECTIVE: u8 = 1;
const FB_FOUNTAIN: u8 = 2;

impl Feedback {
    /// Serializes for travel in the payload of an `Ack` PDU.
    ///
    /// The gap list arrives already bounded by whoever produced it (see
    /// [`arq::MAX_MISSING_REPORTED`]); nothing is trimmed here, because
    /// silently dropping gaps at the last moment would leave the sender
    /// believing it had already sent them all.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Selective {
                cumulative,
                missing,
                window,
            } => {
                out.push(FB_SELECTIVE);
                out.extend_from_slice(&cumulative.to_le_bytes());
                out.extend_from_slice(&window.to_le_bytes());
                let n = u16::try_from(missing.len()).unwrap_or(u16::MAX);
                out.extend_from_slice(&n.to_le_bytes());
                for id in missing.iter().take(n as usize) {
                    out.extend_from_slice(&id.to_le_bytes());
                }
            }
            Self::Fountain { complete, received } => {
                out.push(FB_FOUNTAIN);
                out.push(u8::from(*complete));
                out.extend_from_slice(&received.to_le_bytes());
            }
        }
        out
    }

    /// Interprets what arrived in an `Ack` PDU.
    ///
    /// Returns `None` for anything that does not add up. The CRC already
    /// discarded corrupt frames, so reaching here with garbage means the peer
    /// speaks a different dialect — and that is handled by ignoring the message,
    /// not by tearing down the session.
    #[must_use]
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let (&tag, rest) = buf.split_first()?;
        match tag {
            FB_SELECTIVE => {
                if rest.len() < 8 {
                    return None;
                }
                let cumulative = u32::from_le_bytes(rest[0..4].try_into().ok()?);
                let window = u16::from_le_bytes(rest[4..6].try_into().ok()?);
                let n = u16::from_le_bytes(rest[6..8].try_into().ok()?) as usize;
                let ids = &rest[8..];
                if ids.len() != n * 4 {
                    return None;
                }
                let missing = ids
                    .chunks_exact(4)
                    .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                Some(Self::Selective {
                    cumulative,
                    missing,
                    window,
                })
            }
            FB_FOUNTAIN => {
                if rest.len() != 5 {
                    return None;
                }
                Some(Self::Fountain {
                    complete: rest[0] != 0,
                    received: u32::from_le_bytes(rest[1..5].try_into().ok()?),
                })
            }
            _ => None,
        }
    }
}

/// Why an incoming symbol could not be taken in.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecvError {
    #[error("symbol of {got} B, expected {expected}")]
    SymbolSize { got: usize, expected: usize },

    #[error("identifier {id} falls outside the object ({chunks} chunks)")]
    OutOfRange { id: u32, chunks: u32 },
}

/// The side that holds the object and pays it out.
pub trait Sender {
    /// Next piece to transmit, capped at `max_payload` bytes.
    ///
    /// Returning `None` means "nothing to send right now", not "finished": under
    /// ARQ the window may be full awaiting acknowledgement. Use
    /// [`Sender::is_complete`] to learn whether it is done.
    fn next_symbol(&mut self, max_payload: usize) -> Option<Symbol>;

    /// Takes in what the receiver reported.
    fn on_feedback(&mut self, feedback: &Feedback);

    /// The receiver has the whole object and emission can stop.
    fn is_complete(&self) -> bool;

    fn progress(&self) -> Progress;
}

/// The side that gathers pieces and reassembles.
pub trait Receiver {
    /// Takes in a received symbol.
    fn on_symbol(&mut self, symbol: &Symbol) -> Result<(), RecvError>;

    /// What to tell the sender right now.
    fn feedback(&self) -> Feedback;

    /// Yields the reconstructed object, once.
    ///
    /// Deliberately consuming: reconstructing a multi-megabyte object is not
    /// free, and handing it back by reference would invite copying it on every
    /// progress query.
    fn take_object(&mut self) -> Option<Vec<u8>>;

    fn progress(&self) -> Progress;
}
