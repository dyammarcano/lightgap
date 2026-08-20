//! The interface, compiled to wasm and mounted by Trunk.
//!
//! Everything of substance lives in [`app`]: camera capture, the QR decode,
//! the code on screen, and the calibration loops that settle exposure,
//! brightness and frame size. This file only mounts it.

mod app;

use app::*;
use leptos::prelude::*;

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(|| {
        view! {
            <App/>
        }
    })
}
