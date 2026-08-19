//! Capa física visual: bytes ⇄ códigos en pantalla ⇄ píxeles de cámara.
//!
//! Este crate es el único que sabe qué es un QR. El protocolo
//! ([`optical_protocol`]) no lo sabe y no debe saberlo: esa separación es la
//! que permitirá añadir el canal acústico —o Data Matrix, o una matriz binaria
//! propia— sin tocar la capa de transporte.

pub mod decode;
pub mod distort;
pub mod encode;
pub mod geometry;

pub use decode::{scan_greyscale, scan_pdus, Detection, FrameScan};
pub use distort::{capture, Conditions};
pub use encode::{encode, max_payload, Ecc, EncodeError, Modules};
pub use geometry::{advise, sharpness, Advice, Point, QrGeometry};
