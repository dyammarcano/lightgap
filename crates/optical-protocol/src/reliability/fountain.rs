//! Fountain coding (RaptorQ): the sender waits for nobody.
//!
//! The sender generates coded symbols without end. The receiver reconstructs as
//! soon as it has gathered enough, and it does not matter *which* ones: any
//! sufficiently large subset works. That removes the optical round trip, which
//! is the dominant cost in this medium — showing a QR code, capturing it,
//! decoding it and answering with another QR code costs hundreds of
//! milliseconds.
//!
//! The practical consequence: there are no retransmissions to request, no gaps
//! to track, no window to manage. The only return message that matters is
//! "done, stop".
//!
//! The sender does need that confirmation: without it, it would emit forever. It
//! is the single point where fountain coding depends on the return path, and it
//! is why the design replicates that message across both channels when two are
//! available.

use std::collections::VecDeque;

use raptorq::{Decoder, Encoder, EncodingPacket, ObjectTransmissionInformation};

use super::{Feedback, Progress, Receiver, RecvError, Sender, Symbol};

/// Bytes RaptorQ prepends to every symbol (the `PayloadId`).
///
/// It matters for sizing: if the channel allows a payload of P bytes, the usable
/// symbol size is P − 4.
pub const PACKET_ID_LEN: usize = 4;

/// How many repair symbols are generated per batch and per block.
///
/// Generating them one at a time would waste the encoder's setup; generating
/// them a thousand at a time would hold memory that may never be needed if the
/// receiver confirms early.
const REPAIR_BATCH: u32 = 64;

/// Usable symbol size within a payload of `max_payload` bytes.
///
/// Nothing is trimmed to any alignment: [`plan`] builds the OTI choosing an
/// alignment that fits the size, so any value works and not a byte is wasted per
/// frame. (`ObjectTransmissionInformation::with_defaults` *did* round down to
/// multiples of 8 — that was the source of a bug where the receiver validated
/// against the requested size and rejected **every** symbol.)
///
/// Returns `None` if there is no room left for even one byte of data.
#[must_use]
pub fn symbol_size_for(max_payload: usize) -> Option<u16> {
    let usable = u16::try_from(max_payload.checked_sub(PACKET_ID_LEN)?).ok()?;
    (usable > 0).then_some(usable)
}

/// The most source symbols a single block may contain.
///
/// This number decides decoding cost, and it is not a minor detail: RaptorQ
/// solves each block by Gaussian elimination over GF(256), which grows far
/// worse than linearly in K. Measured in this project, letting a 5 MB object
/// fall into a single block of ~6000 symbols cost over nine minutes of CPU to
/// reconstruct. Split into blocks of ~1000 it drops to seconds, at the price of
/// slightly worse coding efficiency.
///
/// It is a user-experience trade-off: nobody waits nine minutes staring at a
/// stalled bar after holding two laptops face to face.
pub const MAX_SYMBOLS_PER_BLOCK: u32 = 1024;

/// Transmission parameters for an object.
///
/// These **travel the wire**, in the transfer metadata: twelve bytes, once. An
/// earlier version derived them on both sides to save those bytes, but it was a
/// bad trade — it pinned the block count to whatever `with_defaults` decided,
/// which is precisely the parameter that has to be tunable. Twelve bytes per
/// transfer do not compare to minutes of waiting.
///
/// # Panics
/// With a `total_len` of zero: RaptorQ divides by the symbol count and blows up
/// inside the library with a message that says nothing about where it came from.
/// The normal sender and receiver paths never call it — an empty object is
/// handled without touching RaptorQ at all.
#[must_use]
pub fn plan(total_len: u64, symbol_size: u16) -> ObjectTransmissionInformation {
    assert!(
        total_len > 0,
        "RaptorQ does not accept empty objects; handle them before getting here"
    );
    assert!(symbol_size > 0, "symbol size cannot be zero");

    // The alignment has to divide the symbol size: it is a precondition that
    // `ObjectTransmissionInformation::new` asserts on its own, and 1 always
    // satisfies it.
    let alignment: u8 = if symbol_size.is_multiple_of(8) { 8 } else { 1 };

    let total_symbols = total_len.div_ceil(u64::from(symbol_size));
    let blocks = total_symbols.div_ceil(u64::from(MAX_SYMBOLS_PER_BLOCK));
    // `source_blocks` is a u8; beyond 255 blocks we accept larger blocks rather
    // than produce an invalid OTI.
    let blocks = blocks.clamp(1, 255) as u8;

    ObjectTransmissionInformation::new(total_len, symbol_size, blocks, 1, alignment)
}

