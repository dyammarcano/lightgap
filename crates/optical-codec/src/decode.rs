//! A greyscale frame to payloads.
//!
//! A frame may hold more than one code — two displays in view, a reflection — so
//! every code that could be read is returned, with its geometry. Keeping only
//! the first would silently discard the real peer whenever a reflection also
//! happens to be in the field of view.

use optical_protocol::wire::{Pdu, WireError};

use crate::geometry::{sharpness, Point, QrGeometry};

/// A code read out of the frame.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Detection {
    pub payload: Vec<u8>,
    pub geometry: QrGeometry,
}

/// A code that was detected but could not be read.
///
/// Reported separately because it means something different: finding the grid
/// and failing to decode it says the framing is fine and what is excessive is
/// density, or what is missing is focus. Detecting nothing says nobody is there,
/// or they are too far away.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct FailedDetection {
    pub geometry: QrGeometry,
}

/// What came out of one frame.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
pub struct FrameScan {
    pub detections: Vec<Detection>,
    pub failed: Vec<FailedDetection>,
    /// Sharpness over the first code's area, or over the frame if there was
    /// none.
    pub sharpness: f32,
}

impl FrameScan {
    /// How many grids were seen, readable or not.
    #[must_use]
    pub fn grids_seen(&self) -> usize {
        self.detections.len() + self.failed.len()
    }

    /// The most relevant geometry: that of the first readable code, or failing
    /// that, of the first one seen.
    #[must_use]
    pub fn best_geometry(&self) -> Option<QrGeometry> {
        self.detections
            .first()
            .map(|d| d.geometry)
            .or_else(|| self.failed.first().map(|f| f.geometry))
    }
}

/// Finds and reads every code in a greyscale frame.
///
/// `pixels` is row-major, one byte per pixel.
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
                // Without metadata the version is unknown, so the module count
                // is left at zero. The geometry still serves to guide framing,
                // which is what it is needed for here.
                let geometry = QrGeometry::from_corners(corners, 0, width as u32, height as u32);
                scan.failed.push(FailedDetection { geometry });
            }
        }
    }

    // Sharpness is measured over the code's area, not the whole frame: a
    // textured background can produce a high variance and hide the fact that the
    // code specifically is blurry.
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

/// Reads a frame and returns the valid PDUs it contained.
///
/// Payloads that are not valid PDUs are discarded quietly: the field of view may
/// contain any barcode in the real world, and a poster on the wall not speaking
/// our protocol is not an error.
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

/// Like [`scan_pdus`] but reporting why each payload was discarded.
///
/// Useful for diagnosis: telling "not a PDU" apart from "a PDU with a bad CRC"
/// separates a poster on the wall from a link running right at its density
/// limit.
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
