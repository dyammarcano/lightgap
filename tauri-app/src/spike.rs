//! FASE 0 — SPIKE DESECHABLE. Se borra entero al terminar la medición.
//!
//! El panel izquierdo genera el QR y el derecho muestra la cámara.
//!
//! Para que la cámara vea el QR hace falta una de estas (la webcam integrada de
//! un portátil apunta al usuario y NO puede ver su propia pantalla):
//!   - un segundo monitor con esta ventana, y el portátil mirándolo;
//!   - una webcam USB apuntada a la pantalla;
//!   - la segunda máquina, si ya la tienes delante.
//!
//! Sin ningún QR delante los tramos de captura e IPC siguen siendo válidos —
//! son los que deciden el go/no-go— pero el tramo de decode queda algo
//! optimista: `rqrr` corre la detección completa igual, pero se salta la
//! decodificación al no encontrar rejilla. Interpretar con esa reserva.
//!
//! El lazo se cronometra en tres tramos separados a propósito, porque no basta
//! con saber SI falla el objetivo de ≥10 decodificaciones/s: hay que saber cuál
//! de los tres repliegues del diseño aplica, y cada tramo señala uno distinto.
//!
//! | Tramo dominante | Qué significa | Repliegue |
//! |---|---|---|
//! | Captura  | el readback GPU→CPU de `getImageData` manda | bajar resolución u `OffscreenCanvas`; mover el decode a WASM NO ayuda |
//! | IPC      | cruzar el puente cuesta más que decodificar | (b) decode en WASM: elimina el cruce entero |
//! | Decode   | `rqrr` es el cuello | (a) recorte a ROI tras el primer lock; WASM tampoco ayuda |
//!
//! Sin esta separación el spike diría "no pasa" sin decir qué hacer, que es
//! justo lo que un spike no debe hacer.

use js_sys::{Object, Reflect, Uint8Array};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke)]
    async fn invoke(cmd: &str, args: JsValue) -> JsValue;
}

#[derive(Debug, Deserialize)]
struct DecodeReport {
    bytes_in: usize,
    decode_us: f64,
    grids: usize,
    content: Option<String>,
}

/// Qué ruta de IPC se está midiendo.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Path {
    /// Bytes crudos: el typed array es el argumento completo del invoke.
    Raw,
    /// Control: los bytes cruzan como números en un array JSON.
    Json,
}

/// Media móvil sobre las últimas muestras, para que los números no bailen.
#[derive(Clone, Default)]
struct Rolling {
    samples: Vec<f64>,
}

impl Rolling {
    const CAP: usize = 30;

    fn push(&mut self, v: f64) {
        self.samples.push(v);
        if self.samples.len() > Self::CAP {
            self.samples.remove(0);
        }
    }

    fn mean(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        self.samples.iter().sum::<f64>() / self.samples.len() as f64
    }
}

fn now_ms() -> f64 {
    window().performance().map(|p| p.now()).unwrap_or(0.0)
}

/// Convierte el RGBA que devuelve `getImageData` a luma de 8 bits, con la
/// cabecera de 8 B (ancho, alto en u32 LE) que espera el backend.
fn rgba_to_grey_with_header(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let px = (width as usize) * (height as usize);
    let mut out = Vec::with_capacity(8 + px);
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    for i in 0..px {
        let r = rgba[i * 4] as u32;
        let g = rgba[i * 4 + 1] as u32;
        let b = rgba[i * 4 + 2] as u32;
        out.push(((77 * r + 150 * g + 29 * b) >> 8) as u8);
    }
    out
}

