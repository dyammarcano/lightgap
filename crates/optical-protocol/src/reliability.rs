//! Cómo se garantiza que el objeto llega entero.
//!
//! Hay dos estrategias y se eligen por perfil, no por religión:
//!
//! - **Fuente (RaptorQ).** El emisor genera símbolos codificados sin parar y sin
//!   esperar nada. El receptor decodifica cuando junta suficientes, da igual
//!   cuáles. Elimina el round-trip óptico, que en este medio es el coste
//!   dominante: mostrar un QR, capturarlo, decodificarlo y responder con otro QR
//!   cuesta cientos de milisegundos.
//! - **ARQ.** Ventana deslizante con retransmisión selectiva. Cada confirmación
//!   cuesta un round-trip completo, pero da control fino y no desperdicia ancho
//!   de banda cuando el canal está limpio.
//!
//! Emisor y receptor son traits separados a propósito. Son papeles asimétricos
//! —con fuente el emisor no necesita saber nada del receptor hasta el final— y
//! meterlos en un solo trait dejaría la mitad de los métodos vacíos en cada
//! implementación.

pub mod arq;
pub mod fountain;

use crate::wire::Flags;

/// Qué estrategia usa una transferencia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// RaptorQ. Sin orden, sin retransmisión explícita, sin esperas.
    Fountain,
    /// Ventana deslizante con retransmisión selectiva.
    Arq,
}

impl Mode {
    /// Bandera que debe llevar un PDU de datos de este modo.
    #[must_use]
    pub const fn flag(self) -> Flags {
        match self {
            Self::Fountain => Flags::FOUNTAIN,
            Self::Arq => Flags::NONE,
        }
    }
}

/// Una porción del objeto lista para viajar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// Identificador. Va en `Pdu::seq`.
    ///
    /// En ARQ es el índice del chunk y es denso. En fuente es el identificador
    /// del símbolo codificado y crece indefinidamente: el emisor puede generar
    /// muchos más símbolos que chunks tiene el objeto, y eso es justamente el
    /// mecanismo, no un defecto.
    pub id: u32,
    pub bytes: Vec<u8>,
}

/// Cuánto queda. Sirve para la barra de progreso y para que el multiplexor sepa
/// si merece la pena seguir invirtiendo ancho de banda en esta transferencia.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    /// Unidades ya resueltas (chunks confirmados, o símbolos útiles reunidos).
    pub have: u64,
    /// Unidades necesarias para terminar.
    pub need: u64,
}

impl Progress {
    /// Fracción completada en 0..=1. Devuelve 1 si no hace falta nada, para que
    /// un objeto vacío no se quede colgado en el 0 %.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        if self.need == 0 {
            return 1.0;
        }
        (self.have as f32 / self.need as f32).min(1.0)
    }
}

/// Lo que el receptor le cuenta al emisor.
///
/// Va en el payload de un PDU `Ack`. Los dos modos necesitan decir cosas
/// distintas, y forzarlos a un formato común haría que uno de los dos mintiera.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feedback {
    /// ARQ: todo hasta `cumulative` (exclusivo) está, y de lo posterior faltan
    /// los que se listan.
    Selective {
        cumulative: u32,
        missing: Vec<u32>,
        /// Cuántos símbolos más admite la ventana del receptor.
        window: u16,
    },
    /// Fuente: al emisor solo le importa si ya puede parar. Cuántos símbolos
    /// van reunidos sirve para estimar cuánto falta, no para decidir qué
    /// reenviar —en fuente no se reenvía nada concreto.
    Fountain { complete: bool, received: u32 },
}

/// Etiquetas de los dos dialectos de realimentación en el cable.
const FB_SELECTIVE: u8 = 1;
const FB_FOUNTAIN: u8 = 2;

