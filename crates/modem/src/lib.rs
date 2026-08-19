//! The transfer engine: everything below, composed into something that moves a
//! file.
//!
//! Still sans-io. Frames go in, frames come out, and the caller owns the camera,
//! the display and the clock. That is what lets the whole engine — discovery,
//! metadata exchange, transfer, verification — be tested end to end against the
//! simulated channel and the synthetic camera, with no hardware anywhere.
//!
//! The Tauri application is a thin adapter over this: it turns camera frames
//! into `handle_frame` calls and `poll_frame` results into a QR code on screen.
//! Keeping the logic here rather than there is deliberate — a bug in the
//! transfer state machine should be reproducible in a unit test, not only by
//! holding two devices up.

pub mod metadata;

use std::collections::VecDeque;
use std::time::Duration;

use optical_protocol::reliability::fountain::{
    symbol_size_for, FountainReceiver, FountainSender, PACKET_ID_LEN,
};
use optical_protocol::reliability::{Feedback, Receiver, Sender, Symbol};
use optical_protocol::session::{Event as SessionEvent, PeerId, Role, Session, State};
use optical_protocol::wire::{Flags, Pdu, PduKind, OVERHEAD};

pub use metadata::{MetaError, Mode, TransferMeta};

/// What happened, for whoever is driving the modem.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The peer was found and roles assigned.
    PeerFound { peer: PeerId, role: Role },
    /// The peer announced an incoming file.
    IncomingFile { name: String, total_len: u64 },
    /// Progress on whichever transfer is running, in 0..=1.
    Progress { fraction: f32 },
    /// A file arrived and its hash matched.
    FileReceived { name: String, bytes: Vec<u8> },
    /// A file arrived and its hash did not match.
    ///
    /// Distinct from a transport failure on purpose: every frame passed its CRC,
    /// so this points at reassembly rather than at the medium, and reporting it
    /// as a generic failure would send someone looking in the wrong place.
    FileCorrupt { name: String },
    /// The peer confirmed it has the whole file.
    SendComplete,
    /// The peer went quiet for too long.
    PeerLost,
    /// The session ended.
    Closed,
}

/// Where a send stands.
#[derive(Debug)]
enum Sending {
    Idle,
    /// Announcing the file. Metadata repeats until the receiver acknowledges,
    /// because losing it once means losing the whole transfer.
    Announcing {
        meta: TransferMeta,
        object: Vec<u8>,
        acknowledged: bool,
    },
    Transferring {
        meta: TransferMeta,
        sender: FountainSender,
    },
    Done,
}

/// Where a receive stands.
#[derive(Debug)]
enum Receiving {
    Idle,
    Transferring {
        meta: TransferMeta,
        receiver: FountainReceiver,
    },
    Done,
}

/// Length of a BLAKE3 hash, which is how a metadata acknowledgement is
/// recognised.
const HASH_LEN: usize = 32;

/// How often the metadata frame is repeated while unacknowledged, in frames.
///
/// Every third frame. Often enough to survive a bad patch, rare enough not to
/// crowd out the data once the link is working.
const META_EVERY: u32 = 3;

/// How often the receiver broadcasts its feedback, in frames.
///
/// Periodic rather than in response to data. The receiver's display is always
/// showing something, so its state is being broadcast continuously; with
/// reactive acknowledgement, losing one would leave the sender waiting for a
/// message nobody is going to repeat.
const FEEDBACK_EVERY: u32 = 4;

/// The transfer engine.
pub struct Modem {
    session: Session,
    sending: Sending,
    receiving: Receiving,
    outgoing: VecDeque<Pdu>,
    frame_count: u32,
    /// Symbol size for this link, derived from the negotiated MTU.
    symbol_size: u16,
    peer_has_everything: bool,
    stats: Stats,
}

/// What the link has actually delivered.
///
/// Tracked here rather than left to the caller because the modem is the only
/// party that knows whether a frame was intelligible: the channel hands over
/// bytes, and only the wire layer can say whether they were a valid PDU. The UI
/// wants these numbers to show link quality, and the calibration layer wants
/// them to decide whether to back off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames handed to the modem.
    pub frames_seen: u64,
    /// Frames that failed validation and were discarded.
    ///
    /// A subset of `frames_seen`, for the same reason as in `ChannelHealth`:
    /// counting them separately would double-count and make a dead link look
    /// half-alive.
    pub frames_rejected: u64,
}

