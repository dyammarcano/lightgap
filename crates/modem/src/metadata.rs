//! What the receiver needs to know before the first data symbol arrives.
//!
//! Sent once, in a `Capabilities` PDU, and repeated until acknowledged. The
//! receiver cannot do anything useful without it: it cannot size its buffer, it
//! cannot build a decoder, and it cannot tell whether what it reconstructed is
//! what was sent.
//!
//! Repeating it until acknowledged rather than sending it once is not
//! belt-and-braces. It is the single frame the whole transfer depends on, and on
//! a channel that loses a third of its frames, sending something once means
//! sometimes not sending it at all.

/// Bytes of fixed header before the variable-length name.
const FIXED_LEN: usize = 8 + 12 + 32 + 1 + 1;

/// Longest file name that fits.
///
/// Bounded because this has to fit in one frame alongside the rest, and because
/// an unbounded length field read from the wire is an invitation to allocate
/// whatever an attacker asks for.
pub const MAX_NAME_LEN: usize = 120;

/// Which reliability strategy the sender chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Fountain,
    Arq,
}

impl Mode {
    const fn tag(self) -> u8 {
        match self {
            Self::Fountain => 1,
            Self::Arq => 2,
        }
    }

    const fn from_tag(t: u8) -> Option<Self> {
        match t {
            1 => Some(Self::Fountain),
            2 => Some(Self::Arq),
            _ => None,
        }
    }
}

/// Everything the receiver needs before data starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferMeta {
    /// Total object length in bytes.
    pub total_len: u64,
    /// RaptorQ transmission parameters, exactly as the sender computed them.
    ///
    /// Transmitted rather than derived. Deriving them on both sides would pin
    /// the block splitting to a library default, and that default is the
    /// difference between seconds and minutes of decoding on a large object.
    pub oti: [u8; 12],
    /// BLAKE3 of the whole object.
    pub hash: [u8; 32],
    pub mode: Mode,
    pub name: String,
}

/// Why a metadata frame could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetaError {
    #[error("{got} B is not enough for a metadata header")]
    TooShort { got: usize },

    #[error("unknown reliability mode: {0}")]
    UnknownMode(u8),

    #[error("name of {got} B exceeds the {max} B limit")]
    NameTooLong { got: usize, max: usize },

    #[error("declares a {declared} B name but only {available} B follow")]
    NameTruncated { declared: usize, available: usize },

    #[error("the name is not valid UTF-8")]
    NameNotUtf8,
}

impl TransferMeta {
    /// Computes the metadata for an object about to be sent.
    #[must_use]
    pub fn for_object(name: &str, object: &[u8], oti: [u8; 12], mode: Mode) -> Self {
        Self {
            total_len: object.len() as u64,
            oti,
            hash: *blake3::hash(object).as_bytes(),
            mode,
            name: name.chars().take(MAX_NAME_LEN).collect(),
        }
    }

    /// Whether a reconstructed object is the one that was announced.
    ///
    /// Checked even though every frame already carries a CRC and, when
    /// encryption is on, an authentication tag. Those protect individual frames;
    /// this protects the reassembly. A reliability layer that mislaid a chunk
    /// would produce frames that were each perfectly valid and an object that
    /// was wrong.
    #[must_use]
    pub fn verify(&self, object: &[u8]) -> bool {
        object.len() as u64 == self.total_len && *blake3::hash(object).as_bytes() == self.hash
    }

    pub fn encode(&self) -> Result<Vec<u8>, MetaError> {
        let name = self.name.as_bytes();
        if name.len() > MAX_NAME_LEN {
            return Err(MetaError::NameTooLong {
                got: name.len(),
                max: MAX_NAME_LEN,
            });
        }

        let mut out = Vec::with_capacity(FIXED_LEN + name.len());
        out.extend_from_slice(&self.total_len.to_le_bytes());
        out.extend_from_slice(&self.oti);
        out.extend_from_slice(&self.hash);
        out.push(self.mode.tag());
        out.push(name.len() as u8);
        out.extend_from_slice(name);
        Ok(out)
    }

    pub fn decode(buf: &[u8]) -> Result<Self, MetaError> {
        if buf.len() < FIXED_LEN {
            return Err(MetaError::TooShort { got: buf.len() });
        }

        let total_len = u64::from_le_bytes(buf[0..8].try_into().expect("8 B"));
        let oti: [u8; 12] = buf[8..20].try_into().expect("12 B");
        let hash: [u8; 32] = buf[20..52].try_into().expect("32 B");
        let mode = Mode::from_tag(buf[52]).ok_or(MetaError::UnknownMode(buf[52]))?;
        let name_len = buf[53] as usize;

        if name_len > MAX_NAME_LEN {
            return Err(MetaError::NameTooLong {
                got: name_len,
                max: MAX_NAME_LEN,
            });
        }
        let available = buf.len() - FIXED_LEN;
        if available < name_len {
            return Err(MetaError::NameTruncated {
                declared: name_len,
                available,
            });
        }

        let name = core::str::from_utf8(&buf[FIXED_LEN..FIXED_LEN + name_len])
            .map_err(|_| MetaError::NameNotUtf8)?
            .to_owned();

        Ok(Self {
            total_len,
            oti,
            hash,
            mode,
            name,
        })
    }

    /// Bytes this metadata occupies once encoded.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        FIXED_LEN + self.name.len()
    }
}