#[component]
pub fn Spike() -> impl IntoView {
    let video_ref: NodeRef<leptos::html::Video> = NodeRef::new();
    let canvas_ref: NodeRef<leptos::html::Canvas> = NodeRef::new();

    let (width, set_width) = signal(1280u32);
    let (height, set_height) = signal(720u32);
    let (path, set_path) = signal(Path::Raw);
    let (running, set_running) = signal(false);

    let (qr_svg, set_qr_svg) = signal(String::new());
    let (counter, set_counter) = signal(0u64);

    let (fps, set_fps) = signal(0.0f64);
    let (capture_ms, set_capture_ms) = signal(0.0f64);
    let (ipc_ms, set_ipc_ms) = signal(0.0f64);
    let (decode_ms, set_decode_ms) = signal(0.0f64);
    let (bytes, set_bytes) = signal(0usize);
    let (hit_rate, set_hit_rate) = signal(0.0f64);
    let (status, set_status) = signal(String::from("detenido"));

    // El QR en pantalla se regenera despacio: solo sirve de blanco para la cámara.
    Effect::new(move |_| {
        let c = counter.get();
        spawn_local(async move {
            let args = Object::new();
            let _ = Reflect::set(&args, &"payloadBytes".into(), &JsValue::from(800u32));
            let _ = Reflect::set(&args, &"ecc".into(), &JsValue::from_str("Q"));
            let _ = Reflect::set(&args, &"counter".into(), &JsValue::from(c as f64));
            let res = invoke("spike_make_qr", args.into()).await;
            if let Some(svg) = res.as_string() {
                set_qr_svg.set(svg);
            }
        });
    });

    let start = move |_| {
        if running.get_untracked() {
            return;
        }
        set_running.set(true);
        set_status.set("abriendo cámara…".into());

        spawn_local(async move {
            let w = width.get_untracked();
            let h = height.get_untracked();

            // --- abrir la webcam ---------------------------------------------
            let devices = match window().navigator().media_devices() {
                Ok(d) => d,
                Err(e) => {
                    set_status.set(format!("sin mediaDevices: {e:?}"));
                    set_running.set(false);
                    return;
                }
            };

            let video_cfg = Object::new();
            let _ = Reflect::set(&video_cfg, &"width".into(), &JsValue::from(w));
            let _ = Reflect::set(&video_cfg, &"height".into(), &JsValue::from(h));
            let constraints = Object::new();
            let _ = Reflect::set(&constraints, &"video".into(), &video_cfg.into());
            let _ = Reflect::set(&constraints, &"audio".into(), &JsValue::FALSE);
            let constraints: web_sys::MediaStreamConstraints = constraints.unchecked_into();

            let promise = match devices.get_user_media_with_constraints(&constraints) {
                Ok(p) => p,
                Err(e) => {
                    set_status.set(format!("getUserMedia rechazó: {e:?}"));
                    set_running.set(false);
                    return;
                }
            };
            let stream = match JsFuture::from(promise).await {
                Ok(s) => s,
                Err(e) => {
                    set_status.set(format!("sin permiso de cámara: {e:?}"));
                    set_running.set(false);
                    return;
                }
            };
            let stream: web_sys::MediaStream = stream.unchecked_into();

            let Some(video) = video_ref.get_untracked() else {
                set_status.set("falta el elemento <video>".into());
                set_running.set(false);
                return;
            };
            video.set_src_object(Some(&stream));
            let _ = video.play();

            set_status.set("midiendo…".into());

            // --- lazo de captura ---------------------------------------------
            let mut r_fps = Rolling::default();
            let mut r_cap = Rolling::default();
            let mut r_ipc = Rolling::default();
            let mut r_dec = Rolling::default();
            let mut hits = 0.0f64;
            let mut total = 0.0f64;
            let mut tick = 0u64;

            while running.get_untracked() {
                let t_frame = now_ms();

                let (Some(canvas), Some(video)) =
                    (canvas_ref.get_untracked(), video_ref.get_untracked())
                else {
                    break;
                };
                canvas.set_width(w);
                canvas.set_height(h);

                let ctx = canvas
                    .get_context("2d")
                    .ok()
                    .flatten()
                    .and_then(|c| c.dyn_into::<web_sys::CanvasRenderingContext2d>().ok());
                let Some(ctx) = ctx else {
                    set_status.set("sin contexto 2d".into());
                    break;
                };

                let t_capture = now_ms();
                if ctx
                    .draw_image_with_html_video_element_and_dw_and_dh(
                        &video, 0.0, 0.0, w as f64, h as f64,
                    )
                    .is_err()
                {
                    gloo_timers::future::TimeoutFuture::new(50).await;
                    continue;
                }

                let Ok(image_data) = ctx.get_image_data(0.0, 0.0, w as f64, h as f64) else {
                    gloo_timers::future::TimeoutFuture::new(50).await;
                    continue;
                };
                let rgba = image_data.data();
                let grey = rgba_to_grey_with_header(&rgba, w, h);
                r_cap.push(now_ms() - t_capture);

                // --- el invoke que estamos midiendo --------------------------
                let t_ipc = now_ms();
                let report: Option<DecodeReport> = match path.get_untracked() {
                    Path::Raw => {
                        // El typed array va SOLO, sin envolver: es lo que dispara
                        // la ruta binaria en process-ipc-message-fn.js. Anidarlo
                        // en un objeto lo mandaría por JSON.stringify, que es
                        // justo el caso patológico que la otra rama mide.
                        let buf = Uint8Array::new_with_length(grey.len() as u32);
                        buf.copy_from(&grey);
                        let res = invoke("spike_decode_raw", buf.into()).await;
                        serde_wasm_bindgen::from_value(res).ok()
                    }
                    Path::Json => {
                        let arr = js_sys::Array::new();
                        for b in &grey {
                            arr.push(&JsValue::from(*b));
                        }
                        let inner = Object::new();
                        let _ = Reflect::set(&inner, &"frame".into(), &arr.into());
                        let payload = Object::new();
                        let _ = Reflect::set(&payload, &"payload".into(), &inner.into());
                        let res = invoke("spike_decode_json", payload.into()).await;
                        serde_wasm_bindgen::from_value(res).ok()
                    }
                };
                let elapsed_ipc = now_ms() - t_ipc;

                total += 1.0;
                if let Some(rep) = report {
                    let dec = rep.decode_us / 1000.0;
                    r_dec.push(dec);
                    // El coste atribuible al IPC es el reloj de pared del invoke
                    // menos lo que el backend declara haber gastado decodificando.
                    // La captura queda fuera a proposito: se mide aparte porque es
                    // lo que decide QUE repliegue tomar si el spike no pasa.
                    r_ipc.push((elapsed_ipc - dec).max(0.0));
                    set_bytes.set(rep.bytes_in);
                    if rep.grids > 0 {
                        hits += 1.0;
                    }
                    if let Some(c) = rep.content {
                        let head: String = c.chars().take(16).collect();
                        set_status.set(format!("leído: {head}"));
                    }
                }

                let frame_ms = now_ms() - t_frame;
                if frame_ms > 0.0 {
                    r_fps.push(1000.0 / frame_ms);
                }

                tick += 1;
                if tick.is_multiple_of(5) {
                    set_fps.set(r_fps.mean());
                    set_capture_ms.set(r_cap.mean());
                    set_ipc_ms.set(r_ipc.mean());
                    set_decode_ms.set(r_dec.mean());
                    set_hit_rate.set(if total > 0.0 {
                        hits / total * 100.0
                    } else {
                        0.0
                    });
                }
                if tick.is_multiple_of(30) {
                    set_counter.update(|c| *c += 1);
                }

                // Cede al event loop para que el preview siga fluido.
                gloo_timers::future::TimeoutFuture::new(0).await;
            }

            set_status.set("detenido".into());
        });
    };

    let stop = move |_| set_running.set(false);

    view! {
        <main class="spike">
            <h1>"Fase 0 — spike de throughput IPC"</h1>
            <p class="hint">
                "Encuadra el QR de la izquierda con la cámara: segundo monitor, webcam USB, "
                "o la otra máquina. La webcam integrada no puede ver su propia pantalla."
            </p>

            <div class="panes">
                <div class="pane" inner_html=move || qr_svg.get()></div>
                <div class="pane">
                    <video node_ref=video_ref autoplay=true muted=true></video>
                </div>
            </div>

            <canvas node_ref=canvas_ref style="display:none"></canvas>

            <div class="controls">
                <button on:click=start disabled=move || running.get()>"Iniciar"</button>
                <button on:click=stop disabled=move || !running.get()>"Parar"</button>

                <select on:change=move |ev| {
                    let (w, h) = match event_target_value(&ev).as_str() {
                        "960x540" => (960, 540),
                        "640x480" => (640, 480),
                        _ => (1280, 720),
                    };
                    set_width.set(w);
                    set_height.set(h);
                }>
                    <option value="1280x720">"1280 × 720"</option>
                    <option value="960x540">"960 × 540"</option>
                    <option value="640x480">"640 × 480"</option>
                </select>

                <select on:change=move |ev| {
                    set_path
                        .set(match event_target_value(&ev).as_str() {
                            "json" => Path::Json,
                            _ => Path::Raw,
                        });
                }>
                    <option value="raw">"IPC binario crudo"</option>
                    <option value="json">"IPC por array JSON (control)"</option>
                </select>
            </div>

            <table class="metrics">
                <tr>
                    <td>"Decodificaciones/s"</td>
                    <td>{move || format!("{:.1}", fps.get())}</td>
                </tr>
                <tr>
                    <td>"Captura (drawImage + getImageData + luma)"</td>
                    <td>{move || format!("{:.1} ms", capture_ms.get())}</td>
                </tr>
                <tr>
                    <td>"IPC (ida y vuelta, sin decode)"</td>
                    <td>{move || format!("{:.1} ms", ipc_ms.get())}</td>
                </tr>
                <tr>
                    <td>"Decode (backend)"</td>
                    <td>{move || format!("{:.1} ms", decode_ms.get())}</td>
                </tr>
                <tr>
                    <td>"Bytes por frame"</td>
                    <td>{move || format!("{}", bytes.get())}</td>
                </tr>
                <tr>
                    <td>"Frames con QR"</td>
                    <td>{move || format!("{:.0} %", hit_rate.get())}</td>
                </tr>
                <tr>
                    <td>"Estado"</td>
                    <td>{move || status.get()}</td>
                </tr>
            </table>
        </main>
    }
}