impl Stats {
    /// Fraction of frames discarded, in 0..=1. Zero with nothing seen, since a
    /// link nothing is known about is not a bad link.
    #[must_use]
    pub fn rejection_rate(&self) -> f32 {
        if self.frames_seen == 0 {
            return 0.0;
        }
        self.frames_rejected as f32 / self.frames_seen as f32
    }
}

impl Modem {
    /// # Panics
    /// If `mtu` leaves no room for a symbol after PDU and RaptorQ overhead.
    #[must_use]
    pub fn new(local: PeerId, mtu: usize) -> Self {
        let payload = mtu
            .checked_sub(OVERHEAD)
            .expect("the MTU must at least fit a PDU header");
        let symbol_size =
            symbol_size_for(payload).expect("the MTU must leave room for at least one symbol byte");

        Self {
            session: Session::new(local),
            sending: Sending::Idle,
            receiving: Receiving::Idle,
            outgoing: VecDeque::new(),
            frame_count: 0,
            symbol_size,
            peer_has_everything: false,
            stats: Stats::default(),
        }
    }

    /// What the link has delivered so far.
    #[must_use]
    pub fn stats(&self) -> Stats {
        self.stats
    }

    #[must_use]
    pub fn state(&self) -> State {
        self.session.state()
    }

    #[must_use]
    pub fn role(&self) -> Option<Role> {
        self.session.role()
    }

    #[must_use]
    pub fn symbol_size(&self) -> u16 {
        self.symbol_size
    }

    /// Usable payload bytes per frame at this link's MTU.
    #[must_use]
    pub fn payload_per_frame(&self) -> usize {
        usize::from(self.symbol_size) + PACKET_ID_LEN
    }

    /// Offers a file for transfer.
    ///
    /// Nothing goes out until a peer is found: announcing into an empty room
    /// would spend frames on a message nobody is listening for, and the display
    /// is the scarce resource here.
    pub fn send_file(&mut self, name: &str, object: Vec<u8>) {
        let sender = FountainSender::new(&object, self.symbol_size);
        let oti = sender.oti_bytes().unwrap_or([0u8; 12]);
        let meta = TransferMeta::for_object(name, &object, oti, Mode::Fountain);

        self.sending = Sending::Announcing {
            meta,
            object,
            acknowledged: false,
        };
        self.peer_has_everything = false;
    }

    /// Name of the file being sent, if any.
    ///
    /// The UI needs this to say what is in flight, and it is why the metadata is
    /// carried through the transferring state rather than dropped once the
    /// announcement is acknowledged.
    #[must_use]
    pub fn sending_file(&self) -> Option<&str> {
        match &self.sending {
            Sending::Announcing { meta, .. } | Sending::Transferring { meta, .. } => {
                Some(meta.name.as_str())
            }
            _ => None,
        }
    }

    /// Name of the file being received, if any.
    #[must_use]
    pub fn receiving_file(&self) -> Option<&str> {
        match &self.receiving {
            Receiving::Transferring { meta, .. } => Some(meta.name.as_str()),
            _ => None,
        }
    }

    /// Fraction of the outgoing transfer the peer has confirmed, in 0..=1.
    #[must_use]
    pub fn send_progress(&self) -> f32 {
        match &self.sending {
            Sending::Transferring { sender, .. } => sender.progress().fraction(),
            Sending::Done => 1.0,
            _ => 0.0,
        }
    }

    /// Fraction of the incoming transfer gathered, in 0..=1.
    #[must_use]
    pub fn receive_progress(&self) -> f32 {
        match &self.receiving {
            Receiving::Transferring { receiver, .. } => receiver.progress().fraction(),
            Receiving::Done => 1.0,
            Receiving::Idle => 0.0,
        }
    }

    /// Takes in one decoded frame.
    pub fn handle_frame(&mut self, frame: &[u8]) -> Vec<Event> {
        self.stats.frames_seen += 1;
        let Ok(pdu) = Pdu::decode(frame) else {
            // A frame that fails validation is discarded silently rather than
            // raised as an event. On this medium corrupt frames are routine, not
            // exceptional, and one event each would drown the caller during a bad
            // patch. It is counted instead, which is what the UI and the
            // calibration layer actually need.
            self.stats.frames_rejected += 1;
            return Vec::new();
        };
        self.handle_pdu(&pdu)
    }

