//! Simulated link with loss, delay, jitter, duplication and corruption.
//!
//! It exists so the protocol core can be tested end to end with no cameras, no
//! displays and no second machine. A 5 MB transfer at 40% loss has to run inside
//! a unit test, in milliseconds.
//!
//! Two properties make that possible:
//!
//! - **Virtual time.** The caller drives the clock. A 200 ms delay does not cost
//!   200 ms of test time, it costs an addition.
//! - **Seeded randomness.** Same `seed`, same sequence of losses. A failure
//!   reproduces exactly, which is the difference between debugging and guessing.
//!
//! Reordering has no knob of its own: it emerges from jitter, as it does in the
//! real medium. If a frame leaves later but draws less jitter, it overtakes the
//! one before it. A separate "reorder" control would model something that does
//! not happen on its own.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use optical_protocol::channel::{
    Channel, ChannelCaps, ChannelError, ChannelHealth, ChannelId, Direction,
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// How the medium behaves.
///
/// The defaults describe a perfect link, so a test that only wants a reliable
/// pipe does not have to fill in six fields.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkConfig {
    /// Usable bytes per frame.
    pub mtu: usize,
    /// Probability a frame never arrives, in 0..=1.
    pub loss: f64,
    /// Probability a frame arrives twice.
    pub duplicate: f64,
    /// Probability a frame arrives with one bit changed.
    pub corrupt: f64,
    /// Base end-to-end delay.
    pub latency: Duration,
    /// Variation added to the delay, uniform over 0..=jitter. This is what
    /// produces reordering.
    pub jitter: Duration,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            mtu: 1024,
            loss: 0.0,
            duplicate: 0.0,
            corrupt: 0.0,
            latency: Duration::ZERO,
            jitter: Duration::ZERO,
        }
    }
}

impl LinkConfig {
    /// A perfect link with the given MTU.
    #[must_use]
    pub fn perfect(mtu: usize) -> Self {
        Self {
            mtu,
            ..Self::default()
        }
    }

    /// A plausible optical link: about 100 ms of flight time and noticeable
    /// jitter, because between showing a QR code and decoding it there are
    /// several display refreshes and several camera frames.
    #[must_use]
    pub fn optical(mtu: usize, loss: f64) -> Self {
        Self {
            mtu,
            loss,
            duplicate: 0.0,
            corrupt: 0.0,
            latency: Duration::from_millis(100),
            jitter: Duration::from_millis(60),
        }
    }

    #[must_use]
    pub fn with_corruption(mut self, corrupt: f64) -> Self {
        self.corrupt = corrupt;
        self
    }

    #[must_use]
    pub fn with_duplication(mut self, duplicate: f64) -> Self {
        self.duplicate = duplicate;
        self
    }
}

/// A frame waiting for its arrival time.
#[derive(Debug, Clone)]
struct InFlight {
    due: Duration,
    bytes: Vec<u8>,
    /// Emission order, so tests can detect reordering.
    emitted: u64,
}

/// Statistics of what the medium did with the frames. Used to check that the
/// simulator simulates what it claims to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkStats {
    pub offered: u64,
    pub dropped: u64,
    pub duplicated: u64,
    pub corrupted: u64,
    pub delivered: u64,
}

/// A one-directional pipe.
#[derive(Debug)]
struct Wire {
    cfg: LinkConfig,
    rng: ChaCha8Rng,
    inflight: Vec<InFlight>,
    ready: VecDeque<Vec<u8>>,
    stats: LinkStats,
    emitted: u64,
    /// Emission order of the last delivered frame, to count overtakes.
    last_delivered_order: Option<u64>,
    reorders: u64,
}

impl Wire {
    fn new(cfg: LinkConfig, seed: u64) -> Self {
        Self {
            cfg,
            rng: ChaCha8Rng::seed_from_u64(seed),
            inflight: Vec::new(),
            ready: VecDeque::new(),
            stats: LinkStats::default(),
            emitted: 0,
            last_delivered_order: None,
            reorders: 0,
        }
    }

    fn jittered(&mut self) -> Duration {
        if self.cfg.jitter.is_zero() {
            return self.cfg.latency;
        }
        let extra = self.rng.random_range(0..=self.cfg.jitter.as_nanos() as u64);
        self.cfg.latency + Duration::from_nanos(extra)
    }

    fn enqueue(&mut self, frame: &[u8], now: Duration) {
        self.stats.offered += 1;
        let order = self.emitted;
        self.emitted += 1;

        if self.rng.random::<f64>() < self.cfg.loss {
            self.stats.dropped += 1;
            return;
        }

        let mut copies = 1;
        if self.rng.random::<f64>() < self.cfg.duplicate {
            copies = 2;
            self.stats.duplicated += 1;
        }

        for _ in 0..copies {
            let mut bytes = frame.to_vec();
            if self.rng.random::<f64>() < self.cfg.corrupt && !bytes.is_empty() {
                let byte = self.rng.random_range(0..bytes.len());
                let bit = self.rng.random_range(0..8u8);
                bytes[byte] ^= 1 << bit;
                self.stats.corrupted += 1;
            }
            let due = now + self.jittered();
            self.inflight.push(InFlight {
                due,
                bytes,
                emitted: order,
            });
        }
    }

