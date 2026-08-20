//! The desktop and mobile application.
//!
//! A thin adapter over the `modem` crate. The interface owns the camera and the
//! display; this owns one engine and hands frames between them. Everything with
//! a protocol decision in it lives below, where it is tested without hardware.

mod brightness;
mod commands;
mod engine;

use std::sync::Mutex;

use commands::AppState;
use engine::Engine;
#[cfg(desktop)]
use tauri_plugin_window_state::StateFlags;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(brightness::init());

    // Size, position and maximised state are remembered; fullscreen is not.
    //
    // The window opens maximised rather than fullscreen. Big matters — the
    // display IS the transmitter, and a larger code is more pixels per module
    // on the peer's camera, which is the one thing that raises payload per
    // frame — but borderless fullscreen buys those pixels by removing the title
    // bar, and with it any way to move the window or put it beside the thing it
    // is pointed at. F11 still gives real fullscreen to anyone who wants it.
    //
    // Fullscreen deliberately stays out of the saved flags: it is a mode you
    // enter on purpose, not a setting that should follow you into the next run.
    //
    // Desktop only, and not merely because it would be pointless on a tablet:
    // the crate carries `#![cfg(not(any(target_os = "android", target_os =
    // "ios")))]` at its root, so on mobile it compiles away to nothing and any
    // mention of it fails to resolve.
    #[cfg(desktop)]
    let builder = builder.plugin(
        tauri_plugin_window_state::Builder::default()
            .with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED)
            .build(),
    );

    builder
        .manage(AppState(Mutex::new(Engine::new())))
        .invoke_handler(tauri::generate_handler![
            commands::on_scan,
            commands::current_qr,
            commands::status,
            commands::send_file,
            commands::save_received,
            commands::received_name,
            commands::reset,
            commands::toggle_fullscreen,
            commands::leave_fullscreen,
            commands::brightness_controllable,
            commands::set_brightness,
        ])
        .run(tauri::generate_context!())
        .expect("error while running lightgap");
}
