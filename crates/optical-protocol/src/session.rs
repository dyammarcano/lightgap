//! Session state machine.
//!
//! Deliberately small. The original sketch for this project put
//! `AudioNoiseMeasurement`, `AudioFrequencySweep` and friends in as *session*
//! states, which ties the session to audio: adding a third medium would force an
//! edit here. In this design calibration belongs to each channel, which carries
//! its own lifecycle, and the session only knows whether there is a peer,
//! whether it is negotiating, and whether it is transferring.
//!
//! It performs no I/O: you hand it PDUs, ask what to transmit, and tell it that
//! time has passed. The caller owns the clock.

use core::fmt;
use core::time::Duration;

use crate::wire::{Flags, Pdu, PduKind};

/// Peer identifier, drawn when the application starts.
///
/// Sixteen bytes so two instances do not collide by accident: with fewer, a
/// collision would leave leader election without a tie-break and both machines
/// would sit waiting for each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId(pub [u8; 16]);

impl PeerId {
    #[must_use]
    pub const fn from_bytes(b: [u8; 16]) -> Self {
        Self(b)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Four bytes are enough to tell peers apart in a log line.
        for b in &self.0[..4] {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// Who drives the session.
///
/// Two identical applications facing each other need a tie-break: if neither
/// starts calibration they wait forever, and if both emit the acoustic sweep at
/// once each microphone picks up its own speaker and the measurement is
/// worthless. The rule is the lower `PeerId` wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Sequences calibration and fixes the session identifier.
    Leader,
    /// Follows the leader's pacing.
    Follower,
}

/// Where the session stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Emitting `Hello` and looking for someone across the way.
    Discovering,
    /// Both sides have seen each other; roles are assigned.
    Peered,
    /// Agreeing on channel profiles.
    Negotiating,
    /// Moving data.
    Active,
    /// Closing by mutual agreement.
    Closing,
    /// Finished.
    Closed,
}

/// What happened, for whoever drives the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The other side was seen and roles were assigned.
    PeerDiscovered { peer: PeerId, role: Role },
    /// Profile negotiation began.
    NegotiationStarted,
    /// Ready to transfer.
    Ready,
    /// The peer has been quiet for too long.
    PeerLost,
    /// Session over, with or without agreement.
    Closed,
}

/// How often `Hello` repeats while looking for a peer.
///
/// Slow on purpose: during discovery nobody is framed yet, and a QR code that
/// changes quickly is harder to latch onto than one that sits still for half a
/// second.
pub const HELLO_INTERVAL: Duration = Duration::from_millis(500);

/// With no news for this long, the peer is given up for lost.
///
/// Generous relative to the `Hello` rate: an optical link loses frames in bursts
/// — a passing hand, a reflection — and cutting at the first burst would have
/// the session collapsing constantly.
pub const PEER_TIMEOUT: Duration = Duration::from_secs(5);

/// The session.
#[derive(Debug)]
pub struct Session {
    local: PeerId,
    remote: Option<PeerId>,
    session_id: u64,
    state: State,
    role: Option<Role>,
    now: Duration,
    last_rx: Option<Duration>,
    next_hello: Duration,
    pending: Option<Pdu>,
}

impl Session {
    #[must_use]
    pub fn new(local: PeerId) -> Self {
        Self {
            local,
            remote: None,
            session_id: 0,
            state: State::Discovering,
            role: None,
            now: Duration::ZERO,
            last_rx: None,
            next_hello: Duration::ZERO,
            pending: None,
        }
    }

    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    #[must_use]
    pub fn role(&self) -> Option<Role> {
        self.role
    }

    #[must_use]
    pub fn peer(&self) -> Option<PeerId> {
        self.remote
    }

    /// Agreed session identifier. Zero while there is no peer.
    #[must_use]
    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    #[must_use]
    pub fn local(&self) -> PeerId {
        self.local
    }

    /// Derives the session identifier from the two peer identifiers.
    ///
    /// Deterministic and symmetric: both sides arrive at the same number without
    /// negotiating it, so no extra exchange is needed and the leader does not
    /// have to impose one. Mixed with the same constant `SimPair` uses to
    /// separate seeds, chosen for its good bit dispersion.
    fn derive_session_id(a: &PeerId, b: &PeerId) -> u64 {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let mut acc: u64 = 0x9e37_79b9_7f4a_7c15;
        for byte in lo.0.iter().chain(hi.0.iter()) {
            acc ^= u64::from(*byte);
            acc = acc.wrapping_mul(0x0100_0000_01b3);
        }
        // Zero is reserved for "no session yet".
        acc | 1
    }

