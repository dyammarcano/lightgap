//! Negociación y ajuste de perfiles de enlace, agnósticos del medio.
//!
//! Ni la escalera de sondas ni el control adaptativo saben qué es un QR. Eso es
//! deliberado: el canal acústico negociará su velocidad de símbolo con
//! exactamente la misma escalera, y un tercer medio también.

pub mod adaptive;
pub mod ladder;
pub mod lifecycle;
pub mod scoring;

pub use adaptive::{Adaptation, Aimd};
pub use ladder::{Ladder, Phase};
pub use lifecycle::{Lifecycle, LinkState, Transition};
pub use scoring::{best, Measurement};
