//! PHASE 0 — THROWAWAY SPIKE. Deleted in full once the measurement is done.
//!
//! Measures the real cost of the hybrid camera path chosen in the design:
//! `getUserMedia` in the WebView, QR decoding in the backend.
//!
//! Question to answer: does it sustain 10 or more decodes per second at under
//! 30% of one core? If not, the design defines three fallbacks (crop to the
//! region of interest, decode in WASM, native `nokhwa`).
//!
//! It deliberately compares two IPC paths:
//!   - `spike_decode_raw`  -> `InvokeBody::Raw`, raw bytes
//!   - `spike_decode_json` -> JSON arguments, every byte serialized as a number
//!
//! NOTE (verified in tauri-2.11.5/scripts/process-ipc-message-fn.js): the raw
//! path only engages when the typed array is the ENTIRE invoke argument. Nested
//! inside an object it falls through to `JSON.stringify`, which turns every byte
//! into a number in text. That is exactly the pathological case being measured.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::ipc::{InvokeBody, Request};

/// Result of one decode attempt over a frame.
#[derive(Debug, Serialize)]
pub struct DecodeReport {
    /// Bytes that reached the backend, including the 8 B header.
    pub bytes_in: usize,
    /// Microseconds spent purely detecting and decoding, excluding IPC.
    pub decode_us: u64,
    /// Microseconds spent deserializing the arguments (this is what exposes the
    /// JSON path).
    pub deserialize_us: u64,
    /// How many QR grids were detected in the frame.
    pub grids: usize,
    /// Contents of the first successfully decoded grid.
    pub content: Option<String>,
}

/// Minimal binary header at the front of the greyscale buffer: width and height
/// as little-endian u32.
///
/// It lives inside the payload rather than in HTTP headers on purpose: it is
/// what the real protocol will do anyway, and it avoids header plumbing in the
/// spike.
const HEADER_LEN: usize = 8;

fn decode_greyscale(buf: &[u8]) -> Result<(usize, Option<String>), String> {
    if buf.len() < HEADER_LEN {
        return Err(format!(
            "buffer of {} B, smaller than the header",
            buf.len()
        ));
    }
    let width = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
    let height = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
    let pixels = &buf[HEADER_LEN..];

    let expected = width.checked_mul(height).ok_or("dimensions overflow")?;
    if pixels.len() != expected {
        return Err(format!(
            "expected {expected} px for {width}x{height}, got {}",
            pixels.len()
        ));
    }

    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| pixels[y * width + x]);
    let grids = prepared.detect_grids();
    let found = grids.len();

    let content = grids.iter().find_map(|g| {
        let mut out = Vec::new();
        g.decode_to(&mut out)
            .ok()
            .map(|_| String::from_utf8_lossy(&out).into_owned())
    });

    Ok((found, content))
}

/// Fast path: the frame arrives as a raw binary body, never touching JSON.
#[tauri::command]
pub fn spike_decode_raw(request: Request<'_>) -> Result<DecodeReport, String> {
    let t_deser = Instant::now();
    let InvokeBody::Raw(buf) = request.body() else {
        return Err("expected a raw binary body; was the typed array nested?".into());
    };
    let deserialize_us = t_deser.elapsed().as_micros() as u64;

    let t_decode = Instant::now();
    let (grids, content) = decode_greyscale(buf)?;
    let decode_us = t_decode.elapsed().as_micros() as u64;

    Ok(DecodeReport {
        bytes_in: buf.len(),
        decode_us,
        deserialize_us,
        grids,
        content,
    })
}

#[derive(Debug, Deserialize)]
pub struct JsonFrame {
    /// Same layout as the raw path, but every byte travels as a JSON number.
    pub frame: Vec<u8>,
}

/// Control path: identical except the bytes cross IPC as JSON text. It exists
/// solely to quantify the difference and justify the raw path.
#[tauri::command]
pub fn spike_decode_json(payload: JsonFrame) -> Result<DecodeReport, String> {
    let t_decode = Instant::now();
    let (grids, content) = decode_greyscale(&payload.frame)?;
    let decode_us = t_decode.elapsed().as_micros() as u64;

    Ok(DecodeReport {
        bytes_in: payload.frame.len(),
        decode_us,
        // Deserialization already happened before entering here: this path's
        // cost is measured from the frontend, as a wall-clock difference.
        deserialize_us: 0,
        grids,
        content,
    })
}

/// Generates the QR code the spike displays for the webcam to look at. Returns
/// SVG so Leptos can inject it directly.
#[tauri::command]
pub fn spike_make_qr(payload_bytes: usize, ecc: char, counter: u64) -> Result<String, String> {
    use qrcode::{render::svg, EcLevel, QrCode};

    let level = match ecc {
        'L' => EcLevel::L,
        'M' => EcLevel::M,
        'Q' => EcLevel::Q,
        'H' => EcLevel::H,
        other => return Err(format!("unknown correction level: {other}")),
    };

    // The counter goes first so freshness can be measured: if the camera reads
    // an old counter, the loop delay is visible without instrumenting anything
    // else.
    let prefix = format!("{counter:016x}");
    let mut data = prefix.into_bytes();
    data.resize(payload_bytes.max(data.len()), b'#');

    let code = QrCode::with_error_correction_level(&data, level).map_err(|e| e.to_string())?;
    Ok(code
        .render::<svg::Color>()
        .min_dimensions(720, 720)
        .quiet_zone(true)
        .build())
}
