// PHASE 0: the spike is mounted; `app` is the original template, kept intact so
// we can go back to it once the measurement is done.
#[allow(dead_code)]
mod app;
mod spike;

use leptos::prelude::*;

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(|| {
        view! {
            <spike::Spike/>
        }
    })
}
