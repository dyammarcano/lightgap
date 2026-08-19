//! PHASE 0 — THROWAWAY SPIKE. Deleted in full once the measurement is done.
//!
//! The left pane generates the QR code and the right pane shows the camera.
//!
//! For the camera to see the code you need one of these (a laptop's built-in
//! webcam faces the user and CANNOT see its own screen):
//!   - a second monitor showing this window, with the laptop looking at it;
//!   - a USB webcam aimed at the display;
//!   - the second machine, if it is already in front of you;
//!   - a phone running the mobile build, which is often the easiest option.
//!
//! With no QR code in view the capture and IPC segments remain valid — they are
//! what decides the go/no-go — but the decode segment reads optimistically:
//! `rqrr` still runs full detection, but skips decoding when it finds no grid.
//! Read it with that caveat.
//!
//! The loop is timed in three separate segments on purpose, because knowing
//! WHETHER the 10 decodes-per-second target fails is not enough: you need to
//! know which of the three fallbacks applies, and each segment points at a
//! different one.
//!
//! | Dominant segment | What it means | Fallback |
//! |---|---|---|
//! | Capture | the GPU-to-CPU readback of `getImageData` dominates | lower the resolution or use `OffscreenCanvas`; moving decode to WASM does NOT help |
//! | IPC     | crossing the bridge costs more than decoding | (b) decode in WASM: it removes the crossing entirely |
//! | Decode  | `rqrr` is the bottleneck | (a) crop to the region of interest after first lock; WASM does not help either |
//!
//! Without that separation the spike would say "it fails" without saying what to
//! do, which is precisely what a spike must not do.

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

/// Which IPC path is being measured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Path {
    /// Raw bytes: the typed array is the entire invoke argument.
    Raw,
    /// Control: the bytes cross as numbers in a JSON array.
    Json,
}

/// Rolling mean over the last few samples, so the numbers do not jitter.
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

