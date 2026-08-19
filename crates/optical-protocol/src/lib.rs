//! Sans-io core of the air-gapped multimodal modem.
//!
//! This crate opens no cameras, no sockets and no audio devices. It is a pure
//! state machine: you hand it incoming PDUs, ask what to transmit, and tell it
//! that time has passed. That is the `quinn` and `rustls` pattern, and it is
//! what lets a full transfer at 40% loss be tested without turning on a single
//! camera.
//!
//! The design consequence that matters: the protocol does not know which medium
//! it travels over. Adding the acoustic channel, LEDs, or a TCP socket means
//! implementing a trait, not editing this layer.

pub mod channel;
pub mod crypto;
pub mod mux;
pub mod reliability;
pub mod session;
pub mod wire;

pub use channel::{Channel, ChannelCaps, ChannelError, ChannelHealth, ChannelId, Direction};
pub use crypto::{CryptoError, Identity, KeyDirection, SessionKeys};
pub use mux::{ChannelSlot, Dedup, Priority, Scheduler};
pub use reliability::{Feedback, Mode, Progress, Receiver, RecvError, Sender, Symbol};
pub use session::{Event, PeerId, Role, Session, State};
pub use wire::{Flags, Pdu, PduKind, WireError, MAX_PAYLOAD, OVERHEAD, PROTOCOL_VERSION};
