//! Link profile negotiation and adjustment, agnostic to the medium.
//!
//! Neither the probe ladder nor the adaptive controller knows what a QR code is.
//! That is deliberate: the acoustic channel will negotiate its symbol rate with
//! exactly the same ladder, and so would a third medium.

pub mod adaptive;
pub mod ladder;
pub mod lifecycle;
pub mod scoring;

pub use adaptive::{Adaptation, Aimd};
pub use ladder::{Ladder, Phase};
pub use lifecycle::{Lifecycle, LinkState, Transition};
pub use scoring::{best, Measurement};