/// How many source symbols an object has. The theoretical minimum to gather; in
/// practice a few more are needed.
fn source_symbols(total_len: u64, symbol_size: u16) -> u32 {
    if symbol_size == 0 {
        return 0;
    }
    total_len.div_ceil(u64::from(symbol_size)) as u32
}

/// The side that holds the object and emits symbols without pause.
pub struct FountainSender {
    /// `None` for an empty object.
    ///
    /// Not an optimization: `raptorq` divides by the symbol count when
    /// constructed, and with zero length it panics inside the library. An empty
    /// file is a legitimate thing to transfer, so the encoder simply never comes
    /// into existence.
    encoder: Option<Encoder>,
    symbol_size: u16,
    source_symbols: u32,
    pending: VecDeque<Vec<u8>>,
    /// Identifier of the next repair symbol to generate.
    next_repair_id: u32,
    source_emitted: bool,
    emitted: u32,
    peer_received: u32,
    complete: bool,
}

impl FountainSender {
    #[must_use]
    pub fn new(object: &[u8], symbol_size: u16) -> Self {
        let base = Self {
            encoder: None,
            symbol_size,
            source_symbols: 0,
            pending: VecDeque::new(),
            next_repair_id: 0,
            source_emitted: false,
            emitted: 0,
            peer_received: 0,
            complete: false,
        };

        // An empty object never reaches RaptorQ: `plan` divides by the symbol
        // count and panics at zero length.
        if object.is_empty() {
            return base;
        }

        let config = plan(object.len() as u64, symbol_size);
        // The effective size is set by the OTI, not by what was requested: the
        // OTI may adjust it, and using the requested value would throw off every
        // derived calculation.
        let effective = config.symbol_size();
        Self {
            encoder: Some(Encoder::new(object, config)),
            symbol_size: effective,
            source_symbols: source_symbols(object.len() as u64, effective),
            ..base
        }
    }

    /// How many symbols have been emitted in total. Under fountain coding this
    /// can far exceed the source symbol count, and that is the mechanism working
    /// rather than a symptom.
    #[must_use]
    pub fn emitted(&self) -> u32 {
        self.emitted
    }

    #[must_use]
    pub fn source_symbols(&self) -> u32 {
        self.source_symbols
    }

    /// The **effective** symbol size, as fixed by the OTI. May differ from what
    /// was requested at construction.
    #[must_use]
    pub fn symbol_size(&self) -> u16 {
        self.symbol_size
    }

    /// Bytes each serialized symbol occupies, identifier included. This is what
    /// has to fit in a PDU payload.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        usize::from(self.symbol_size) + PACKET_ID_LEN
    }

    /// Transmission parameters to send in the metadata.
    ///
    /// `None` for an empty object: there is no plan to send, and the receiver
    /// resolves it knowing only that the length is zero.
    #[must_use]
    pub fn oti_bytes(&self) -> Option<[u8; 12]> {
        self.encoder.as_ref().map(|e| e.get_config().serialize())
    }

    /// Refills the queue. Source symbols first — they are the cheapest to decode
    /// — and repair symbols without end from there on.
    fn refill(&mut self) {
        let Some(encoder) = self.encoder.as_ref() else {
            return;
        };

        if !self.source_emitted {
            self.source_emitted = true;
            for packet in encoder.get_encoded_packets(0) {
                self.pending.push_back(packet.serialize());
            }
            if !self.pending.is_empty() {
                return;
            }
        }

        let start = self.next_repair_id;
        self.next_repair_id = self.next_repair_id.saturating_add(REPAIR_BATCH);
        for block in encoder.get_block_encoders() {
            for packet in block.repair_packets(start, REPAIR_BATCH) {
                self.pending.push_back(packet.serialize());
            }
        }
    }
}

impl Sender for FountainSender {
    fn next_symbol(&mut self, max_payload: usize) -> Option<Symbol> {
        if self.complete {
            return None;
        }
        // An empty object has nothing to emit; without this exit the refill
        // would spin forever producing empty batches.
        if self.source_symbols == 0 {
            return None;
        }

        if self.pending.is_empty() {
            self.refill();
        }

        let bytes = self.pending.front()?;
        if bytes.len() > max_payload {
            return None;
        }

        let bytes = self.pending.pop_front()?;
        let id = self.emitted;
        self.emitted = self.emitted.saturating_add(1);
        Some(Symbol { id, bytes })
    }

    fn on_feedback(&mut self, feedback: &Feedback) {
        // ARQ feedback on a fountain transfer indicates a negotiation failure.
        // Ignored rather than allowed to tear down the session.
        let Feedback::Fountain { complete, received } = feedback else {
            return;
        };
        self.peer_received = self.peer_received.max(*received);
        if *complete {
            self.complete = true;
        }
    }

    fn is_complete(&self) -> bool {
        self.complete || self.source_symbols == 0
    }