impl Feedback {
    /// Serializa para viajar en el payload de un PDU `Ack`.
    ///
    /// La lista de huecos ya viene acotada por quien la produce
    /// ([`arq::MAX_MISSING_REPORTED`]); aquí no se recorta nada, porque
    /// silenciar huecos a última hora dejaría al emisor creyendo que ya los
    /// mandó todos.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Selective {
                cumulative,
                missing,
                window,
            } => {
                out.push(FB_SELECTIVE);
                out.extend_from_slice(&cumulative.to_le_bytes());
                out.extend_from_slice(&window.to_le_bytes());
                let n = u16::try_from(missing.len()).unwrap_or(u16::MAX);
                out.extend_from_slice(&n.to_le_bytes());
                for id in missing.iter().take(n as usize) {
                    out.extend_from_slice(&id.to_le_bytes());
                }
            }
            Self::Fountain { complete, received } => {
                out.push(FB_FOUNTAIN);
                out.push(u8::from(*complete));
                out.extend_from_slice(&received.to_le_bytes());
            }
        }
        out
    }

    /// Interpreta lo que venía en un PDU `Ack`.
    ///
    /// Devuelve `None` ante cualquier cosa que no cuadre. El CRC ya descartó los
    /// marcos corruptos, así que llegar aquí con basura significa que el par
    /// habla otro dialecto — y eso se trata ignorando el mensaje, no rompiendo
    /// la sesión.
    #[must_use]
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let (&tag, rest) = buf.split_first()?;
        match tag {
            FB_SELECTIVE => {
                if rest.len() < 8 {
                    return None;
                }
                let cumulative = u32::from_le_bytes(rest[0..4].try_into().ok()?);
                let window = u16::from_le_bytes(rest[4..6].try_into().ok()?);
                let n = u16::from_le_bytes(rest[6..8].try_into().ok()?) as usize;
                let ids = &rest[8..];
                if ids.len() != n * 4 {
                    return None;
                }
                let missing = ids
                    .chunks_exact(4)
                    .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                Some(Self::Selective {
                    cumulative,
                    missing,
                    window,
                })
            }
            FB_FOUNTAIN => {
                if rest.len() != 5 {
                    return None;
                }
                Some(Self::Fountain {
                    complete: rest[0] != 0,
                    received: u32::from_le_bytes(rest[1..5].try_into().ok()?),
                })
            }
            _ => None,
        }
    }
}

/// Por qué un símbolo entrante no se pudo incorporar.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecvError {
    #[error("símbolo de {got} B, se esperaban {expected}")]
    SymbolSize { got: usize, expected: usize },

    #[error("identificador {id} fuera del objeto ({chunks} chunks)")]
    OutOfRange { id: u32, chunks: u32 },
}

/// El lado que tiene el objeto y lo va soltando.
pub trait Sender {
    /// Siguiente porción a transmitir, limitada a `max_payload` bytes.
    ///
    /// Devolver `None` significa "ahora mismo no hay nada que mandar", no
    /// "terminé": con ARQ la ventana puede estar llena esperando confirmación.
    /// Para saber si terminó está [`Sender::is_complete`].
    fn next_symbol(&mut self, max_payload: usize) -> Option<Symbol>;

    /// Incorpora lo que el receptor ha contado.
    fn on_feedback(&mut self, feedback: &Feedback);

    /// El receptor ya tiene el objeto entero y se puede dejar de emitir.
    fn is_complete(&self) -> bool;

    fn progress(&self) -> Progress;
}

/// El lado que reúne las porciones y reconstruye.
pub trait Receiver {
    /// Incorpora un símbolo recibido.
    fn on_symbol(&mut self, symbol: &Symbol) -> Result<(), RecvError>;

    /// Qué contarle al emisor ahora mismo.
    fn feedback(&self) -> Feedback;

    /// Devuelve el objeto reconstruido, una sola vez.
    ///
    /// Consume el resultado a propósito: reconstruir un objeto de varios MB no
    /// es gratis, y devolverlo por referencia invitaría a copiarlo en cada
    /// consulta de progreso.
    fn take_object(&mut self) -> Option<Vec<u8>>;

    fn progress(&self) -> Progress;
}
