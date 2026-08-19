//! The interface's entry points into the engine.
//!
//! Deliberately thin. Everything with a decision in it lives in `engine` or, for
//! anything the protocol decides, in the `modem` crate — where it can be tested
//! without a window on screen.

use std::sync::Mutex;

use tauri::ipc::{InvokeBody, Request};
use tauri::State;

use crate::engine::{Engine, FrameOutcome, Status};

pub struct AppState(pub Mutex<Engine>);

/// Header at the front of a camera frame: width and height as little-endian
/// u32.
///
/// Carried inside the payload rather than as separate arguments because the
/// frame has to arrive as a raw binary body, and that only happens when the
/// typed array is the *entire* invoke argument. Adding a second argument would
/// wrap it in an object and send every pixel through JSON as a number — about
/// four megabytes of text per frame instead of nine hundred kilobytes of bytes.
const HEADER_LEN: usize = 8;

/// Hands one greyscale camera frame to the engine.
#[tauri::command]
pub fn on_frame(state: State<'_, AppState>, request: Request<'_>) -> Result<FrameOutcome, String> {
    let InvokeBody::Raw(buf) = request.body() else {
        return Err("expected a raw binary body; was the typed array nested in an object?".into());
    };
    if buf.len() < HEADER_LEN {
        return Err(format!(
            "frame of {} B is shorter than its header",
            buf.len()
        ));
    }

    let width = u32::from_le_bytes(buf[0..4].try_into().expect("4 B")) as usize;
    let height = u32::from_le_bytes(buf[4..8].try_into().expect("4 B")) as usize;
    let pixels = &buf[HEADER_LEN..];

    let expected = width.checked_mul(height).ok_or("dimensions overflow")?;
    if pixels.len() != expected {
        return Err(format!(
            "expected {expected} px for {width}x{height}, got {}",
            pixels.len()
        ));
    }

    let mut engine = state.0.lock().map_err(|_| "engine lock poisoned")?;
    Ok(engine.on_camera_frame(width, height, pixels))
}

/// The code to show right now, as an SVG document.
///
/// Safe to call at the display's refresh rate: the engine only advances to the
/// next code once the current one has been up long enough for the peer's camera
/// to have had a real chance at it.
#[tauri::command]
pub fn current_qr(state: State<'_, AppState>) -> Result<String, String> {
    let mut engine = state.0.lock().map_err(|_| "engine lock poisoned")?;
    Ok(engine.current_qr().to_owned())
}

#[tauri::command]
pub fn status(state: State<'_, AppState>) -> Result<Status, String> {
    let engine = state.0.lock().map_err(|_| "engine lock poisoned")?;
    Ok(engine.status())
}

/// Reads a file from disk and offers it for transfer.
///
/// The path comes from the system file dialog on the interface side, so this
/// never invents one. Reading here rather than in the WebView keeps the file
/// bytes out of the interface entirely — they would otherwise cross the boundary
/// twice for no reason.
#[tauri::command]
pub fn send_file(state: State<'_, AppState>, path: String) -> Result<String, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());

    let len = bytes.len();
    let mut engine = state.0.lock().map_err(|_| "engine lock poisoned")?;
    engine.send_file(&name, bytes);
    Ok(format!("{name} ({len} B) queued"))
}

/// Writes the received file to disk.
///
/// The bytes are taken rather than copied: a received file can be large, and
/// leaving a second copy in memory for the rest of the session serves nothing.
#[tauri::command]
pub fn save_received(state: State<'_, AppState>, path: String) -> Result<String, String> {
    let mut engine = state.0.lock().map_err(|_| "engine lock poisoned")?;
    let Some((name, bytes)) = engine.take_received() else {
        return Err("nothing has been received yet".into());
    };
    let len = bytes.len();

    if let Err(e) = std::fs::write(&path, &bytes) {
        // Put it back. Losing a file that arrived because the destination was
        // not writable would be an unforced error, and the transfer took
        // minutes.
        engine.restore_received(name, bytes);
        return Err(format!("cannot write {path}: {e}"));
    }

    Ok(format!("saved {name} ({len} B)"))
}

/// Suggests a file name for the save dialog.
#[tauri::command]
pub fn received_name(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let engine = state.0.lock().map_err(|_| "engine lock poisoned")?;
    Ok(engine.received_ref().map(|(n, _)| n.clone()))
}

/// Starts over with a fresh identity.
#[tauri::command]
pub fn reset(state: State<'_, AppState>) -> Result<(), String> {
    let mut engine = state.0.lock().map_err(|_| "engine lock poisoned")?;
    *engine = Engine::new();
    Ok(())
}
