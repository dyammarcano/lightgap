// FASE 0: `spike` es codigo desechable, se borra al cerrar la medicion.
mod spike;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            greet,
            spike::spike_decode_raw,
            spike::spike_decode_json,
            spike::spike_make_qr,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