    fn handle_pdu(&mut self, pdu: &Pdu) -> Vec<Event> {
        let mut events: Vec<Event> = self
            .session
            .handle_incoming(pdu)
            .into_iter()
            .filter_map(map_session_event)
            .collect();

        match pdu.kind {
            PduKind::Capabilities => events.extend(self.dispatch_capabilities(pdu)),
            PduKind::Data => events.extend(self.on_data(pdu)),
            PduKind::Ack => events.extend(self.on_ack(pdu)),
            PduKind::Complete if !self.peer_has_everything => {
                self.peer_has_everything = true;
                self.sending = Sending::Done;
                events.push(Event::SendComplete);
            }
            _ => {}
        }

        events
    }

    /// Capabilities frames carry either an announcement or a bare hash
    /// acknowledging one. They are told apart by size, and that decision lives
    /// in its own function rather than buried in the parser, because burying it
    /// would make it easy to break by accident.
    fn dispatch_capabilities(&mut self, pdu: &Pdu) -> Vec<Event> {
        if pdu.payload.len() == HASH_LEN {
            self.note_metadata_ack(&pdu.payload);
            return Vec::new();
        }
        self.on_metadata(pdu)
    }

    fn on_metadata(&mut self, pdu: &Pdu) -> Vec<Event> {
        let Ok(meta) = TransferMeta::decode(&pdu.payload) else {
            return Vec::new();
        };

        // Already receiving this exact object: re-announcements are expected,
        // since the sender repeats metadata until acknowledged.
        if let Receiving::Transferring { meta: current, .. } = &self.receiving {
            if current.hash == meta.hash {
                self.queue_metadata_ack(&meta);
                return Vec::new();
            }
        }
        if matches!(self.receiving, Receiving::Done) {
            self.queue_metadata_ack(&meta);
            return Vec::new();
        }

        let receiver = FountainReceiver::from_oti_bytes(&meta.oti);
        let mut events = vec![Event::IncomingFile {
            name: meta.name.clone(),
            total_len: meta.total_len,
        }];
        self.queue_metadata_ack(&meta);
        self.receiving = Receiving::Transferring { meta, receiver };

        // An empty file arrives complete: the receiver is born finished and no
        // data symbol will ever follow, because the sender has none to send.
        // Without this the transfer would sit in Transferring forever waiting
        // for a frame nobody is going to produce.
        events.extend(self.finish_if_complete());
        events
    }

    /// Completes the incoming transfer if the receiver has everything.
    ///
    /// Shared by the metadata path and the data path because either can be the
    /// moment it becomes true — normally the last data symbol, but for an empty
    /// file the announcement itself.
    fn finish_if_complete(&mut self) -> Vec<Event> {
        let Receiving::Transferring { meta, receiver } = &mut self.receiving else {
            return Vec::new();
        };
        if !receiver.is_complete() {
            return Vec::new();
        }
        let Some(object) = receiver.take_object() else {
            return Vec::new();
        };

        let name = meta.name.clone();
        let ok = meta.verify(&object);
        self.receiving = Receiving::Done;

        // Tell the sender to stop either way. If the object was corrupt, letting
        // it keep emitting would not help: fountain coding already delivered
        // enough symbols, so more of them cannot fix a reassembly fault.
        self.outgoing.push_back(Pdu {
            session_id: self.session.session_id(),
            kind: PduKind::Complete,
            flags: Flags::NONE,
            seq: 0,
            ack: 0,
            payload: Vec::new(),
        });

        if ok {
            vec![Event::FileReceived {
                name,
                bytes: object,
            }]
        } else {
            vec![Event::FileCorrupt { name }]
        }
    }

    /// Acknowledges metadata by echoing its hash, so the sender knows *which*
    /// announcement was heard rather than merely that something was.
    fn queue_metadata_ack(&mut self, meta: &TransferMeta) {
        self.outgoing.push_back(Pdu {
            session_id: self.session.session_id(),
            kind: PduKind::Capabilities,
            flags: Flags::ACK_VALID,
            seq: 0,
            ack: 0,
            payload: meta.hash.to_vec(),
        });
    }

    fn on_data(&mut self, pdu: &Pdu) -> Vec<Event> {
        let Receiving::Transferring { receiver, .. } = &mut self.receiving else {
            // Data before metadata. Not an error: the sender starts emitting as
            // soon as it can, and the announcement may simply not have landed
            // yet. Dropping these costs a few symbols, which fountain coding is
            // built to absorb.
            return Vec::new();
        };

        let symbol = Symbol {
            id: pdu.seq,
            bytes: pdu.payload.clone(),
        };
        if receiver.on_symbol(&symbol).is_err() {
            return Vec::new();
        }

        if !receiver.is_complete() {
            return vec![Event::Progress {
                fraction: receiver.progress().fraction(),
            }];
        }

        self.finish_if_complete()
    }

