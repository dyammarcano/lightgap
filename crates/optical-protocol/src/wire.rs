//! Formato de wire de la unidad de protocolo (PDU).
//!
//! A mano y con layout fijo, no `bincode`: bincode no promete estabilidad de
//! representación entre versiones, y esto es un formato que dos máquinas
//! distintas —potencialmente con binarios de fechas distintas— tienen que
//! interpretar igual.
//!
//! Little-endian en todos los campos.
//!
//! ```text
//! off  tam  campo
//!   0    1  version
//!   1    8  session_id
//!   9    1  kind
//!  10    2  flags
//!  12    4  seq
//!  16    4  ack
//!  20    2  payload_len
//!  22    N  payload
//! 22+N    4  crc32   (sobre todo lo anterior)
//! ```
//!
//! Sobre el tamaño de `session_id`: 8 B por PDU sobre un payload de ~900 B es
//! un 0,9 %. Recortarlo a u32 ahorraría un 0,4 % a cambio de mantener dos
//! identificadores distintos (el completo en la sesión, el truncado en el
//! wire). El diseño ahorra bytes cuando es gratis —el nonce de ChaCha20 se
//! deriva y no viaja— pero no cuando cuesta claridad.

use core::fmt;

/// Versión del protocolo que este binario habla.
pub const PROTOCOL_VERSION: u8 = 1;

/// Bytes de cabecera que preceden al payload.
pub const HEADER_LEN: usize = 22;
/// Bytes de CRC que siguen al payload.
pub const TRAILER_LEN: usize = 4;
/// Coste fijo de encapsular un payload.
pub const OVERHEAD: usize = HEADER_LEN + TRAILER_LEN;

/// Máximo que el campo `payload_len` puede expresar.
///
/// El límite real de cada enlace es mucho menor y lo impone la MTU del canal
/// (un QR ronda los 2 KB). El formato de wire no sabe nada de QR a propósito:
/// esa es justo la separación que permite añadir el canal acústico sin tocar
/// esta capa.
pub const MAX_PAYLOAD: usize = u16::MAX as usize;

/// Qué es cada PDU. Un byte, valores explícitos porque viajan por el cable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PduKind {
    /// Anuncio de presencia, portador del `peer_id` para elegir líder.
    Hello = 1,
    /// Capacidades y perfiles ofrecidos.
    Capabilities = 2,
    /// Datos de la transferencia.
    Data = 3,
    /// Confirmación (acumulativa o selectiva, según `flags`).
    Ack = 4,
    /// Sonda de calibración con patrón conocido.
    Probe = 5,
    /// Métricas medidas sobre una sonda.
    ProbeResult = 6,
    /// La transferencia terminó y se verificó.
    Complete = 7,
    /// Aborto explícito.
    Cancel = 8,
}

impl PduKind {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::Hello,
            2 => Self::Capabilities,
            3 => Self::Data,
            4 => Self::Ack,
            5 => Self::Probe,
            6 => Self::ProbeResult,
            7 => Self::Complete,
            8 => Self::Cancel,
            _ => return None,
        })
    }
}

/// Bits de control. Newtype en vez de `bitflags` para no añadir dependencia
/// por seis constantes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Flags(pub u16);

impl Flags {
    pub const NONE: Self = Self(0);
    /// Abre la sesión.
    pub const SYN: Self = Self(1 << 0);
    /// Cierra la sesión.
    pub const FIN: Self = Self(1 << 1);
    /// El campo `ack` lleva información válida.
    pub const ACK_VALID: Self = Self(1 << 2);
    /// El payload va cifrado con la clave de sesión.
    pub const ENCRYPTED: Self = Self(1 << 3);
    /// El payload es un símbolo de fuente (RaptorQ), no un chunk ordenado.
    pub const FOUNTAIN: Self = Self(1 << 4);
    /// Réplica de un PDU crítico enviado también por el otro canal.
    pub const DUPLICATED: Self = Self(1 << 5);

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl core::ops::BitOr for Flags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// Una unidad de protocolo, ya decodificada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pdu {
    pub session_id: u64,
    pub kind: PduKind,
    pub flags: Flags,
    pub seq: u32,
    pub ack: u32,
    pub payload: Vec<u8>,
}