    fn progress(&self) -> Progress {
        // The sender's progress is what the receiver says it holds, not what the
        // sender has emitted: under fountain coding, emitting more is not
        // advancing.
        Progress {
            have: u64::from(self.peer_received.min(self.source_symbols)),
            need: u64::from(self.source_symbols),
        }
    }
}

/// The side that gathers symbols until it can reconstruct.
pub struct FountainReceiver {
    /// `None` for an empty object, for the same reason as in the sender:
    /// constructing the decoder at zero length makes `raptorq` divide by zero.
    decoder: Option<Decoder>,
    symbol_size: u16,
    source_symbols: u32,
    received: u32,
    object: Option<Vec<u8>>,
    /// Kept separate from `object` on purpose: `take_object` empties the object,
    /// and if `is_complete` depended on it the receiver would declare itself
    /// incomplete right after handing over the result — and its feedback would
    /// tell the sender to keep emitting.
    complete: bool,
    taken: bool,
}

impl FountainReceiver {
    #[must_use]
    pub fn new(total_len: u64, symbol_size: u16) -> Self {
        // An empty object is already reconstructed, and the decoder cannot be
        // built for it anyway: `plan` panics at zero length. Handling it here
        // avoids both problems.
        if total_len == 0 {
            return Self {
                decoder: None,
                symbol_size,
                source_symbols: 0,
                received: 0,
                object: Some(Vec::new()),
                complete: true,
                taken: false,
            };
        }
        Self::from_config(plan(total_len, symbol_size))
    }

    /// Builds from the parameters the sender transmitted.
    ///
    /// This is the preferred path: using the sender's exact plan removes any
    /// possibility of the two sides splitting the object differently.
    #[must_use]
    pub fn from_oti_bytes(oti: &[u8; 12]) -> Self {
        Self::from_config(ObjectTransmissionInformation::deserialize(oti))
    }

    fn from_config(config: ObjectTransmissionInformation) -> Self {
        let total_len = config.transfer_length();
        // The effective size is set by the OTI, not by what was requested.
        // Counting source symbols with the requested value would give a
        // different count than the sender's, and progress would lie in the
        // dangerous direction: low.
        let effective = config.symbol_size();
        Self {
            decoder: Some(Decoder::new(config)),
            symbol_size: effective,
            source_symbols: source_symbols(total_len, effective),
            received: 0,
            object: None,
            complete: false,
            taken: false,
        }
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// How many symbols have been taken in, useful or not.
    #[must_use]
    pub fn received(&self) -> u32 {
        self.received
    }

    /// The **effective** symbol size, as fixed by the OTI.
    #[must_use]
    pub fn symbol_size(&self) -> u16 {
        self.symbol_size
    }

    /// How many source symbols the object has according to the plan. The
    /// theoretical minimum to gather; in practice a few more are needed.
    #[must_use]
    pub fn source_symbols_expected(&self) -> u32 {
        self.source_symbols
    }

    /// Bytes each serialized symbol must carry, identifier included.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        usize::from(self.symbol_size) + PACKET_ID_LEN
    }
}

impl Receiver for FountainReceiver {
    fn on_symbol(&mut self, symbol: &Symbol) -> Result<(), RecvError> {
        // `EncodingPacket::deserialize` indexes the first four bytes without
        // checking them: a shorter buffer panics. A truncated symbol is
        // something this medium genuinely produces, so it is filtered here
        // before the library ever sees it.
        let expected = usize::from(self.symbol_size) + PACKET_ID_LEN;
        if symbol.bytes.len() != expected {
            return Err(RecvError::SymbolSize {
                got: symbol.bytes.len(),
                expected,
            });
        }

        self.received = self.received.saturating_add(1);

        if self.complete {
            // Already reconstructed; feeding the decoder further adds nothing
            // and costs time.
            return Ok(());
        }

        let Some(decoder) = self.decoder.as_mut() else {
            return Ok(());
        };
        let packet = EncodingPacket::deserialize(&symbol.bytes);
        if let Some(obj) = decoder.decode(packet) {
            self.object = Some(obj);
            self.complete = true;
        }
        Ok(())
    }

    fn feedback(&self) -> Feedback {
        Feedback::Fountain {
            complete: self.complete,
            received: self.received,
        }
    }

    fn take_object(&mut self) -> Option<Vec<u8>> {
        if self.taken {
            return None;
        }
        let obj = self.object.take()?;
        self.taken = true;
        Some(obj)
    }

    fn progress(&self) -> Progress {
        if self.complete {
            return Progress {
                have: u64::from(self.source_symbols),
                need: u64::from(self.source_symbols),
            };
        }
        Progress {
            have: u64::from(self.received.min(self.source_symbols)),
            need: u64::from(self.source_symbols),
        }
    }
}
