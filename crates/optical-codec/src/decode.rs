//! De un frame en escala de grises a payloads.
//!
//! Un frame puede contener más de un código —dos pantallas en el encuadre, un
//! reflejo— así que se devuelven todos los que se hayan podido leer, con su
//! geometría. Quedarse solo con el primero descartaría en silencio al par
//! bueno cuando además hay un reflejo en el campo de visión.

use optical_protocol::wire::{Pdu, WireError};

use crate::geometry::{sharpness, Point, QrGeometry};

/// Un código leído del frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub payload: Vec<u8>,
    pub geometry: QrGeometry,
}

/// Un código que se detectó pero no se pudo leer.
///
/// Se informa aparte porque significa algo distinto: haber encontrado la
/// rejilla y fallar al decodificarla dice que el encuadre está bien y lo que
/// sobra es densidad o falta enfoque. No detectar nada dice que no hay nadie
/// enfrente, o que está muy lejos.
#[derive(Debug, Clone, PartialEq)]
pub struct FailedDetection {
    pub geometry: QrGeometry,
}

/// Lo que se sacó de un frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FrameScan {
    pub detections: Vec<Detection>,
    pub failed: Vec<FailedDetection>,
    /// Nitidez de la zona del primer código, o del frame si no hubo ninguno.
    pub sharpness: f32,
}

impl FrameScan {
    /// Cuántas rejillas se vieron, se leyeran o no.
    #[must_use]
    pub fn grids_seen(&self) -> usize {
        self.detections.len() + self.failed.len()
    }

    /// La geometría más relevante: la del primer código legible, y si no hay
    /// ninguno la del primero que se vio.
    #[must_use]
    pub fn best_geometry(&self) -> Option<QrGeometry> {
        self.detections
            .first()
            .map(|d| d.geometry)
            .or_else(|| self.failed.first().map(|f| f.geometry))
    }
}

/// Busca y lee todos los códigos de un frame en escala de grises.
///
/// `pixels` va por filas, un byte por píxel.
#[must_use]
pub fn scan_greyscale(width: usize, height: usize, pixels: &[u8]) -> FrameScan {
    if width == 0 || height == 0 || pixels.len() < width * height {
        return FrameScan::default();
    }

    let mut img =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| pixels[y * width + x]);

    let mut scan = FrameScan::default();
    for grid in img.detect_grids() {
        let corners = [
            Point {
                x: grid.bounds[0].x as f32,
                y: grid.bounds[0].y as f32,
            },
            Point {
                x: grid.bounds[1].x as f32,
                y: grid.bounds[1].y as f32,
            },
            Point {
                x: grid.bounds[2].x as f32,
                y: grid.bounds[2].y as f32,
            },
            Point {
                x: grid.bounds[3].x as f32,
                y: grid.bounds[3].y as f32,
            },
        ];

        let mut out = Vec::new();
        match grid.decode_to(&mut out) {
            Ok(meta) => {
                let modules = u32::from(meta.version.0 as u16) * 4 + 17;
                let geometry =
                    QrGeometry::from_corners(corners, modules, width as u32, height as u32);
                scan.detections.push(Detection {
                    payload: out,
                    geometry,
                });
            }
            Err(_) => {
                // Sin metadatos no se conoce la versión; se estima el número de
                // módulos como desconocido y la geometría sirve igual para
                // orientar el encuadre, que es para lo que hace falta.
                let geometry = QrGeometry::from_corners(corners, 0, width as u32, height as u32);
                scan.failed.push(FailedDetection { geometry });
            }
        }
    }

    // La nitidez se mide sobre la zona del código, no sobre el frame entero: un
    // fondo con textura puede dar una varianza alta y ocultar que justo el
    // código está borroso.
    let region = scan.best_geometry().map(|g| {
        let xs = g.corners.map(|p| p.x);
        let ys = g.corners.map(|p| p.y);
        let x0 = xs.iter().cloned().fold(f32::INFINITY, f32::min).max(0.0) as usize;
        let y0 = ys.iter().cloned().fold(f32::INFINITY, f32::min).max(0.0) as usize;
        let x1 = xs
            .iter()
            .cloned()
            .fold(f32::NEG_INFINITY, f32::max)
            .max(0.0) as usize;
        let y1 = ys
            .iter()
            .cloned()
            .fold(f32::NEG_INFINITY, f32::max)
            .max(0.0) as usize;
        (x0, y0, x1.min(width), y1.min(height))
    });
    scan.sharpness = sharpness(width, height, pixels, region);

    scan
}

/// Lee un frame y devuelve directamente las PDUs válidas que contenía.
///
/// Los payloads que no son PDUs válidas se descartan sin ruido: en el campo de
/// visión puede haber cualquier código de barras del mundo real, y no es un
/// error que un cartel de la pared no hable nuestro protocolo.
#[must_use]
pub fn scan_pdus(width: usize, height: usize, pixels: &[u8]) -> (Vec<Pdu>, FrameScan) {
    let scan = scan_greyscale(width, height, pixels);
    let pdus = scan
        .detections
        .iter()
        .filter_map(|d| Pdu::decode(&d.payload).ok())
        .collect();
    (pdus, scan)
}

/// Igual que [`scan_pdus`] pero informando de por qué se descartó cada payload.
///
/// Útil para diagnosticar: distinguir «no era una PDU» de «era una PDU con el
/// CRC mal» separa un cartel de la pared de un enlace que va justo de densidad.
#[must_use]
pub fn scan_pdus_verbose(
    width: usize,
    height: usize,
    pixels: &[u8],
) -> (Vec<Result<Pdu, WireError>>, FrameScan) {
    let scan = scan_greyscale(width, height, pixels);
    let results = scan
        .detections
        .iter()
        .map(|d| Pdu::decode(&d.payload))
        .collect();
    (results, scan)
}
