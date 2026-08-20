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

use crate::crypto::{Pairing, ANNOUNCEMENT_LEN};
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
    /// Key agreement completed; the authentication string can be shown.
    Paired,
    /// A fresh ephemeral key was drawn because nobody had answered.
    PairingRotated,
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

/// Bytes in a `Beacon` payload: the peer identifier, and nothing else.
pub const BEACON_LEN: usize = 16;

/// Bytes in a `Hello` payload: peer identifier, key material, then one byte
/// saying how well this end is reading the other.
///
/// That last byte is what makes transmit sizing possible at all. How well I
/// read your code says nothing about how well you read mine — different camera,
/// different display, and the link is measured separately in each direction. So
/// each end reports what it observes, and the *other* end is the one that acts
/// on it. Without it, sizing would have to assume the two directions are alike,
/// which is the one thing this design has said from the start that they are not.
pub const HELLO_LEN: usize = 16 + ANNOUNCEMENT_LEN + 2;

/// How long one pairing code stays valid before a fresh ephemeral key is drawn.
///
/// Long enough that the code is readable — photographable, even, which the
/// animated data stream is not — and short enough that a code left facing a
/// window does not stay usable all afternoon. Rotation stops the moment a peer
/// is seen: past that point a changing key would only be a way to lose an
/// agreement that already succeeded.
pub const PAIRING_ROTATION: Duration = Duration::from_secs(30);

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
    next_rotation: Duration,
    /// Whether the peer has demonstrably seen us.
    ///
    /// Not the same question as whether we can see them, and the difference is
    /// the useful one: an optical link fails one direction at a time, and
    /// knowing which end is aimed wrong is most of knowing what to do about it.
    peer_sees_us: bool,
    /// How well this end is reading the peer, as it will be reported to them.
    read_quality: u8,
    /// How well the peer says it is reading this end, if it has said.
    peer_read_quality: Option<u8>,
    /// Whether this end is finding any code at all in what it photographs.
    ///
    /// Separate from the quality figure, and the separation is the whole point.
    /// Finding a code and failing to read it, and finding nothing whatsoever,
    /// both come back as a low number — but they are opposite faults with
    /// opposite remedies. One means the peer's display is too much for this
    /// camera: too bright, too dense, blooming until the modules run together.
    /// The other means it is too little: too dim, too small, too far. A peer
    /// told only "badly" has no way to know which way to move, and half its
    /// guesses will make things worse.
    sees_anything: bool,
    peer_sees_anything: Option<bool>,
    pairing: Pairing,
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
            next_rotation: PAIRING_ROTATION,
            peer_sees_us: false,
            read_quality: 0,
            peer_read_quality: None,
            sees_anything: false,
            peer_sees_anything: None,
            pairing: Pairing::new(),
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

    /// Records how well this end is reading the peer, for the peer to act on.
    ///
    /// Quantised to a byte because it rides in every announcement and this link
    /// charges by the byte; a 1-in-254 resolution is far finer than the
    /// measurement behind it deserves.
    ///
    /// Zero is reserved for "nothing measured yet" and the scale starts at one.
    /// The distinction is not pedantic: a session that has just started has read
    /// nothing, and a plain zero would be indistinguishable from having measured
    /// carefully and found the peer unreadable. The peer acts on this figure by
    /// shrinking what it transmits, so the two readings send it in opposite
    /// directions at exactly the moment it can least afford it.
    pub fn set_read_quality(&mut self, fraction: f32) {
        self.read_quality = 1 + (fraction.clamp(0.0, 1.0) * 254.0).round() as u8;
    }

    /// Records whether this end is finding any code at all to try to read.
    pub fn set_sees_anything(&mut self, seen: bool) {
        self.sees_anything = seen;
    }

    /// Whether the peer is finding any code at all in what it photographs.
    ///
    /// `Some(false)` is the useful one: the peer is looking and finding nothing,
    /// which points at this end's display being too faint or too small rather
    /// than too much for its camera.
    #[must_use]
    pub const fn peer_sees_anything(&self) -> Option<bool> {
        self.peer_sees_anything
    }

    /// How well the peer says it is reading this end.
    ///
    /// `None` until the peer has measured something. This, and not our own read
    /// rate, is what should size what we transmit.
    #[must_use]
    pub fn peer_read_quality(&self) -> Option<f32> {
        match self.peer_read_quality {
            None | Some(0) => None,
            Some(q) => Some(f32::from(q - 1) / 254.0),
        }
    }

    /// Whether we can see the peer.
    #[must_use]
    pub const fn sees_peer(&self) -> bool {
        self.remote.is_some()
    }

    /// Whether the peer has proved it can see us.
    #[must_use]
    pub const fn peer_sees_us(&self) -> bool {
        self.peer_sees_us
    }

    /// The digits for the user to compare across the two displays, once both
    /// sides have each other's key material.
    #[must_use]
    pub fn short_auth_string(&self) -> Option<&str> {
        self.pairing.short_auth_string()
    }

    #[must_use]
    pub fn is_paired(&self) -> bool {
        self.pairing.is_paired()
    }

    /// When the pairing code on screen stops being valid.
    ///
    /// `None` once a peer has been found, because rotation stops there.
    #[must_use]
    pub fn rotation_due(&self) -> Option<Duration> {
        (self.state == State::Discovering).then_some(self.next_rotation)
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

    /// Records a peer seen in any announcement, whichever shape it arrived in.
    ///
    /// A session identifier is derived from *both* identifiers, so a peer can
    /// only be carrying ours if it has read ours. Their first announcement
    /// carries zero, because at that moment they had not. Nothing extra goes on
    /// the wire to learn this: the proof was already in a field that had to be
    /// there anyway.
    fn observe_peer(&mut self, remote: PeerId, session_id: u64, events: &mut Vec<Event>) {
        self.peer_sees_us = session_id == Self::derive_session_id(&self.local, &remote);

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

    fn beacon(&self) -> Pdu {
        Pdu {
            session_id: self.session_id,
            kind: PduKind::Beacon,
            flags: Flags::SYN,
            seq: 0,
            ack: 0,
            payload: self.local.0.to_vec(),
        }
    }

    fn hello(&self) -> Pdu {
        Pdu {
            session_id: self.session_id,
            kind: PduKind::Hello,
            flags: Flags::SYN,
            seq: 0,
            ack: 0,
            payload: {
                let mut payload = Vec::with_capacity(HELLO_LEN);
                payload.extend_from_slice(&self.local.0);
                payload.extend_from_slice(&self.pairing.announcement());
                payload.push(self.read_quality);
                payload.push(u8::from(self.sees_anything));
                payload
            },
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
            PduKind::Beacon => {
                if pdu.payload.len() != BEACON_LEN {
                    return events;
                }
                let id: [u8; 16] = pdu.payload[..].try_into().expect("length checked");
                let remote = PeerId::from_bytes(id);
                if remote == self.local {
                    return events;
                }
                self.observe_peer(remote, pdu.session_id, &mut events);
                // No key material here, so no agreement. It arrives with the
                // `Hello` this discovery causes the peer to start sending.
            }

            PduKind::Hello => {
                if pdu.payload.len() != HELLO_LEN {
                    // A `Hello` of the wrong size belongs to another protocol
                    // version. Ignore it: there is no way to assign roles
                    // against a peer whose announcement is unintelligible.
                    return events;
                }
                // Named offsets rather than arithmetic against the total.
                //
                // The nonce used to be cut at `HELLO_LEN - 1`, which was right
                // until a byte was appended and then quietly asked for
                // seventeen. Every field here is at a fixed place; deriving one
                // of them from the length of the whole makes the last field a
                // load-bearing part of an unrelated one.
                const PUBLIC: usize = 16;
                const NONCE: usize = PUBLIC + 32;
                const QUALITY: usize = NONCE + 16;
                const SEES: usize = QUALITY + 1;

                let id: [u8; 16] = pdu.payload[..PUBLIC].try_into().expect("length checked");
                let peer_public: [u8; 32] = pdu.payload[PUBLIC..NONCE]
                    .try_into()
                    .expect("length checked");
                let peer_nonce: [u8; 16] = pdu.payload[NONCE..QUALITY]
                    .try_into()
                    .expect("length checked");
                self.peer_read_quality = Some(pdu.payload[QUALITY]);
                self.peer_sees_anything = Some(pdu.payload[SEES] != 0);
                let remote = PeerId::from_bytes(id);
                if remote == self.local {
                    // Seeing yourself — a mirror, or your own screen in frame —
                    // is not discovering a peer.
                    return events;
                }

                self.observe_peer(remote, pdu.session_id, &mut events);

                // Agree, or agree again if the peer is announcing material we
                // have not used yet. Re-running is cheap and it is what makes a
                // rotation on either side recoverable instead of a dead link.
                if !self.pairing.agreed_with(&peer_public) {
                    match self
                        .pairing
                        .agree(self.local, remote, &peer_public, &peer_nonce)
                    {
                        Ok(()) => events.push(Event::Paired),
                        Err(_) => {
                            // A key we cannot agree with is a peer that is
                            // broken or hostile. Stay discoverable rather than
                            // pretending to be paired.
                            self.pairing.forget();
                        }
                    }
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
        // Two shapes, and which one goes out is decided by whether anything is
        // known to be listening.
        //
        // The smallest frame the protocol can express, until the peer has
        // demonstrated it can read this end. Not until this end can read the
        // peer — that was the first version of this condition and it had the
        // test backwards.
        //
        // The two are different in exactly the case that matters. A good camera
        // looking at a poor one reads it immediately, moves to `Peered`, and on
        // the old rule started announcing at twice the density — aimed at the
        // camera that had not managed the sparse version yet and now had no
        // chance at all. Seeing is not evidence about being seen; the link is
        // measured separately in each direction, and this is that principle
        // applied to the announcement itself.
        //
        // `peer_sees_us` is proof, not inference: it is set when a peer sends an
        // identifier derived from both, which it can only have computed by
        // reading ours. Once that has happened the link has demonstrably
        // carried a beacon, and the full announcement — key material, and how
        // well this end is reading — is worth its extra modules.
        //
        // It keeps repeating after a peer is found because the other side may
        // not have seen ours yet: discovery is not symmetric in time.
        if matches!(self.state, State::Discovering | State::Peered) && self.now >= self.next_hello {
            self.next_hello = self.now + HELLO_INTERVAL;
            return Some(if self.peer_sees_us {
                self.hello()
            } else {
                self.beacon()
            });
        }
        None
    }

    /// Advances the clock. Returns whatever the passage of time triggered.
    pub fn handle_timeout(&mut self, now: Duration) -> Vec<Event> {
        self.now = now;
        let mut events = Vec::new();

        // Ahead of the early return below, because rotating only matters while
        // still looking — which is exactly the state that returns early.
        if self.state == State::Discovering && now >= self.next_rotation {
            self.pairing.rotate();
            self.next_rotation = now + PAIRING_ROTATION;
            events.push(Event::PairingRotated);
        }

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
                self.peer_sees_us = false;
                self.peer_read_quality = None;
                self.peer_sees_anything = None;
                // Keys agreed with a peer that is gone are not keys worth
                // keeping, and the next code shown should be a fresh one.
                self.pairing.rotate();
                self.next_rotation = now + PAIRING_ROTATION;
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
