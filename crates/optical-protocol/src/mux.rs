//! Deciding which channel carries what.
//!
//! With one channel this layer is trivial. With two it is where the whole
//! multimodal idea either pays off or does not: the visual channel carries
//! volume, the acoustic one carries signalling, and when either degrades the
//! other has to pick up its traffic without the transfer noticing.
//!
//! The routing rule is not "use the fastest". It is **match the message to what
//! the channel is good at**. An acknowledgement is 12 bytes and needs to arrive
//! soon; a data symbol is 900 bytes and can wait. Sending acknowledgements over
//! the visual channel costs a full optical round trip — display a QR code,
//! capture it, decode it — which is the single largest latency in this system.

use std::collections::VecDeque;

use crate::channel::{ChannelCaps, ChannelHealth, ChannelId};
use crate::wire::{Pdu, PduKind};

/// How urgent a message is.
///
/// Ordered so that `Control > Metadata > Data`, which is also the order in which
/// losing one hurts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Bulk transfer data. Losing one costs a retransmission.
    Data,
    /// Transfer parameters, hashes, probe results. Losing one stalls a phase.
    Metadata,
    /// Handshake, acknowledgement, abort. Losing one can hang the session.
    Control,
}

impl Priority {
    /// Which class a PDU belongs to.
    #[must_use]
    pub const fn of(pdu: &Pdu) -> Self {
        match pdu.kind {
            PduKind::Data => Self::Data,
            PduKind::Capabilities | PduKind::Probe | PduKind::ProbeResult => Self::Metadata,
            PduKind::Beacon
            | PduKind::Hello
            | PduKind::Ack
            | PduKind::Complete
            | PduKind::Cancel => Self::Control,
        }
    }

    /// Whether this class is worth sending over both channels at once.
    ///
    /// Only control traffic. Duplicating data would halve throughput to buy
    /// redundancy the reliability layer already provides more cheaply, whereas a
    /// lost `Cancel` or `Complete` can leave both sides waiting on each other
    /// indefinitely.
    #[must_use]
    pub const fn worth_duplicating(self) -> bool {
        matches!(self, Self::Control)
    }
}

/// One channel as the scheduler sees it.
#[derive(Debug, Clone, Copy)]
pub struct ChannelSlot {
    pub caps: ChannelCaps,
    pub health: ChannelHealth,
    /// Whether the channel can carry anything right now. Driven by the
    /// channel's own lifecycle, which the scheduler does not own.
    pub usable: bool,
}

impl ChannelSlot {
    /// A rough figure of merit for carrying bulk data: throughput, discounted by
    /// how much of what arrives has to be thrown away.
    #[must_use]
    pub fn bulk_score(&self) -> f64 {
        if !self.usable {
            return 0.0;
        }
        let quality = 1.0 - f64::from(self.health.rejection_rate());
        self.caps.nominal_bps as f64 * quality.max(0.0)
    }

    /// A figure of merit for carrying urgent, small messages: low latency first,
    /// throughput barely at all.
    ///
    /// The two scores genuinely disagree, and that disagreement is the whole
    /// point. The visual channel wins on throughput by a wide margin and loses
    /// on latency by a wide margin, so a single "which channel is better" number
    /// would be wrong for one of the two jobs.
    #[must_use]
    pub fn control_score(&self) -> f64 {
        if !self.usable {
            return 0.0;
        }
        let quality = 1.0 - f64::from(self.health.rejection_rate());
        let latency_ms = self.caps.nominal_latency.as_secs_f64() * 1000.0;
        quality.max(0.0) / (1.0 + latency_ms / 100.0)
    }
}

/// Chooses channels for outgoing messages.
#[derive(Debug, Default)]
pub struct Scheduler {
    slots: Vec<ChannelSlot>,
}

impl Scheduler {
    #[must_use]
    pub fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Adds or replaces a channel by id.
    pub fn upsert(&mut self, slot: ChannelSlot) {
        match self.slots.iter_mut().find(|s| s.caps.id == slot.caps.id) {
            Some(existing) => *existing = slot,
            None => self.slots.push(slot),
        }
    }