/// Por qué un buffer no es una PDU válida.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("buffer de {got} B, se necesitan al menos {need}")]
    TooShort { got: usize, need: usize },

    #[error("versión de protocolo {got}, este binario habla la {expected}")]
    Version { got: u8, expected: u8 },

    #[error("tipo de PDU desconocido: {0}")]
    UnknownKind(u8),

    #[error("declara {declared} B de payload pero el buffer solo trae {available}")]
    PayloadLen { declared: usize, available: usize },

    #[error("sobran {0} B tras el CRC")]
    TrailingBytes(usize),

    #[error("CRC no coincide: calculado {computed:08x}, recibido {received:08x}")]
    Crc { computed: u32, received: u32 },

    #[error("payload de {got} B, el máximo del formato es {max}")]
    PayloadTooLarge { got: usize, max: usize },
}

impl Pdu {
    /// Cuántos bytes ocupa esta PDU codificada.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        OVERHEAD + self.payload.len()
    }

    /// Serializa al final de `out`.
    ///
    /// Falla solo si el payload no cabe en el campo de longitud; todo lo demás
    /// es infalible por construcción.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        if self.payload.len() > MAX_PAYLOAD {
            return Err(WireError::PayloadTooLarge {
                got: self.payload.len(),
                max: MAX_PAYLOAD,
            });
        }

        let start = out.len();
        out.reserve(self.encoded_len());

        out.push(PROTOCOL_VERSION);
        out.extend_from_slice(&self.session_id.to_le_bytes());
        out.push(self.kind as u8);
        out.extend_from_slice(&self.flags.0.to_le_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.ack.to_le_bytes());
        out.extend_from_slice(&(self.payload.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.payload);

        let crc = crc32fast::hash(&out[start..]);
        out.extend_from_slice(&crc.to_le_bytes());
        Ok(())
    }

    /// Serializa a un `Vec` nuevo. Conveniencia para tests y para quien no
    /// esté reutilizando buffer.
    pub fn to_vec(&self) -> Result<Vec<u8>, WireError> {
        let mut out = Vec::with_capacity(self.encoded_len());
        self.encode(&mut out)?;
        Ok(out)
    }

    /// Interpreta `buf` como exactamente una PDU.
    ///
    /// Es estricto con los bytes sobrantes: un canal de marcos (un QR entrega
    /// exactamente los bytes que se codificaron) no tiene por qué producirlos,
    /// así que su presencia indica corrupción, no relleno.
    pub fn decode(buf: &[u8]) -> Result<Self, WireError> {
        if buf.len() < OVERHEAD {
            return Err(WireError::TooShort {
                got: buf.len(),
                need: OVERHEAD,
            });
        }

        let version = buf[0];
        if version != PROTOCOL_VERSION {
            return Err(WireError::Version {
                got: version,
                expected: PROTOCOL_VERSION,
            });
        }

        let kind = PduKind::from_u8(buf[9]).ok_or(WireError::UnknownKind(buf[9]))?;
        let payload_len = u16::from_le_bytes([buf[20], buf[21]]) as usize;

        let total = OVERHEAD + payload_len;
        if buf.len() < total {
            return Err(WireError::PayloadLen {
                declared: payload_len,
                available: buf.len() - OVERHEAD,
            });
        }
        if buf.len() > total {
            return Err(WireError::TrailingBytes(buf.len() - total));
        }

        // El CRC se comprueba antes de confiar en cualquier campo salvo los
        // mínimos para saber dónde termina la PDU.
        let crc_at = HEADER_LEN + payload_len;
        let received = u32::from_le_bytes([
            buf[crc_at],
            buf[crc_at + 1],
            buf[crc_at + 2],
            buf[crc_at + 3],
        ]);
        let computed = crc32fast::hash(&buf[..crc_at]);
        if computed != received {
            return Err(WireError::Crc { computed, received });
        }

        Ok(Self {
            session_id: u64::from_le_bytes(buf[1..9].try_into().expect("8 B")),
            kind,
            flags: Flags(u16::from_le_bytes([buf[10], buf[11]])),
            seq: u32::from_le_bytes(buf[12..16].try_into().expect("4 B")),
            ack: u32::from_le_bytes(buf[16..20].try_into().expect("4 B")),
            payload: buf[HEADER_LEN..crc_at].to_vec(),
        })
    }
}

impl fmt::Display for Pdu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} seq={} ack={} flags={:#06x} payload={}B",
            self.kind,
            self.seq,
            self.ack,
            self.flags.0,
            self.payload.len()
        )
    }
}
