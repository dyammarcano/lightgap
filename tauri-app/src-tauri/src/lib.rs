//! The desktop and mobile application.
//!
//! A thin adapter over the `modem` crate. The interface owns the camera and the
//! display; this owns one engine and hands frames between them. Everything with
//! a protocol decision in it lives below, where it is tested without hardware.

mod commands;
mod engine;

use std::sync::Mutex;

use commands::AppState;
use engine::Engine;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState(Mutex::new(Engine::new())))
        .invoke_handler(tauri::generate_handler![
            commands::on_frame,
            commands::current_qr,
            commands::status,
            commands::send_file,
            commands::save_received,
            commands::received_name,
            commands::reset,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
