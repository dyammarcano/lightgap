//! Sliding window with selective retransmission.
//!
//! The object is split into fixed-size chunks with dense indices. The sender
//! keeps a window of chunks in flight; the receiver acknowledges cumulatively
//! and also lists the gaps it can see beyond the cumulative point.
//!
//! The cost of this strategy on an optical channel is real: every
//! acknowledgement requires the receiver to display a QR code and the sender to
//! capture and decode it. That is why the window matters so much — with a window
//! of one this degenerates into stop-and-wait, which is what makes the link
//! unusable.

use super::{Feedback, Progress, Receiver, RecvError, Sender, Symbol};

/// How many gaps fit in one feedback message.
///
/// Bounded because a `Feedback` travels in a PDU payload, and that payload has
/// to fit inside a channel frame. An acknowledgement that does not fit in a QR
/// code is not an acknowledgement. When there are more gaps than this limit the
/// oldest are sent: those are the ones blocking the cumulative point.
pub const MAX_MISSING_REPORTED: usize = 32;

/// Initial window, in chunks. Calibration adjusts it.
pub const DEFAULT_WINDOW: u32 = 16;

/// The side that holds the object.
#[derive(Debug)]
pub struct ArqSender {
    object: Vec<u8>,
    chunk_size: usize,
    total_chunks: u32,
    /// First unacknowledged chunk.
    base: u32,
    /// First chunk never yet sent.
    next: u32,
    window: u32,
    /// Gaps the receiver asked for, in arrival order.
    retransmit: Vec<u32>,
}

impl ArqSender {
    /// # Panics
    /// If `chunk_size` is zero.
    #[must_use]
    pub fn new(object: Vec<u8>, chunk_size: usize) -> Self {
        assert!(chunk_size > 0, "chunk size cannot be zero");
        let total_chunks = object.len().div_ceil(chunk_size) as u32;
        Self {
            object,
            chunk_size,
            total_chunks,
            base: 0,
            next: 0,
            window: DEFAULT_WINDOW,
            retransmit: Vec::new(),
        }
    }

    #[must_use]
    pub fn total_chunks(&self) -> u32 {
        self.total_chunks
    }

    fn chunk(&self, id: u32) -> Option<Vec<u8>> {
        if id >= self.total_chunks {
            return None;
        }
        let start = id as usize * self.chunk_size;
        let end = (start + self.chunk_size).min(self.object.len());
        Some(self.object[start..end].to_vec())
    }
}

impl Sender for ArqSender {
    fn next_symbol(&mut self, max_payload: usize) -> Option<Symbol> {
        // Gaps first: they are what blocks the cumulative point, and until they
        // are filled the receiver cannot advance no matter what else arrives.
        if let Some(&id) = self.retransmit.first() {
            let bytes = self.chunk(id)?;
            // A chunk cannot be split: the index is the identity of the data. If
            // it does not fit, the profile is mis-negotiated, and sending
            // nothing beats sending something the receiver cannot reassemble.
            if bytes.len() > max_payload {
                return None;
            }
            self.retransmit.remove(0);
            return Some(Symbol { id, bytes });
        }

        if self.next < self.total_chunks && self.next < self.base.saturating_add(self.window) {
            let id = self.next;
            let bytes = self.chunk(id)?;
            if bytes.len() > max_payload {
                return None;
            }
            // State only advances once the symbol actually goes out.
            self.next += 1;
            return Some(Symbol { id, bytes });
        }

        None
    }

    fn on_feedback(&mut self, feedback: &Feedback) {
        let Feedback::Selective {
            cumulative,
            missing,
            window,
        } = feedback
        else {
            // Feedback from another mode: ignored rather than panicked on. A
            // peer speaking a different dialect is a negotiation failure, not a
            // reason to tear down the session.
            return;
        };

        self.base = self.base.max(*cumulative);
        if *window > 0 {
            self.window = u32::from(*window);
        }

        // Only gaps still beyond the cumulative point matter; earlier ones are
        // already accounted for.
        self.retransmit.retain(|id| *id >= self.base);
        for id in missing {
            if *id >= self.base && *id < self.total_chunks && !self.retransmit.contains(id) {
                self.retransmit.push(*id);
            }
        }
    }

    fn is_complete(&self) -> bool {
        self.base >= self.total_chunks
    }

    fn progress(&self) -> Progress {
        Progress {
            have: u64::from(self.base),
            need: u64::from(self.total_chunks),
        }
    }
}

/// The side that reassembles.
#[derive(Debug)]
pub struct ArqReceiver {
    buffer: Vec<u8>,
    received: Vec<bool>,
    chunk_size: usize,
    total_len: usize,
    total_chunks: u32,
    /// First missing chunk. Kept incrementally so as not to walk the whole map
    /// on every symbol.
    cumulative: u32,
    count: u32,
    taken: bool,
}

impl ArqReceiver {
    /// # Panics
    /// If `chunk_size` is zero.
    #[must_use]
    pub fn new(total_len: usize, chunk_size: usize) -> Self {
        assert!(chunk_size > 0, "chunk size cannot be zero");
        let total_chunks = total_len.div_ceil(chunk_size) as u32;
        Self {
            buffer: vec![0; total_len],
            received: vec![false; total_chunks as usize],
            chunk_size,
            total_len,
            total_chunks,
            cumulative: 0,
            count: 0,
            taken: false,
        }
    }

    /// Length chunk `id` should have. The last one is shorter unless the object
    /// is an exact multiple of the chunk size.
    fn expected_len(&self, id: u32) -> usize {
        let start = id as usize * self.chunk_size;
        (start + self.chunk_size).min(self.total_len) - start
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.count == self.total_chunks
    }
}

impl Receiver for ArqReceiver {
    fn on_symbol(&mut self, symbol: &Symbol) -> Result<(), RecvError> {
        if symbol.id >= self.total_chunks {
            return Err(RecvError::OutOfRange {
                id: symbol.id,
                chunks: self.total_chunks,
            });
        }

        let expected = self.expected_len(symbol.id);
        if symbol.bytes.len() != expected {
            return Err(RecvError::SymbolSize {
                got: symbol.bytes.len(),
                expected,
            });
        }

        let idx = symbol.id as usize;
        if self.received[idx] {
            // A duplicate. Not an error: the medium produces them on its own,
            // and on a channel where every QR code is held for several refreshes
            // it is the expected case.
            return Ok(());
        }

        let start = idx * self.chunk_size;
        self.buffer[start..start + expected].copy_from_slice(&symbol.bytes);
        self.received[idx] = true;
        self.count += 1;

        while (self.cumulative as usize) < self.received.len()
            && self.received[self.cumulative as usize]
        {
            self.cumulative += 1;
        }

        Ok(())
    }

    fn feedback(&self) -> Feedback {
        let mut missing = Vec::new();
        for (idx, got) in self
            .received
            .iter()
            .enumerate()
            .skip(self.cumulative as usize)
        {
            if !*got {
                missing.push(idx as u32);
                if missing.len() == MAX_MISSING_REPORTED {
                    break;
                }
            }
        }

        Feedback::Selective {
            cumulative: self.cumulative,
            missing,
            window: DEFAULT_WINDOW as u16,
        }
    }

    fn take_object(&mut self) -> Option<Vec<u8>> {
        if !self.is_complete() || self.taken {
            return None;
        }
        self.taken = true;
        Some(core::mem::take(&mut self.buffer))
    }

    fn progress(&self) -> Progress {
        Progress {
            have: u64::from(self.count),
            need: u64::from(self.total_chunks),
        }
    }
}
