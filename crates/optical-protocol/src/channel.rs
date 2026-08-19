//! Abstraction over the physical medium.
//!
//! A channel carries **byte frames, not PDUs**. A QR code hands back exactly
//! the bytes that were encoded; interpreting them is [`crate::wire`]'s job. If
//! the channel knew about PDUs the abstraction would leak precisely where the
//! acoustic channel later plugs in — it frames differently — and where a TCP
//! socket would, since it has no frames at all.
//!
//! Channels also do not decide *what* gets sent. That is the multiplexer's job,
//! looking at live [`ChannelHealth`].

use core::fmt;
use core::time::Duration;

/// Which physical medium this is. Used to route by priority class and to let
/// telemetry say where each number came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelId {
    /// Display to camera.
    Visual,
    /// Speaker to microphone.
    Acoustic,
    /// Two instances on the same machine, to exercise the protocol without
    /// optical hardware.
    Loopback,
    /// In-memory simulated link, tests only.
    Simulated,
}

/// Which directions the channel serves.
///
/// The asymmetry is not hypothetical: calibration may conclude that audio works
/// from A to B but not the other way, because the two machines' microphones and
/// speakers have no reason to resemble each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    TxOnly,
    RxOnly,
    Bidirectional,
}

impl Direction {
    #[must_use]
    pub const fn can_tx(self) -> bool {
        matches!(self, Self::TxOnly | Self::Bidirectional)
    }

    #[must_use]
    pub const fn can_rx(self) -> bool {
        matches!(self, Self::RxOnly | Self::Bidirectional)
    }
}

/// What the channel promises. Fixed when the profile is negotiated, and only
/// changed by a recalibration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelCaps {
    pub id: ChannelId,
    /// Usable bytes per frame. This is the ceiling on PDU size for this channel.
    pub mtu: usize,
    pub direction: Direction,
    /// Nominal throughput of the negotiated profile, so the multiplexer can
    /// apportion without measuring again.
    pub nominal_bps: u64,
    /// Typical end-to-end delay of one frame.
    pub nominal_latency: Duration,
}

/// How the channel is doing *right now*. This is what the multiplexer watches
/// in order to degrade.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ChannelHealth {
    pub frames_sent: u64,
    pub frames_received: u64,
    /// Frames that arrived but failed validation.
    ///
    /// A subset of `frames_received`: every collected frame counts as received,
    /// and additionally as rejected if it turned out invalid. Keeping these
    /// separate from lost frames is deliberate — losing frames points at framing
    /// or noise, receiving corrupt ones points at an over-aggressive profile.
    /// They call for different fixes.
    pub frames_rejected: u64,
    /// When the last valid frame arrived, in session time.
    ///
    /// Session time rather than `Instant` because the core is sans-io: the
    /// caller owns the clock, and in tests that clock is virtual so a 5 MB
    /// transfer does not take 5 MB worth of seconds.
    pub last_rx: Option<Duration>,
}

impl ChannelHealth {
    /// Fraction of received frames that had to be discarded, in 0..=1.
    ///
    /// The divisor is `frames_received` alone, because rejected frames are
    /// already counted there. Adding them separately would double-count them and
    /// a channel producing nothing but garbage would report 0.5 — enough for the
    /// multiplexer to keep using it.
    ///
    /// Returns 0 with nothing received: a channel nothing is known about yet is
    /// not a bad channel, and treating it as one would have the multiplexer
    /// discard it before giving it a chance.
    #[must_use]
    pub fn rejection_rate(&self) -> f32 {
        if self.frames_received == 0 {
            return 0.0;
        }
        self.frames_rejected as f32 / self.frames_received as f32
    }
}

/// Why a frame could not be handed to the medium.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelError {
    #[error("frame of {got} B exceeds the channel MTU of {mtu} B")]
    OverMtu { got: usize, mtu: usize },

    #[error("the channel does not transmit in this direction")]
    NotTransmitting,

    #[error("the channel is down")]
    Down,

    #[error("the outbound queue is full")]
    Backpressure,
}

/// A medium that frames travel over.
///
/// Deliberately not async: the core is sans-io and must not pick a runtime. Real
/// drivers (camera, audio) run in their own tasks and feed an implementation of
/// this trait through a queue.
pub trait Channel {
    fn caps(&self) -> ChannelCaps;

    fn health(&self) -> ChannelHealth;

    /// Queues an already-serialized frame.
    ///
    /// `Ok` means accepted for transmission, not delivered. An optical channel
    /// has no acknowledgement at this level; that is the reliability layer's
    /// problem.
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), ChannelError>;

    /// Collects the next received frame, if any. Never blocks.
    fn recv_frame(&mut self) -> Option<Vec<u8>>;

    /// Reports the passage of time. Channels that model delay need it in order
    /// to decide which frames should have arrived by now.
    fn advance(&mut self, _now: Duration) {}
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Visual => "visual",
            Self::Acoustic => "acoustic",
            Self::Loopback => "loopback",
            Self::Simulated => "simulated",
        };
        f.write_str(s)
    }
}
