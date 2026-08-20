//! The interface's entry points into the engine.
//!
//! Deliberately thin. Everything with a decision in it lives in `engine` or, for
//! anything the protocol decides, in the `modem` crate — where it can be tested
//! without a window on screen.

use std::sync::Mutex;

use optical_codec::decode::FrameScan;
use tauri::State;

use crate::engine::{Engine, FrameOutcome, Status};

pub struct AppState(pub Mutex<Engine>);

/// Hands one scan result to the engine.
///
/// The decode happens in the interface, not here. Nine hundred kilobytes of
/// pixels per frame used to cross this boundary as a raw binary body, which
/// worked on desktop and could never work on Android: its WebView does not
/// expose request bodies, so Tauri falls back to a text channel and the frame
/// arrives as anything but bytes. Sending the result of the decode instead —
/// well under a hundred bytes — removes the difference between the two
/// platforms rather than papering over it.
#[tauri::command]
pub fn on_scan(
    state: State<'_, AppState>,
    scan: FrameScan,
    decode_ms: f32,
) -> Result<FrameOutcome, String> {
    let mut engine = state.0.lock().map_err(|_| "engine lock poisoned")?;
    Ok(engine.on_scan(&scan, decode_ms))
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

/// Flips fullscreen.
///
/// Bound to F11 in the interface. The window starts fullscreen every time, and
/// a fullscreen window with no chrome and no way out is a trap rather than a
/// feature — this and `leave_fullscreen` are that way out.
///
/// On mobile there is nothing to flip: the one window IS the screen, and
/// `tauri::Window` has no `set_fullscreen` there at all. The command still
/// exists so the handler list stays the same on every platform.
#[tauri::command]
pub fn toggle_fullscreen(window: tauri::Window) -> Result<bool, String> {
    #[cfg(desktop)]
    {
        let now = window.is_fullscreen().map_err(|e| e.to_string())?;
        window.set_fullscreen(!now).map_err(|e| e.to_string())?;
        Ok(!now)
    }
    #[cfg(not(desktop))]
    {
        let _ = window;
        Ok(true)
    }
}

/// Leaves fullscreen, and does nothing if the window is already windowed.
///
/// Bound to Escape. Separate from the toggle because an Escape that *entered*
/// fullscreen would surprise anyone who pressed it to get out of something else.
#[tauri::command]
pub fn leave_fullscreen(window: tauri::Window) -> Result<(), String> {
    #[cfg(desktop)]
    {
        if window.is_fullscreen().map_err(|e| e.to_string())? {
            window.set_fullscreen(false).map_err(|e| e.to_string())?;
        }
    }
    #[cfg(not(desktop))]
    {
        let _ = window;
    }
    Ok(())
}

/// Whether this platform hands out control of the display's brightness.
///
/// The interface asks once and hides the control where the answer is no, rather
/// than offering a slider that silently does nothing.
#[tauri::command]
#[must_use]
pub fn brightness_controllable() -> bool {
    crate::brightness::controllable()
}

/// Sets the display brightness, 0..1.
///
/// On this application that is transmit power, not a comfort setting: it is the
/// only control in the interface that changes the physics of the link.
#[tauri::command]
pub fn set_brightness(app: tauri::AppHandle, level: f32) -> Result<(), String> {
    crate::brightness::set(&app, level)
}
