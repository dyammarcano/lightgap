//! The acoustic physical layer: bytes to tones to bytes.
//!
//! Two-tone frequency-shift keying in a near-inaudible band. No audio I/O
//! happens here — this crate turns bytes into samples and samples back into
//! bytes, and `cpal` moves them. That split is what lets the whole modem be
//! tested against synthetic noise without a speaker in the room.
//!
//! **Why 2-FSK and not something denser.** Higher-order FSK or OFDM would carry
//! more per symbol, but this channel is not bandwidth-limited in a useful sense:
//! the usable band is narrow, laptop speakers roll off badly near it, and the
//! operating system may be applying echo cancellation and noise suppression that
//! nobody asked for. Under those conditions the win from a denser scheme is
//! small and the loss of robustness is large. The acoustic channel exists to
//! carry acknowledgements, not files.
//!
//! **Why not ultrasound proper.** Above 20 kHz most laptop microphones filter
//! hard, speakers lose output, and OS noise suppression tends to treat anything
//! up there as garbage. In practice 16.5 to 19 kHz behaves far better, at the
//! cost of being audible to some younger listeners. Which band actually works is
//! a question for calibration to answer per device pair, not for this crate to
//! assume.

pub mod calibration;
pub mod framing;
pub mod fsk;
pub mod impair;

pub use calibration::{assign_bands, BandMeasurement, BandPlan, Viability};
pub use framing::{Framer, FramingError};
pub use fsk::{demodulate, modulate, AcousticProfile, Demodulated};
pub use impair::{impair, Impairment};
