//! FASE 0 — SPIKE DESECHABLE. Se borra entero al terminar la medición.
//!
//! Mide el coste real de la ruta híbrida de cámara elegida en el diseño:
//! `getUserMedia` en el WebView → frame en gris por IPC → decode en el backend.
//!
//! Pregunta a responder: ¿sostiene ≥10 decodificaciones/s con <30% de un core?
//! Si no, hay tres repliegues definidos en el diseño (recorte a ROI, decode en
//! WASM, `nokhwa` nativo).
//!
//! Compara dos rutas de IPC deliberadamente:
//!   - `spike_decode_raw`  → `InvokeBody::Raw`, bytes crudos
//!   - `spike_decode_json` → argumentos JSON, cada byte serializado como número
//!
//! OJO (verificado en tauri-2.11.5/scripts/process-ipc-message-fn.js): la ruta
//! cruda SOLO se activa si el typed array es el argumento COMPLETO del invoke.
//! Metido como campo de un objeto cae al `JSON.stringify`, que convierte cada
//! byte en un número en texto. Ese es justo el caso patológico que medimos.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::ipc::{InvokeBody, Request};

/// Resultado de un intento de decodificación sobre un frame.
#[derive(Debug, Serialize)]
pub struct DecodeReport {
    /// Bytes que llegaron al backend (incluye la cabecera de 8 B).
    pub bytes_in: usize,
    /// Microsegundos gastados solo en detectar y decodificar, sin IPC.
    pub decode_us: u64,
    /// Microsegundos gastados en deserializar los argumentos (delata la ruta JSON).
    pub deserialize_us: u64,
    /// Cuántas rejillas QR se detectaron en el frame.
    pub grids: usize,
    /// Contenido de la primera rejilla decodificada con éxito.
    pub content: Option<String>,
}

/// Cabecera binaria mínima al frente del buffer en gris: ancho y alto, u32 LE.
///
/// Va dentro del payload en vez de en cabeceras HTTP a propósito: es lo que el
/// protocolo real hará de todas formas, y evita plomería de headers en el spike.
const HEADER_LEN: usize = 8;

fn decode_greyscale(buf: &[u8]) -> Result<(usize, Option<String>), String> {
    if buf.len() < HEADER_LEN {
        return Err(format!("buffer de {} B, menor que la cabecera", buf.len()));
    }
    let width = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
    let height = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
    let pixels = &buf[HEADER_LEN..];

    let expected = width.checked_mul(height).ok_or("dimensiones desbordan")?;
    if pixels.len() != expected {
        return Err(format!(
            "esperaba {expected} px para {width}x{height}, llegaron {}",
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

/// Ruta rápida: el frame llega como cuerpo binario, sin pasar por JSON.
#[tauri::command]
pub fn spike_decode_raw(request: Request<'_>) -> Result<DecodeReport, String> {
    let t_deser = Instant::now();
    let InvokeBody::Raw(buf) = request.body() else {
        return Err("se esperaba un cuerpo binario; ¿el typed array iba anidado?".into());
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
    /// Mismo layout que la ruta cruda, pero cada byte viaja como número JSON.
    pub frame: Vec<u8>,
}

/// Ruta de control: idéntica salvo que los bytes cruzan el IPC como texto JSON.
/// Existe solo para cuantificar la diferencia y justificar la ruta cruda.
#[tauri::command]
pub fn spike_decode_json(payload: JsonFrame) -> Result<DecodeReport, String> {
    let t_decode = Instant::now();
    let (grids, content) = decode_greyscale(&payload.frame)?;
    let decode_us = t_decode.elapsed().as_micros() as u64;

    Ok(DecodeReport {
        bytes_in: payload.frame.len(),
        decode_us,
        // La deserialización ya ocurrió antes de entrar aquí: el coste de esta
        // ruta se mide desde el frontend, como diferencia de reloj de pared.
        deserialize_us: 0,
        grids,
        content,
    })
}

/// Genera el QR que el spike muestra en pantalla para que la webcam lo mire.
/// Devuelve SVG para que Leptos lo inyecte tal cual.
#[tauri::command]
pub fn spike_make_qr(payload_bytes: usize, ecc: char, counter: u64) -> Result<String, String> {
    use qrcode::{render::svg, EcLevel, QrCode};

    let level = match ecc {
        'L' => EcLevel::L,
        'M' => EcLevel::M,
        'Q' => EcLevel::Q,
        'H' => EcLevel::H,
        other => return Err(format!("nivel de corrección desconocido: {other}")),
    };

    // El contador va delante para poder medir frescura: si la cámara lee un
    // contador viejo, el retardo del lazo es visible sin instrumentar nada más.
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
