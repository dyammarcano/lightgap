//! Núcleo sans-io del módem multimodal air-gapped.
//!
//! Este crate no abre cámaras, ni sockets, ni audio. Es una máquina de estados
//! pura: se le entregan PDUs entrantes, se le pregunta qué transmitir, y se le
//! avisa del paso del tiempo. Ese es el patrón de `quinn` y `rustls`, y es lo
//! que permite probar una transferencia completa con 40 % de pérdida sin
//! encender una sola cámara.
//!
//! La consecuencia de diseño importante: el protocolo no sabe por qué medio
//! viaja. Añadir el canal acústico, LEDs o un socket TCP es implementar un
//! trait, no editar esta capa.

pub mod channel;
pub mod reliability;
pub mod wire;

pub use channel::{Channel, ChannelCaps, ChannelError, ChannelHealth, ChannelId, Direction};
pub use reliability::{Feedback, Mode, Progress, Receiver, RecvError, Sender, Symbol};
pub use wire::{Flags, Pdu, PduKind, WireError, MAX_PAYLOAD, OVERHEAD, PROTOCOL_VERSION};
