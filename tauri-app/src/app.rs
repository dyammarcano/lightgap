//! The desktop interface.
//!
//! Two panes: the code this device is transmitting, and what its camera sees.
//! The camera pane is not decoration — without it there is no way to tell
//! whether the peer's code is framed, in focus, or in view at all, and the
//! commonest failure of an optical link is that it was never aimed properly.
//!
//! The interface owns the camera and the display and nothing else. Every
//! decision about what to transmit belongs to the engine, which belongs to the
//! `modem` crate, which is tested without any of this.

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

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "dialog"], js_name = open)]
    async fn dialog_open(options: JsValue) -> JsValue;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "dialog"], js_name = save)]
    async fn dialog_save(options: JsValue) -> JsValue;
}

/// Capture resolution requested from the camera.
///
/// 720p rather than the highest available. Per-frame payload grows with the
/// square of resolution, so more is genuinely better — but every extra pixel
/// also crosses the process boundary and gets scanned, and the point of
/// measuring in the running application is to find out where that trade lands on
/// real hardware rather than to guess.
const CAPTURE_W: u32 = 1280;
const CAPTURE_H: u32 = 720;

/// How often the camera frame is scanned, in milliseconds.
///
/// Independent of the preview, which runs at whatever the camera provides. There
/// is no point scanning faster than codes change on the other side, and scanning
/// is the expensive half of the loop.
const SCAN_INTERVAL_MS: u32 = 60;

/// How often the on-screen code is refreshed, in milliseconds.
///
/// Faster than the code actually changes. The engine owns the hold time and
/// simply returns the same code until it is due to advance, so polling more
/// often costs a string copy and keeps the display in step with the engine
/// rather than with this timer.
const DISPLAY_INTERVAL_MS: u32 = 40;

/// How often the status panel refreshes.
const STATUS_INTERVAL_MS: u32 = 250;