    /// Moves everything that should have arrived by now onto the ready queue.
    fn advance(&mut self, now: Duration) {
        // Stable by `due` so two frames sharing a deadline keep their emission
        // order; reordering must come from jitter, not from a container detail.
        self.inflight
            .sort_by(|a, b| a.due.cmp(&b.due).then(a.emitted.cmp(&b.emitted)));

        let split = self.inflight.partition_point(|f| f.due <= now);
        for f in self.inflight.drain(..split) {
            if let Some(prev) = self.last_delivered_order {
                if f.emitted < prev {
                    self.reorders += 1;
                }
            }
            self.last_delivered_order = Some(f.emitted.max(self.last_delivered_order.unwrap_or(0)));
            self.stats.delivered += 1;
            self.ready.push_back(f.bytes);
        }
    }
}

/// One end of the link: writes into one pipe and reads from the other.
pub struct SimEndpoint {
    tx: Rc<RefCell<Wire>>,
    rx: Rc<RefCell<Wire>>,
    caps: ChannelCaps,
    health: ChannelHealth,
    now: Duration,
}

impl SimEndpoint {
    /// Statistics of what this end has emitted.
    #[must_use]
    pub fn tx_stats(&self) -> LinkStats {
        self.tx.borrow().stats
    }

    /// Statistics of what this end has received.
    #[must_use]
    pub fn rx_stats(&self) -> LinkStats {
        self.rx.borrow().stats
    }

    /// How many times a frame arrived ahead of one emitted before it.
    #[must_use]
    pub fn rx_reorders(&self) -> u64 {
        self.rx.borrow().reorders
    }

    /// Marks a received frame as invalid. Driven by the layer above, since it is
    /// the only one that knows how to interpret the bytes.
    pub fn note_rejected(&mut self) {
        self.health.frames_rejected += 1;
    }

    /// Whether anything is still pending delivery. A test waiting for the link
    /// to drain needs to know without poking at internals.
    #[must_use]
    pub fn rx_idle(&self) -> bool {
        let w = self.rx.borrow();
        w.inflight.is_empty() && w.ready.is_empty()
    }
}

impl Channel for SimEndpoint {
    fn caps(&self) -> ChannelCaps {
        self.caps
    }

    fn health(&self) -> ChannelHealth {
        self.health
    }

    fn send_frame(&mut self, frame: &[u8]) -> Result<(), ChannelError> {
        if frame.len() > self.caps.mtu {
            return Err(ChannelError::OverMtu {
                got: frame.len(),
                mtu: self.caps.mtu,
            });
        }
        self.tx.borrow_mut().enqueue(frame, self.now);
        self.health.frames_sent += 1;
        Ok(())
    }

    fn recv_frame(&mut self) -> Option<Vec<u8>> {
        let frame = self.rx.borrow_mut().ready.pop_front();
        if frame.is_some() {
            self.health.frames_received += 1;
            self.health.last_rx = Some(self.now);
        }
        frame
    }

    fn advance(&mut self, now: Duration) {
        self.now = now;
        self.rx.borrow_mut().advance(now);
        // The outbound pipe advances too: there is an endpoint on the other side
        // reading it, and its clock may be running behind ours.
        self.tx.borrow_mut().advance(now);
    }
}

/// Two endpoints joined by two independent pipes.
///
/// Independent on purpose: the design allows for asymmetric links where one
/// direction works and the other does not. A single shared medium could not
/// express that.
pub struct SimPair {
    pub a: SimEndpoint,
    pub b: SimEndpoint,
}

impl SimPair {
    /// Builds a pair with the same configuration in both directions.
    #[must_use]
    pub fn new(cfg: LinkConfig, seed: u64) -> Self {
        Self::asymmetric(cfg.clone(), cfg, seed)
    }

    /// Builds a pair with a different configuration per direction.
    #[must_use]
    pub fn asymmetric(a_to_b: LinkConfig, b_to_a: LinkConfig, seed: u64) -> Self {
        let mtu_ab = a_to_b.mtu;
        let mtu_ba = b_to_a.mtu;

        // Different seeds per direction: with the same one, both directions
        // would drop frames at the same instants and the test would be measuring
        // a coincidence rather than the protocol.
        let ab = Rc::new(RefCell::new(Wire::new(a_to_b, seed)));
        let ba = Rc::new(RefCell::new(Wire::new(
            b_to_a,
            seed ^ 0x9e37_79b9_7f4a_7c15,
        )));

        let caps = |mtu: usize| ChannelCaps {
            id: ChannelId::Simulated,
            mtu,
            direction: Direction::Bidirectional,
            nominal_bps: 0,
            nominal_latency: Duration::ZERO,
        };

        Self {
            a: SimEndpoint {
                tx: Rc::clone(&ab),
                rx: Rc::clone(&ba),
                caps: caps(mtu_ab),
                health: ChannelHealth::default(),
                now: Duration::ZERO,
            },
            b: SimEndpoint {
                tx: ba,
                rx: ab,
                caps: caps(mtu_ba),
                health: ChannelHealth::default(),
                now: Duration::ZERO,
            },
        }
    }

    /// Moves both endpoints' clocks at once.
    pub fn advance(&mut self, now: Duration) {
        self.a.advance(now);
        self.b.advance(now);
    }
}
