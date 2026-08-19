//! De bytes a matriz de módulos.
//!
//! El codificador devuelve **la matriz, no píxeles**. Quién la dibuja y a qué
//! tamaño es otra decisión: la interfaz la pinta en un canvas, el banco de
//! pruebas la rasteriza con distorsión, y mañana un panel de LEDs la encendería
//! módulo a módulo. Mezclar «qué es el código» con «cómo se dibuja» ataría el
//! protocolo a una pantalla concreta.

use qrcode::{EcLevel, QrCode};

/// Nivel de corrección de errores.
///
/// El compromiso es directo: más corrección, menos payload por marco pero más
/// tolerancia a desenfoque y reflejos. La calibración de la Fase 3 lo elige
/// midiendo goodput real, porque el nivel que más datos mete no es el que más
/// datos entrega.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ecc {
    /// ~7 % de tolerancia. Máxima capacidad.
    L,
    /// ~15 %. Equilibrio.
    M,
    /// ~25 %. Robusto.
    Q,
    /// ~30 %. Máxima tolerancia, mínima capacidad.
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

    /// Todos los niveles, de más capacidad a más robustez.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::L, Self::M, Self::Q, Self::H]
    }
}

/// Por qué no se pudo construir el marco.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error("{len} B no caben en un QR con corrección {ecc:?}")]
    TooLarge { len: usize, ecc: Ecc },

    #[error("el codificador rechazó los datos: {0}")]
    Rejected(String),
}

/// Una matriz cuadrada de módulos, sin zona de silencio.
///
/// La zona de silencio no se incluye porque es cosa del dibujado: quien pinta
/// decide cuántos módulos de margen deja, y el margen no forma parte del código.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modules {
    size: usize,
    dark: Vec<bool>,
    ecc: Ecc,
}

impl Modules {
    /// Módulos por lado.
    #[must_use]
    pub fn size(&self) -> usize {
        self.size
    }

    #[must_use]
    pub fn ecc(&self) -> Ecc {
        self.ecc
    }

    /// Si el módulo en `(x, y)` es oscuro. Fuera de rango devuelve `false`, que
    /// es el color del fondo.
    #[must_use]
    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        if x >= self.size || y >= self.size {
            return false;
        }
        self.dark[y * self.size + x]
    }

    /// La matriz en crudo, por filas.
    #[must_use]
    pub fn as_slice(&self) -> &[bool] {
        &self.dark
    }

    /// Rasteriza a escala de grises: 0 oscuro, 255 claro.
    ///
    /// `scale` son píxeles por módulo y `quiet` módulos de margen. La zona de
    /// silencio no es decorativa: sin ella el detector no distingue dónde
    /// empieza el código, y cuatro módulos es el mínimo del estándar.
    ///
    /// Devuelve `(ancho, alto, píxeles)`.
    #[must_use]
    pub fn render_greyscale(&self, scale: usize, quiet: usize) -> (usize, usize, Vec<u8>) {
        let lado_modulos = self.size + quiet * 2;
        let lado_px = lado_modulos * scale;
        let mut px = vec![255u8; lado_px * lado_px];

        for my in 0..self.size {
            for mx in 0..self.size {
                if !self.is_dark(mx, my) {
                    continue;
                }
                let x0 = (mx + quiet) * scale;
                let y0 = (my + quiet) * scale;
                for y in y0..y0 + scale {
                    let fila = y * lado_px;
                    px[fila + x0..fila + x0 + scale].fill(0);
                }
            }
        }

        (lado_px, lado_px, px)
    }
}

/// Construye el marco óptico para un payload.
pub fn encode(payload: &[u8], ecc: Ecc) -> Result<Modules, EncodeError> {
    let code = QrCode::with_error_correction_level(payload, ecc.to_level()).map_err(|e| {
        // `qrcode` distingue el caso de capacidad, que es el único accionable:
        // significa que hay que bajar el payload o subir la versión.
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

/// Payload máximo **garantizado** para datos binarios arbitrarios, en bytes.
///
/// Se mide probando con relleno incompresible, que es el peor caso y el que
/// describe a nuestras PDUs: llevan CRC y payloads cifrados o codificados, sin
/// estructura que el codificador pueda aprovechar.
///
/// Contenido con suerte cabe más: `qrcode` elige el modo óptimo por tramos, y
/// una ristra de dígitos ASCII entra en modo numérico a 3,33 bits por carácter
/// en vez de 8. Por eso esto es una **cota inferior segura**, no la capacidad de
/// un dato concreto — para eso, probar a codificarlo.
#[must_use]
pub fn max_payload(ecc: Ecc) -> usize {
    // Búsqueda binaria sobre el mayor tamaño que codifica. El techo teórico del
    // modo byte en versión 40 con corrección L es 2953 B.
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