    pub fn remove(&mut self, id: ChannelId) {
        self.slots.retain(|s| s.caps.id != id);
    }

    #[must_use]
    pub fn slots(&self) -> &[ChannelSlot] {
        &self.slots
    }

    /// How many channels can currently carry anything.
    #[must_use]
    pub fn usable_count(&self) -> usize {
        self.slots.iter().filter(|s| s.usable).count()
    }

    fn best_by(&self, score: impl Fn(&ChannelSlot) -> f64) -> Option<ChannelId> {
        self.slots
            .iter()
            .filter(|s| s.usable)
            .map(|s| (s.caps.id, score(s)))
            .filter(|(_, sc)| *sc > 0.0)
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(core::cmp::Ordering::Equal))
            .map(|(id, _)| id)
    }

    /// Which channel should carry a message of this class.
    ///
    /// Returns `None` when nothing usable is left, which is a real state rather
    /// than an error: it is what the session sees when both channels have gone
    /// down and it needs to stop pretending it can send.
    #[must_use]
    pub fn route(&self, class: Priority) -> Option<ChannelId> {
        match class {
            Priority::Data => self.best_by(ChannelSlot::bulk_score),
            Priority::Metadata | Priority::Control => self.best_by(ChannelSlot::control_score),
        }
    }

    /// Every channel a message of this class should go out on.
    ///
    /// For control traffic with more than one channel up, that is all of them:
    /// the duplicate costs a handful of bytes and removes a class of hang that
    /// is very hard to diagnose from the outside.
    #[must_use]
    pub fn route_all(&self, class: Priority) -> Vec<ChannelId> {
        if !class.worth_duplicating() {
            return self.route(class).into_iter().collect();
        }
        self.slots
            .iter()
            .filter(|s| s.usable && s.control_score() > 0.0)
            .map(|s| s.caps.id)
            .collect()
    }

    /// Whether a message of this class must respect a size limit on its route.
    #[must_use]
    pub fn mtu_for(&self, id: ChannelId) -> Option<usize> {
        self.slots
            .iter()
            .find(|s| s.caps.id == id)
            .map(|s| s.caps.mtu)
    }
}

/// How many recently seen messages to remember.
///
/// Bounded because a transfer runs for tens of thousands of frames and an
/// unbounded set would grow without limit over a long session. The window only
/// has to outlast the difference in arrival time between two copies of the same
/// message, which is one channel's latency, not the whole transfer.
pub const DEDUP_WINDOW: usize = 512;

/// Suppresses the second copy of a duplicated message.
///
/// Needed because duplicating control traffic across channels means the peer
/// receives it twice. Acting on a `Cancel` twice is harmless, but acting on a
/// `Hello` twice would restart discovery, and counting an `Ack` twice would
/// corrupt the progress estimate.
#[derive(Debug)]
pub struct Dedup {
    /// Insertion order, for eviction.
    order: VecDeque<(u64, u32, u8)>,
    seen: std::collections::HashSet<(u64, u32, u8)>,
    capacity: usize,
}

impl Default for Dedup {
    fn default() -> Self {
        Self::new(DEDUP_WINDOW)
    }
}

impl Dedup {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            order: VecDeque::with_capacity(capacity),
            seen: std::collections::HashSet::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    /// Records a PDU and reports whether it is new.
    ///
    /// The key includes the kind as well as session and sequence, because the
    /// two travel independently: a `Hello` and an `Ack` may legitimately share a
    /// sequence number, and treating them as the same message would silently
    /// drop one of them.
    pub fn accept(&mut self, pdu: &Pdu) -> bool {
        let key = (pdu.session_id, pdu.seq, pdu.kind as u8);
        if !self.seen.insert(key) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn clear(&mut self) {
        self.order.clear();
        self.seen.clear();
    }
}
