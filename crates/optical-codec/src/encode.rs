//! Bytes to a module matrix.
//!
//! The encoder returns **the matrix, not pixels**. Who draws it and at what size
//! is a separate decision: the UI paints it on a canvas, the test bench
//! rasterizes it with distortion, and tomorrow an LED panel would light it up
//! module by module. Mixing "what the code is" with "how it is drawn" would tie
//! the protocol to one particular display.

use qrcode::{EcLevel, QrCode};

/// Error correction level.
///
/// The trade-off is direct: more correction means less payload per frame but
/// more tolerance for blur and reflections. Calibration picks it by measuring
/// real goodput, because the level that packs in the most data is not the one
/// that delivers the most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ecc {
    /// About 7% tolerance. Maximum capacity.
    L,
    /// About 15%. Balanced.
    M,
    /// About 25%. Robust.
    Q,
    /// About 30%. Maximum tolerance, minimum capacity.
    H,
}

impl Ecc {
    fn to_level(self) -> EcLevel {
        match self {
            Self::L => EcLevel::L,
            Self::M => EcLevel::M,
            Self::Q => EcLevel::Q,
            Self::H => EcLevel::H,
        }
    }

    /// Every level, from most capacity to most robustness.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::L, Self::M, Self::Q, Self::H]
    }
}

/// Why the frame could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error("{len} B do not fit a QR code at correction level {ecc:?}")]
    TooLarge { len: usize, ecc: Ecc },

    #[error("the encoder rejected the data: {0}")]
    Rejected(String),
}

/// A square matrix of modules, without the quiet zone.
///
/// The quiet zone is excluded because it belongs to drawing: whoever paints
/// decides how much margin to leave, and the margin is not part of the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modules {
    size: usize,
    dark: Vec<bool>,
    ecc: Ecc,
}

impl Modules {
    /// Modules per side.
    #[must_use]
    pub fn size(&self) -> usize {
        self.size
    }

    #[must_use]
    pub fn ecc(&self) -> Ecc {
        self.ecc
    }

    /// Whether the module at `(x, y)` is dark. Out of range returns `false`,
    /// which is the background colour.
    #[must_use]
    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        if x >= self.size || y >= self.size {
            return false;
        }
        self.dark[y * self.size + x]
    }

    /// The raw matrix, row-major.
    #[must_use]
    pub fn as_slice(&self) -> &[bool] {
        &self.dark
    }

    /// Rasterizes to greyscale: 0 dark, 255 light.
    ///
    /// `scale` is pixels per module and `quiet` is the margin in modules. The
    /// quiet zone is not decorative: without it the detector cannot tell where
    /// the code begins, and four modules is the standard's minimum.
    ///
    /// Returns `(width, height, pixels)`.
    #[must_use]
    pub fn render_greyscale(&self, scale: usize, quiet: usize) -> (usize, usize, Vec<u8>) {
        let side_modules = self.size + quiet * 2;
        let side_px = side_modules * scale;
        let mut px = vec![255u8; side_px * side_px];

        for my in 0..self.size {
            for mx in 0..self.size {
                if !self.is_dark(mx, my) {
                    continue;
                }
                let x0 = (mx + quiet) * scale;
                let y0 = (my + quiet) * scale;
                for y in y0..y0 + scale {
                    let row = y * side_px;
                    px[row + x0..row + x0 + scale].fill(0);
                }
            }
        }

        (side_px, side_px, px)
    }
}

/// Builds the optical frame for a payload.
pub fn encode(payload: &[u8], ecc: Ecc) -> Result<Modules, EncodeError> {
    let code = QrCode::with_error_correction_level(payload, ecc.to_level()).map_err(|e| {
        // `qrcode` distinguishes the capacity case, which is the only actionable
        // one: it means lowering the payload or raising the version.
        if matches!(e, qrcode::types::QrError::DataTooLong) {
            EncodeError::TooLarge {
                len: payload.len(),
                ecc,
            }
        } else {
            EncodeError::Rejected(e.to_string())
        }
    })?;

    let size = code.width();
    let dark = code
        .to_colors()
        .into_iter()
        .map(|c| c == qrcode::Color::Dark)
        .collect();

    Ok(Modules { size, dark, ecc })
}

/// Payload size **guaranteed** to fit for arbitrary binary data, in bytes.
///
/// Measured by probing with incompressible filler, which is the worst case and
/// describes our PDUs: they carry a CRC and encrypted or coded payloads, with no
/// structure the encoder can exploit.
///
/// Luckier content fits more: `qrcode` picks the optimal mode per run, and a
/// stretch of ASCII digits goes into numeric mode at 3.33 bits per character
/// instead of 8. So this is a **safe lower bound**, not the capacity of any
/// particular blob — for that, try encoding it.
#[must_use]
pub fn max_payload(ecc: Ecc) -> usize {
    // Binary search for the largest size that encodes. The theoretical ceiling
    // for byte mode at version 40 with correction L is 2953 B.
    let mut lo = 0usize;
    let mut hi = 3000usize;
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if encode(&vec![0u8; mid], ecc).is_ok() {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}
