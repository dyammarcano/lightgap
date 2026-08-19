// FASE 0: se monta el spike; `app` es la plantilla original, se conserva
// intacta para volver a ella al terminar la medicion.
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