    fn on_ack(&mut self, pdu: &Pdu) -> Vec<Event> {
        if let Some(feedback) = Feedback::decode(&pdu.payload) {
            if let Sending::Transferring { sender, .. } = &mut self.sending {
                sender.on_feedback(&feedback);
                if sender.is_complete() {
                    self.sending = Sending::Done;
                    self.peer_has_everything = true;
                    return vec![Event::SendComplete];
                }
            }
        }
        Vec::new()
    }

    /// Advances the clock.
    pub fn tick(&mut self, now: Duration) -> Vec<Event> {
        self.session
            .handle_timeout(now)
            .into_iter()
            .filter_map(map_session_event)
            .collect()
    }

    /// The next frame to display, if there is one.
    ///
    /// The ordering here is the transfer's priority policy in miniature: session
    /// control first, then anything queued, then metadata while it is still
    /// unacknowledged, then feedback on a fixed cadence, and data last. Data is
    /// last because it is the only class that can be re-derived — a lost symbol
    /// is replaced by the next one, whereas a lost announcement stalls
    /// everything.
    pub fn poll_frame(&mut self) -> Option<Vec<u8>> {
        self.frame_count = self.frame_count.wrapping_add(1);

        if let Some(pdu) = self.session.poll_transmit() {
            return pdu.to_vec().ok();
        }
        if let Some(pdu) = self.outgoing.pop_front() {
            return pdu.to_vec().ok();
        }

        // Promote an announced transfer once the peer has confirmed it heard.
        if let Sending::Announcing {
            meta,
            object,
            acknowledged: true,
        } = &self.sending
        {
            let sender = FountainSender::new(object, self.symbol_size);
            self.sending = Sending::Transferring {
                meta: meta.clone(),
                sender,
            };
        }

        if let Sending::Announcing { meta, .. } = &self.sending {
            if self.frame_count.is_multiple_of(META_EVERY) {
                return self.metadata_frame(meta);
            }
        }

        if self.frame_count.is_multiple_of(FEEDBACK_EVERY) {
            if let Some(frame) = self.feedback_frame() {
                return Some(frame);
            }
        }

        let session_id = self.session.session_id();
        let max_payload = self.payload_per_frame();
        if let Sending::Transferring { sender, .. } = &mut self.sending {
            if let Some(symbol) = sender.next_symbol(max_payload) {
                return Pdu {
                    session_id,
                    kind: PduKind::Data,
                    flags: Flags::FOUNTAIN,
                    seq: symbol.id,
                    ack: 0,
                    payload: symbol.bytes,
                }
                .to_vec()
                .ok();
            }
        }

        // Nothing to send. The session still announces itself, so the peer does
        // not conclude we vanished.
        None
    }

    fn metadata_frame(&self, meta: &TransferMeta) -> Option<Vec<u8>> {
        Pdu {
            session_id: self.session.session_id(),
            kind: PduKind::Capabilities,
            flags: Flags::NONE,
            seq: 0,
            ack: 0,
            payload: meta.encode().ok()?,
        }
        .to_vec()
        .ok()
    }

    fn feedback_frame(&self) -> Option<Vec<u8>> {
        let Receiving::Transferring { receiver, .. } = &self.receiving else {
            return None;
        };
        Pdu {
            session_id: self.session.session_id(),
            kind: PduKind::Ack,
            flags: Flags::ACK_VALID,
            seq: 0,
            ack: 0,
            payload: receiver.feedback().encode(),
        }
        .to_vec()
        .ok()
    }

    /// Marks an announcement acknowledged. Called when the peer echoes the hash.
    fn note_metadata_ack(&mut self, hash: &[u8]) {
        if let Sending::Announcing {
            meta, acknowledged, ..
        } = &mut self.sending
        {
            if hash == meta.hash {
                *acknowledged = true;
            }
        }
    }
}

fn map_session_event(e: SessionEvent) -> Option<Event> {
    match e {
        SessionEvent::PeerDiscovered { peer, role } => Some(Event::PeerFound { peer, role }),
        SessionEvent::PeerLost => Some(Event::PeerLost),
        SessionEvent::Closed => Some(Event::Closed),
        SessionEvent::NegotiationStarted | SessionEvent::Ready => None,
    }
}
