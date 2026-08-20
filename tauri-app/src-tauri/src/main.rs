//! Desktop entry point. The application itself is a library so that the
//! Android build, which is started by the JVM rather than by `main`, can share
//! every line of it.

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    lightgap_lib::run()
}
