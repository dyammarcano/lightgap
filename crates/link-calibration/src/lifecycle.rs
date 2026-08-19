//! A channel's lifecycle, independent of the session.
//!
//! This separation is what lets the acoustic channel be added without touching
//! the session state machine. Each medium is born down, gets probed, comes up
//! with a profile, degrades, and may go down again, without the session knowing
//! anything about it.

use core::time::Duration;

/// A channel's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    /// No link over this medium.
    Down,
    /// Searching for a profile.
    Probing,
    /// Operational.
    Up,
    /// Operational but struggling; still used while it delivers anything.
    Degraded,
}

/// Time without valid frames after which the channel is given up as down.
pub const SILENCE_TO_DOWN: Duration = Duration::from_secs(4);

/// How long degradation has to persist before it is declared.
///
/// A patch of bad luck is not degradation. Declaring it on the first one would
/// produce a churn of profiles that costs more than the degradation itself.
pub const DEGRADE_DEBOUNCE: Duration = Duration::from_millis(1500);

/// What happened to the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Probing started.
    ProbingStarted,
    /// It became operational.
    CameUp,
    /// It began struggling persistently.
    Degraded,
    /// It went back to working well.
    Recovered,
    /// It went down.
    WentDown,
}

/// A channel's state machine.
#[derive(Debug, Clone)]
pub struct Lifecycle {
    state: LinkState,
    now: Duration,
    last_good: Option<Duration>,
    degraded_since: Option<Duration>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl Lifecycle {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: LinkState::Down,
            now: Duration::ZERO,
            last_good: None,
            degraded_since: None,
        }
    }

    #[must_use]
    pub fn state(&self) -> LinkState {
        self.state
    }

    /// Starts the profile search.
    pub fn start_probing(&mut self) -> Option<Transition> {
        if self.state == LinkState::Probing {
            return None;
        }
        self.state = LinkState::Probing;
        Some(Transition::ProbingStarted)
    }

    /// Declares the channel operational with the profile already chosen.
    pub fn bring_up(&mut self) -> Option<Transition> {
        if self.state == LinkState::Up {
            return None;
        }
        self.state = LinkState::Up;
        self.degraded_since = None;
        self.last_good = Some(self.now);
        Some(Transition::CameUp)
    }

    /// Takes in a quality observation.
    pub fn observe(&mut self, now: Duration, success_rate: f32) -> Option<Transition> {
        self.now = now;
        if matches!(self.state, LinkState::Down | LinkState::Probing) {
            return None;
        }

        if success_rate >= crate::adaptive::ACCEPTABLE {
            self.last_good = Some(now);
            self.degraded_since = None;
            if self.state == LinkState::Degraded {
                self.state = LinkState::Up;
                return Some(Transition::Recovered);
            }
            return None;
        }

        let since = *self.degraded_since.get_or_insert(now);
        if self.state == LinkState::Up && now.saturating_sub(since) >= DEGRADE_DEBOUNCE {
            self.state = LinkState::Degraded;
            return Some(Transition::Degraded);
        }
        None
    }

    /// Advances the clock with no new observation.
    pub fn tick(&mut self, now: Duration) -> Option<Transition> {
        self.now = now;
        if matches!(self.state, LinkState::Down | LinkState::Probing) {
            return None;
        }
        let last = self.last_good?;
        if now.saturating_sub(last) >= SILENCE_TO_DOWN {
            self.state = LinkState::Down;
            self.degraded_since = None;
            self.last_good = None;
            return Some(Transition::WentDown);
        }
        None
    }

    /// Forces the channel down, for instance when the capture device
    /// disappears.
    pub fn force_down(&mut self) -> Option<Transition> {
        if self.state == LinkState::Down {
            return None;
        }
        self.state = LinkState::Down;
        self.degraded_since = None;
        self.last_good = None;
        Some(Transition::WentDown)
    }

    /// Whether the channel can carry anything right now.
    #[must_use]
    pub fn usable(&self) -> bool {
        matches!(self.state, LinkState::Up | LinkState::Degraded)
    }
}
