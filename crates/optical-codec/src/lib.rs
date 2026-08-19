//! The visual physical layer: bytes to on-screen codes to camera pixels.
//!
//! This crate is the only one that knows what a QR code is. The protocol
//! ([`optical_protocol`]) does not, and must not: that separation is what will
//! let the acoustic channel — or Data Matrix, or a bespoke binary matrix — be
//! added without touching the transport layer.

pub mod decode;
pub mod distort;
pub mod encode;
pub mod geometry;

pub use decode::{scan_greyscale, scan_pdus, Detection, FrameScan};
pub use distort::{capture, Conditions};
pub use encode::{encode, max_payload, Ecc, EncodeError, Modules};
pub use geometry::{advise, sharpness, Advice, Point, QrGeometry};