/// Converts the RGBA that `getImageData` returns into 8-bit luma, with the 8 B
/// header (width, height as little-endian u32) the backend expects.
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
    let (status, set_status) = signal(String::from("stopped"));

    // The on-screen QR regenerates slowly: it is only a target for the camera.
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
        set_status.set("opening camera...".into());

        spawn_local(async move {
            let w = width.get_untracked();
            let h = height.get_untracked();

            // --- open the webcam ---------------------------------------------
            let devices = match window().navigator().media_devices() {
                Ok(d) => d,
                Err(e) => {
                    set_status.set(format!("no mediaDevices: {e:?}"));
                    set_running.set(false);
                    return;
                }
            };

            let video_cfg = Object::new();
            let _ = Reflect::set(&video_cfg, &"width".into(), &JsValue::from(w));
            let _ = Reflect::set(&video_cfg, &"height".into(), &JsValue::from(h));
            // Rear camera when there is one: on a phone it is far better than the
            // front camera, and it is the one that will be pointed at the peer.
            let _ = Reflect::set(&video_cfg, &"facingMode".into(), &"environment".into());
            let constraints = Object::new();
            let _ = Reflect::set(&constraints, &"video".into(), &video_cfg.into());
            let _ = Reflect::set(&constraints, &"audio".into(), &JsValue::FALSE);
            let constraints: web_sys::MediaStreamConstraints = constraints.unchecked_into();

            let promise = match devices.get_user_media_with_constraints(&constraints) {
                Ok(p) => p,
                Err(e) => {
                    set_status.set(format!("getUserMedia refused: {e:?}"));
                    set_running.set(false);
                    return;
                }
            };
            let stream = match JsFuture::from(promise).await {
                Ok(s) => s,
                Err(e) => {
                    set_status.set(format!("no camera permission: {e:?}"));
                    set_running.set(false);
                    return;
                }
            };
            let stream: web_sys::MediaStream = stream.unchecked_into();

            let Some(video) = video_ref.get_untracked() else {
                set_status.set("the <video> element is missing".into());
                set_running.set(false);
                return;
            };
            video.set_src_object(Some(&stream));
            let _ = video.play();

            set_status.set("measuring...".into());

            // --- capture loop -------------------------------------------------
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
                    set_status.set("no 2d context".into());
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

                // --- the invoke being measured -------------------------------
                let t_ipc = now_ms();
                let report: Option<DecodeReport> = match path.get_untracked() {
                    Path::Raw => {
                        // The typed array goes ALONE, unwrapped: that is what
                        // triggers the binary path in
                        // process-ipc-message-fn.js. Nesting it in an object
                        // would send it through JSON.stringify, which is exactly
                        // the pathological case the other branch measures.
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
                    // The cost attributable to IPC is the invoke's wall clock
                    // minus what the backend reports having spent decoding.
                    // Capture is deliberately excluded and measured separately,
                    // because it is what decides WHICH fallback to take if the
                    // spike does not pass.
                    r_ipc.push((elapsed_ipc - dec).max(0.0));
                    set_bytes.set(rep.bytes_in);
                    if rep.grids > 0 {
                        hits += 1.0;
                    }
                    if let Some(c) = rep.content {
                        let head: String = c.chars().take(16).collect();
                        set_status.set(format!("read: {head}"));
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

                // Yield to the event loop so the preview stays smooth.
                gloo_timers::future::TimeoutFuture::new(0).await;
            }

            set_status.set("stopped".into());
        });
    };

    let stop = move |_| set_running.set(false);

    view! {
        <main class="spike">
            <h1>"Phase 0 - IPC throughput spike"</h1>
            <p class="hint">
                "Frame the QR code on the left with the camera: a second monitor, a USB "
                "webcam, a phone, or the other machine. A built-in webcam cannot see its "
                "own screen."
            </p>

            <div class="panes">
                <div class="pane" inner_html=move || qr_svg.get()></div>
                <div class="pane">
                    <video node_ref=video_ref autoplay=true muted=true></video>
                </div>
            </div>

            <canvas node_ref=canvas_ref style="display:none"></canvas>

            <div class="controls">
                <button on:click=start disabled=move || running.get()>"Start"</button>
                <button on:click=stop disabled=move || !running.get()>"Stop"</button>

                <select on:change=move |ev| {
                    let (w, h) = match event_target_value(&ev).as_str() {
                        "960x540" => (960, 540),
                        "640x480" => (640, 480),
                        _ => (1280, 720),
                    };
                    set_width.set(w);
                    set_height.set(h);
                }>
                    <option value="1280x720">"1280 x 720"</option>
                    <option value="960x540">"960 x 540"</option>
                    <option value="640x480">"640 x 480"</option>
                </select>

                <select on:change=move |ev| {
                    set_path
                        .set(match event_target_value(&ev).as_str() {
                            "json" => Path::Json,
                            _ => Path::Raw,
                        });
                }>
                    <option value="raw">"Raw binary IPC"</option>
                    <option value="json">"JSON array IPC (control)"</option>
                </select>
            </div>

            <table class="metrics">
                <tr>
                    <td>"Decodes/s"</td>
                    <td>{move || format!("{:.1}", fps.get())}</td>
                </tr>
                <tr>
                    <td>"Capture (drawImage + getImageData + luma)"</td>
                    <td>{move || format!("{:.1} ms", capture_ms.get())}</td>
                </tr>
                <tr>
                    <td>"IPC (round trip, decode excluded)"</td>
                    <td>{move || format!("{:.1} ms", ipc_ms.get())}</td>
                </tr>
                <tr>
                    <td>"Decode (backend)"</td>
                    <td>{move || format!("{:.1} ms", decode_ms.get())}</td>
                </tr>
                <tr>
                    <td>"Bytes per frame"</td>
                    <td>{move || format!("{}", bytes.get())}</td>
                </tr>
                <tr>
                    <td>"Frames with a QR"</td>
                    <td>{move || format!("{:.0} %", hit_rate.get())}</td>
                </tr>
                <tr>
                    <td>"Status"</td>
                    <td>{move || status.get()}</td>
                </tr>
            </table>
        </main>
    }
}