#[derive(Debug, Clone, Default, Deserialize)]
struct Metrics {
    frames_captured: u64,
    frames_with_code: u64,
    frames_decoded: u64,
    decode_ms: f32,
    frames_displayed: u64,
    decode_rate: f32,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Status {
    session_state: String,
    role: Option<String>,
    peer_found: bool,
    sending: Option<String>,
    send_progress: f32,
    receiving: Option<String>,
    receive_progress: f32,
    received_name: Option<String>,
    received_len: Option<usize>,
    advice: String,
    pixels_per_module: f32,
    payload_per_frame: usize,
    metrics: Metrics,
    log: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FrameOutcome {
    #[allow(dead_code)]
    found_code: bool,
    #[allow(dead_code)]
    decoded: bool,
    #[allow(dead_code)]
    decode_ms: f32,
}

/// Rolling mean, so the timings do not flicker.
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

/// Converts the RGBA `getImageData` returns into 8-bit luma, prefixed with the
/// width and height as little-endian u32.
///
/// The dimensions travel inside the buffer because the frame has to arrive as a
/// raw binary body, and that only happens when the typed array is the entire
/// invoke argument. Passing them as a second argument would wrap the pixels in
/// an object and send each one through JSON as a number.
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

fn percent(v: f32) -> String {
    format!("{:.0}%", (v * 100.0).clamp(0.0, 100.0))
}

#[component]
pub fn App() -> impl IntoView {
    let video_ref: NodeRef<leptos::html::Video> = NodeRef::new();
    let canvas_ref: NodeRef<leptos::html::Canvas> = NodeRef::new();

    let (qr, set_qr) = signal(String::new());
    let (status, set_status) = signal(Status::default());
    let (camera_on, set_camera_on) = signal(false);
    let (message, set_message) = signal(String::new());
    let (capture_ms, set_capture_ms) = signal(0.0f64);
    let (transport_ms, set_transport_ms) = signal(0.0f64);

    // --- the displayed code -------------------------------------------------
    spawn_local(async move {
        loop {
            let res = invoke("current_qr", JsValue::UNDEFINED).await;
            if let Some(svg) = res.as_string() {
                set_qr.set(svg);
            }
            gloo_timers::future::TimeoutFuture::new(DISPLAY_INTERVAL_MS).await;
        }
    });

    // --- the status panel ---------------------------------------------------
    spawn_local(async move {
        loop {
            let res = invoke("status", JsValue::UNDEFINED).await;
            if let Ok(s) = serde_wasm_bindgen::from_value::<Status>(res) {
                set_status.set(s);
            }
            gloo_timers::future::TimeoutFuture::new(STATUS_INTERVAL_MS).await;
        }
    });

    // --- the camera ---------------------------------------------------------
    let start_camera = move |_| {
        if camera_on.get_untracked() {
            return;
        }
        set_camera_on.set(true);
        set_message.set("opening camera...".into());

        spawn_local(async move {
            let devices = match window().navigator().media_devices() {
                Ok(d) => d,
                Err(e) => {
                    set_message.set(format!("no camera available: {e:?}"));
                    set_camera_on.set(false);
                    return;
                }
            };

            let video_cfg = Object::new();
            let _ = Reflect::set(&video_cfg, &"width".into(), &JsValue::from(CAPTURE_W));
            let _ = Reflect::set(&video_cfg, &"height".into(), &JsValue::from(CAPTURE_H));
            // The rear camera when there is one. On a phone it is far better
            // than the front camera and it is the one that will be aimed at the
            // peer; on a laptop this is simply ignored.
            let _ = Reflect::set(&video_cfg, &"facingMode".into(), &"environment".into());
            let constraints = Object::new();
            let _ = Reflect::set(&constraints, &"video".into(), &video_cfg.into());
            let _ = Reflect::set(&constraints, &"audio".into(), &JsValue::FALSE);
            let constraints: web_sys::MediaStreamConstraints = constraints.unchecked_into();

            let promise = match devices.get_user_media_with_constraints(&constraints) {
                Ok(p) => p,
                Err(e) => {
                    set_message.set(format!("camera refused: {e:?}"));
                    set_camera_on.set(false);
                    return;
                }
            };
            let stream = match JsFuture::from(promise).await {
                Ok(s) => s,
                Err(e) => {
                    set_message.set(format!("camera permission denied: {e:?}"));
                    set_camera_on.set(false);
                    return;
                }
            };
            let stream: web_sys::MediaStream = stream.unchecked_into();

            let Some(video) = video_ref.get_untracked() else {
                set_message.set("the video element is missing".into());
                set_camera_on.set(false);
                return;
            };
            video.set_src_object(Some(&stream));
            let _ = video.play();
            set_message.set(String::new());

            let mut r_capture = Rolling::default();
            let mut r_transport = Rolling::default();
            let mut tick = 0u64;

            while camera_on.get_untracked() {
                let (Some(canvas), Some(video)) =
                    (canvas_ref.get_untracked(), video_ref.get_untracked())
                else {
                    break;
                };
                canvas.set_width(CAPTURE_W);
                canvas.set_height(CAPTURE_H);

                let ctx = canvas
                    .get_context("2d")
                    .ok()
                    .flatten()
                    .and_then(|c| c.dyn_into::<web_sys::CanvasRenderingContext2d>().ok());
                let Some(ctx) = ctx else {
                    set_message.set("no 2d canvas context".into());
                    break;
                };

                let t_capture = now_ms();
                if ctx
                    .draw_image_with_html_video_element_and_dw_and_dh(
                        &video,
                        0.0,
                        0.0,
                        f64::from(CAPTURE_W),
                        f64::from(CAPTURE_H),
                    )
                    .is_err()
                {
                    gloo_timers::future::TimeoutFuture::new(SCAN_INTERVAL_MS).await;
                    continue;
                }
                let Ok(image_data) =
                    ctx.get_image_data(0.0, 0.0, f64::from(CAPTURE_W), f64::from(CAPTURE_H))
                else {
                    gloo_timers::future::TimeoutFuture::new(SCAN_INTERVAL_MS).await;
                    continue;
                };
                let rgba = image_data.data();
                let grey = rgba_to_grey_with_header(&rgba, CAPTURE_W, CAPTURE_H);
                r_capture.push(now_ms() - t_capture);

                // The typed array goes across ALONE. Nesting it in an object
                // would send it through JSON.stringify, turning every pixel into
                // a number in text: roughly four megabytes per frame instead of
                // nine hundred kilobytes.
                let t_transport = now_ms();
                let buf = Uint8Array::new_with_length(grey.len() as u32);
                buf.copy_from(&grey);
                let res = invoke("on_frame", buf.into()).await;
                let elapsed = now_ms() - t_transport;

                if let Ok(outcome) = serde_wasm_bindgen::from_value::<FrameOutcome>(res) {
                    // What is attributable to transport is the wall clock minus
                    // what the backend reports having spent scanning. Keeping
                    // them apart is what says which fallback to reach for if the
                    // loop turns out too slow.
                    r_transport.push((elapsed - f64::from(outcome.decode_ms)).max(0.0));
                }

                tick += 1;
                if tick.is_multiple_of(5) {
                    set_capture_ms.set(r_capture.mean());
                    set_transport_ms.set(r_transport.mean());
                }

                gloo_timers::future::TimeoutFuture::new(SCAN_INTERVAL_MS).await;
            }
        });
    };

    let stop_camera = move |_| set_camera_on.set(false);

    // --- file actions -------------------------------------------------------
    let pick_and_send = move |_| {
        spawn_local(async move {
            let opts = Object::new();
            let _ = Reflect::set(&opts, &"multiple".into(), &JsValue::FALSE);
            let _ = Reflect::set(&opts, &"directory".into(), &JsValue::FALSE);
            let picked = dialog_open(opts.into()).await;
            let Some(path) = picked.as_string() else {
                return;
            };

            let args = Object::new();
            let _ = Reflect::set(&args, &"path".into(), &JsValue::from_str(&path));
            let res = invoke("send_file", args.into()).await;
            set_message.set(
                res.as_string()
                    .unwrap_or_else(|| "could not read that file".into()),
            );
        });
    };

    let save_received = move |_| {
        spawn_local(async move {
            let suggested = invoke("received_name", JsValue::UNDEFINED)
                .await
                .as_string()
                .unwrap_or_default();

            let opts = Object::new();
            let _ = Reflect::set(&opts, &"defaultPath".into(), &JsValue::from_str(&suggested));
            let picked = dialog_save(opts.into()).await;
            let Some(path) = picked.as_string() else {
                return;
            };

            let args = Object::new();
            let _ = Reflect::set(&args, &"path".into(), &JsValue::from_str(&path));
            let res = invoke("save_received", args.into()).await;
            set_message.set(res.as_string().unwrap_or_else(|| "could not save".into()));
        });
    };

    let reset = move |_| {
        spawn_local(async move {
            invoke("reset", JsValue::UNDEFINED).await;
            set_message.set("session reset".into());
        });
    };

    view! {
        <main class="app">
            <header class="bar">
                <h1>"qr_comm"</h1>
                <span class="state">
                    {move || {
                        let s = status.get();
                        match s.role {
                            Some(r) => format!("{} · {}", s.session_state, r),
                            None => s.session_state,
                        }
                    }}
                </span>
                <span class="advice" class:good=move || status.get().peer_found>
                    {move || status.get().advice}
                </span>
            </header>

            <section class="panes">
                <div class="qr" inner_html=move || qr.get()></div>

                <div class="camera">
                    <video node_ref=video_ref autoplay=true muted=true></video>
                    <Show when=move || !camera_on.get()>
                        <div class="camera-off">
                            "Point this device's camera at the other one's screen"
                        </div>
                    </Show>
                </div>
            </section>

            <canvas node_ref=canvas_ref style="display:none"></canvas>

            <section class="controls">
                <button on:click=start_camera disabled=move || camera_on.get()>
                    "Start camera"
                </button>
                <button on:click=stop_camera disabled=move || !camera_on.get()>
                    "Stop camera"
                </button>
                <button on:click=pick_and_send>"Send a file…"</button>
                <button
                    on:click=save_received
                    disabled=move || status.get().received_name.is_none()
                >
                    "Save received…"
                </button>
                <button on:click=reset>"Reset"</button>
            </section>

            <Show when=move || !message.get().is_empty()>
                <p class="message">{move || message.get()}</p>
            </Show>

            <section class="transfers">
                <Show when=move || status.get().sending.is_some()>
                    <div class="transfer">
                        <span class="label">
                            "Sending " {move || status.get().sending.unwrap_or_default()}
                        </span>
                        <progress max="1" value=move || status.get().send_progress></progress>
                        <span class="pct">{move || percent(status.get().send_progress)}</span>
                    </div>
                </Show>

                <Show when=move || status.get().receiving.is_some()>
                    <div class="transfer">
                        <span class="label">
                            "Receiving " {move || status.get().receiving.unwrap_or_default()}
                        </span>
                        <progress max="1" value=move || status.get().receive_progress></progress>
                        <span class="pct">{move || percent(status.get().receive_progress)}</span>
                    </div>
                </Show>

                <Show when=move || status.get().received_name.is_some()>
                    <div class="arrived">
                        {move || {
                            let s = status.get();
                            format!(
                                "{} arrived ({} B) — save it before resetting",
                                s.received_name.unwrap_or_default(),
                                s.received_len.unwrap_or(0),
                            )
                        }}
                    </div>
                </Show>
            </section>

            <details class="link">
                <summary>"Link"</summary>
                <table>
                    <tr>
                        <td>"Payload per frame"</td>
                        <td>{move || format!("{} B", status.get().payload_per_frame)}</td>
                    </tr>
                    <tr>
                        <td>"Pixels per module"</td>
                        <td>{move || format!("{:.1}", status.get().pixels_per_module)}</td>
                    </tr>
                    <tr>
                        <td>"Frames read"</td>
                        <td>
                            {move || {
                                let m = status.get().metrics;
                                format!("{} of {}", m.frames_decoded, m.frames_captured)
                            }}
                        </td>
                    </tr>
                    <tr>
                        <td>"Read rate"</td>
                        <td>{move || percent(status.get().metrics.decode_rate)}</td>
                    </tr>
                    <tr>
                        // Worth its own row rather than folded into the one
                        // above: seeing a code and failing to read it means the
                        // density is too high or the focus is off, whereas
                        // seeing none means nothing is aimed at this camera.
                        // They call for opposite reactions.
                        <td>"Codes seen but unread"</td>
                        <td>
                            {move || {
                                let m = status.get().metrics;
                                format!("{}", m.frames_with_code.saturating_sub(m.frames_decoded))
                            }}
                        </td>
                    </tr>
                    <tr>
                        <td>"Capture"</td>
                        <td>{move || format!("{:.1} ms", capture_ms.get())}</td>
                    </tr>
                    <tr>
                        <td>"Transport"</td>
                        <td>{move || format!("{:.1} ms", transport_ms.get())}</td>
                    </tr>
                    <tr>
                        <td>"Scan"</td>
                        <td>{move || format!("{:.1} ms", status.get().metrics.decode_ms)}</td>
                    </tr>
                    <tr>
                        <td>"Codes displayed"</td>
                        <td>{move || format!("{}", status.get().metrics.frames_displayed)}</td>
                    </tr>
                </table>
            </details>

            <details class="history">
                <summary>"History"</summary>
                <ul>
                    {move || {
                        status
                            .get()
                            .log
                            .into_iter()
                            .rev()
                            .map(|line| view! { <li>{line}</li> })
                            .collect_view()
                    }}
                </ul>
            </details>
        </main>
    }
}