    fn hello(&self) -> Pdu {
        Pdu {
            session_id: self.session_id,
            kind: PduKind::Hello,
            flags: Flags::SYN,
            seq: 0,
            ack: 0,
            payload: self.local.0.to_vec(),
        }
    }

    /// Takes in a received PDU.
    pub fn handle_incoming(&mut self, pdu: &Pdu) -> Vec<Event> {
        if self.state == State::Closed {
            return Vec::new();
        }
        self.last_rx = Some(self.now);
        let mut events = Vec::new();

        match pdu.kind {
            PduKind::Hello => {
                let Ok(bytes) = <[u8; 16]>::try_from(pdu.payload.as_slice()) else {
                    // A `Hello` carrying a differently sized identifier belongs
                    // to another protocol version. Ignore it: there is no way to
                    // assign roles against a peer whose identifier is
                    // unintelligible.
                    return events;
                };
                let remote = PeerId::from_bytes(bytes);
                if remote == self.local {
                    // Seeing yourself — a mirror, or your own screen in frame —
                    // is not discovering a peer.
                    return events;
                }

                if self.remote != Some(remote) {
                    self.remote = Some(remote);
                    self.session_id = Self::derive_session_id(&self.local, &remote);
                    let role = if self.local < remote {
                        Role::Leader
                    } else {
                        Role::Follower
                    };
                    self.role = Some(role);
                    self.state = State::Peered;
                    events.push(Event::PeerDiscovered { peer: remote, role });
                }
            }

            PduKind::Capabilities if self.state == State::Peered => {
                self.state = State::Negotiating;
                events.push(Event::NegotiationStarted);
            }

            PduKind::Cancel => {
                self.state = State::Closed;
                events.push(Event::Closed);
            }

            _ => {}
        }

        events
    }

    /// What to transmit now, if anything is due.
    pub fn poll_transmit(&mut self) -> Option<Pdu> {
        if let Some(pdu) = self.pending.take() {
            return Some(pdu);
        }
        // `Hello` keeps repeating after a peer is found: the other side may not
        // have seen ours yet, and discovery is not symmetric in time.
        if matches!(self.state, State::Discovering | State::Peered) && self.now >= self.next_hello {
            self.next_hello = self.now + HELLO_INTERVAL;
            return Some(self.hello());
        }
        None
    }

    /// Advances the clock. Returns whatever the passage of time triggered.
    pub fn handle_timeout(&mut self, now: Duration) -> Vec<Event> {
        self.now = now;
        let mut events = Vec::new();

        if matches!(self.state, State::Closed | State::Discovering) {
            return events;
        }

        if let Some(last) = self.last_rx {
            if now.saturating_sub(last) >= PEER_TIMEOUT {
                self.remote = None;
                self.role = None;
                self.session_id = 0;
                self.state = State::Discovering;
                self.last_rx = None;
                // Resume announcing immediately: whoever just lost the peer is
                // the one in the biggest hurry to be found again.
                self.next_hello = now;
                events.push(Event::PeerLost);
            }
        }

        events
    }

    /// Declares profiles agreed and moves to transferring.
    ///
    /// Decided by the calibration layer, not by the session: the session does
    /// not know what a visual or acoustic profile is, and teaching it would
    /// reintroduce exactly the coupling this design avoids.
    pub fn mark_ready(&mut self) -> Vec<Event> {
        if matches!(self.state, State::Peered | State::Negotiating) {
            self.state = State::Active;
            return vec![Event::Ready];
        }
        Vec::new()
    }

    /// Closes the session and queues the notice to the peer.
    pub fn close(&mut self) -> Vec<Event> {
        if self.state == State::Closed {
            return Vec::new();
        }
        self.pending = Some(Pdu {
            session_id: self.session_id,
            kind: PduKind::Cancel,
            flags: Flags::FIN,
            seq: 0,
            ack: 0,
            payload: Vec::new(),
        });
        self.state = State::Closed;
        vec![Event::Closed]
    }
}
