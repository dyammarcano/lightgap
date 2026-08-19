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

/// The video inputs this machine offers, as (device id, label).
///
/// Labels come back blank until camera permission has been granted at least
/// once — a page is not told what hardware is attached before then. The
/// numbered fallback keeps the list usable on a first run, and the list is read
/// again once a stream is open so the real names replace it.
async fn enumerate_cameras() -> Vec<(String, String)> {
    let Ok(devices) = window().navigator().media_devices() else {
        return Vec::new();
    };
    let Ok(promise) = devices.enumerate_devices() else {
        return Vec::new();
    };
    let Ok(list) = JsFuture::from(promise).await else {
        return Vec::new();
    };

    let mut found: Vec<(String, String)> = Vec::new();
    for item in js_sys::Array::from(&list).iter() {
        let info: web_sys::MediaDeviceInfo = item.unchecked_into();
        if info.kind() != web_sys::MediaDeviceKind::Videoinput {
            continue;
        }
        let label = info.label();
        let label = if label.is_empty() {
            format!("Camera {}", found.len() + 1)
        } else {
            label
        };
        found.push((info.device_id(), label));
    }
    found
}

/// Hands the camera back to the system.
///
/// Clearing the video element is not enough on its own: the tracks keep
/// running, the capture light stays lit, and the device stays held. That reads
/// as merely untidy right up until the moment someone picks a different camera,
/// at which point the old one was never released and the new one cannot open.
fn release_stream(video: &web_sys::HtmlVideoElement) {
    if let Some(source) = video.src_object() {
        let stream: web_sys::MediaStream = source.unchecked_into();
        for track in stream.get_tracks().iter() {
            let track: web_sys::MediaStreamTrack = track.unchecked_into();
            track.stop();
        }
    }
    video.set_src_object(None);
}

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

/// A secondary control.
const BTN: &str = "min-h-10 cursor-pointer rounded-lg border border-line bg-panel px-3.5 \
                   text-sm text-ink transition-colors hover:border-beam \
                   disabled:cursor-default disabled:opacity-35 disabled:hover:border-line";

/// The one control that starts the link. There is exactly one primary action on
/// this screen, and it is the one nothing else works without.
const BTN_PRIMARY: &str = "min-h-10 cursor-pointer rounded-lg border border-gold/50 bg-gold/15 \
                           px-3.5 text-sm font-medium text-gold transition-colors \
                           hover:border-gold hover:bg-gold/25 \
                           disabled:cursor-default disabled:opacity-35 \
                           disabled:hover:border-gold/50 disabled:hover:bg-gold/15";

/// One line of the live measurement panel.
#[component]
fn Metric(label: &'static str, #[prop(into)] value: Signal<String>) -> impl IntoView {
    view! {
        <div class="flex items-baseline justify-between gap-3 py-1">
            <span class="text-dim">{label}</span>
            <span class="font-semibold tabular-nums text-ink">{move || value.get()}</span>
        </div>
    }
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
    let (cameras, set_cameras) = signal(Vec::<(String, String)>::new());
    let (camera_id, set_camera_id) = signal(Option::<String>::None);

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
    // Read up front, so the picker is populated before the camera is ever
    // opened rather than only after.
    spawn_local(async move {
        set_cameras.set(enumerate_cameras().await);
    });

    let open_camera = move || {
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
            // An explicitly chosen device wins. facingMode is only a hint, and
            // on a desktop with several cameras attached it selects nothing at
            // all — which is the case the picker exists for. It stays as the
            // fallback because on a phone it is the right default: the rear
            // camera is far better than the front one, and it is the one that
            // gets aimed at the peer.
            match camera_id.get_untracked() {
                Some(id) => {
                    let exact = Object::new();
                    let _ = Reflect::set(&exact, &"exact".into(), &JsValue::from_str(&id));
                    let _ = Reflect::set(&video_cfg, &"deviceId".into(), &exact.into());
                }
                None => {
                    let _ = Reflect::set(&video_cfg, &"facingMode".into(), &"environment".into());
                }
            }
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

            // Permission now exists, so the devices have readable names. Until
            // this point the list held numbered placeholders.
            let named = enumerate_cameras().await;
            if !named.is_empty() {
                set_cameras.set(named);
            }

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

    let close_camera = move || {
        set_camera_on.set(false);
        if let Some(video) = video_ref.get_untracked() {
            release_stream(&video);
        }
    };

    let start_camera = move |_| open_camera();
    let stop_camera = move |_| close_camera();

    let pick_camera = move |ev| {
        let chosen = event_target_value(&ev);
        set_camera_id.set(if chosen.is_empty() {
            None
        } else {
            Some(chosen)
        });

        // Switching while running has to close the old device first. The delay
        // is not politeness: the capture loop needs a tick to notice it should
        // stop, and a camera is not handed to the next caller the instant the
        // last one lets go. Reopening immediately fails intermittently.
        if camera_on.get_untracked() {
            close_camera();
            spawn_local(async move {
                gloo_timers::future::TimeoutFuture::new(250).await;
                open_camera();
            });
        }
    };

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
        <main class="min-h-screen bg-ground text-ink antialiased">
            <div class="mx-auto flex max-w-6xl flex-col gap-4 p-4">

                <header class="flex flex-col gap-1">
                    <div class="flex flex-wrap items-center gap-x-3 gap-y-1">
                        <span class="h-3 w-3 shrink-0 rounded-full bg-gold"></span>
                        <h1 class="text-base font-semibold tracking-wide">"Lightgap"</h1>
                        <span
                            class="rounded-full border border-line px-2 py-0.5 text-xs \
                                   tabular-nums text-dim transition-colors"
                            class=("border-verified/50", move || status.get().peer_found)
                            class=("text-verified", move || status.get().peer_found)
                        >
                            {move || {
                                let s = status.get();
                                match s.role {
                                    Some(r) => format!("{} · {}", s.session_state, r),
                                    None => s.session_state,
                                }
                            }}
                        </span>
                    </div>

                    // Aiming guidance sits directly under the title because it
                    // is the one line worth reading while holding two devices
                    // up at each other.
                    <p
                        class="text-sm text-dim transition-colors"
                        class=("text-verified", move || status.get().peer_found)
                    >
                        {move || status.get().advice}
                    </p>
                </header>

                <section class="grid grid-cols-1 items-start gap-4 lg:grid-cols-[1.6fr_1fr]">

                    <div class="flex flex-col gap-3">
                        // White, always, and never tinted by the theme: this is
                        // the transmitter, and its contrast IS the link's
                        // signal-to-noise ratio.
                        <div
                            class="flex aspect-square items-center justify-center rounded-xl \
                                   bg-white p-3 [&>svg]:block [&>svg]:h-full [&>svg]:w-full"
                            inner_html=move || qr.get()
                        ></div>

                        <div class="flex flex-wrap gap-2">
                            <button
                                class=BTN_PRIMARY
                                on:click=start_camera
                                disabled=move || camera_on.get()
                            >
                                "Start camera"
                            </button>
                            <select
                                class="min-h-10 max-w-56 cursor-pointer rounded-lg border \
                                       border-line bg-panel px-2.5 text-sm text-ink \
                                       transition-colors hover:border-beam"
                                on:change=pick_camera
                                prop:value=move || camera_id.get().unwrap_or_default()
                            >
                                <option value="">"Default camera"</option>
                                {move || {
                                    cameras
                                        .get()
                                        .into_iter()
                                        .map(|(id, label)| {
                                            view! { <option value=id>{label}</option> }
                                        })
                                        .collect_view()
                                }}
                            </select>
                            <button
                                class=BTN
                                on:click=stop_camera
                                disabled=move || !camera_on.get()
                            >
                                "Stop"
                            </button>
                        </div>

                        <div class="flex flex-wrap gap-2">
                            <button class=BTN on:click=pick_and_send>"Send a file…"</button>
                            <button
                                class=BTN
                                on:click=save_received
                                disabled=move || status.get().received_name.is_none()
                            >
                                "Save received…"
                            </button>
                            <button class=BTN on:click=reset>"Reset"</button>
                        </div>

                        <Show when=move || !message.get().is_empty()>
                            <p class="rounded border-l-2 border-beam bg-panel px-3 py-2 text-sm">
                                {move || message.get()}
                            </p>
                        </Show>

                        <Show when=move || status.get().sending.is_some()>
                            <div class="flex items-center gap-3 text-sm">
                                <span class="min-w-36 truncate text-dim">
                                    "Sending "
                                    {move || status.get().sending.unwrap_or_default()}
                                </span>
                                <progress
                                    class="h-2 flex-1 accent-gold"
                                    max="1"
                                    value=move || status.get().send_progress
                                ></progress>
                                <span class="w-12 text-right tabular-nums text-dim">
                                    {move || percent(status.get().send_progress)}
                                </span>
                            </div>
                        </Show>

                        <Show when=move || status.get().receiving.is_some()>
                            <div class="flex items-center gap-3 text-sm">
                                <span class="min-w-36 truncate text-dim">
                                    "Receiving "
                                    {move || status.get().receiving.unwrap_or_default()}
                                </span>
                                <progress
                                    class="h-2 flex-1 accent-beam"
                                    max="1"
                                    value=move || status.get().receive_progress
                                ></progress>
                                <span class="w-12 text-right tabular-nums text-dim">
                                    {move || percent(status.get().receive_progress)}
                                </span>
                            </div>
                        </Show>

                        <Show when=move || status.get().received_name.is_some()>
                            <div
                                class="rounded border-l-2 border-verified bg-panel px-3 py-2 \
                                       text-sm"
                            >
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
                    </div>

                    <div class="flex flex-col gap-3">
                        // Not decoration: without it there is no way to tell
                        // whether the peer's code is framed, in focus, or in
                        // view at all.
                        <div
                            class="relative aspect-video overflow-hidden rounded-xl border \
                                   border-line bg-black"
                        >
                            <video
                                class="h-full w-full object-cover"
                                node_ref=video_ref
                                autoplay=true
                                muted=true
                            ></video>
                            <Show when=move || !camera_on.get()>
                                <div
                                    class="absolute inset-0 flex items-center justify-center \
                                           p-4 text-center text-sm text-dim"
                                >
                                    "Point this device's camera at the other one's screen"
                                </div>
                            </Show>
                        </div>

                        // Always visible rather than behind a panel. These
                        // numbers are how anyone tells a link that is aimed
                        // wrong from one that is aimed right and too dense.
                        <div
                            class="divide-y divide-line/60 rounded-xl border border-line \
                                   bg-panel px-3 py-1 text-sm"
                        >
                            <Metric
                                label="Read rate"
                                value=Signal::derive(move || {
                                    percent(status.get().metrics.decode_rate)
                                })
                            />
                            <Metric
                                label="Pixels per module"
                                value=Signal::derive(move || {
                                    format!("{:.1}", status.get().pixels_per_module)
                                })
                            />
                            <Metric
                                label="Payload per frame"
                                value=Signal::derive(move || {
                                    format!("{} B", status.get().payload_per_frame)
                                })
                            />
                            <Metric
                                label="Frames read"
                                value=Signal::derive(move || {
                                    let m = status.get().metrics;
                                    format!("{} of {}", m.frames_decoded, m.frames_captured)
                                })
                            />
                            // Its own row rather than folded into the one above:
                            // seeing a code and failing to read it means the
                            // density is too high or the focus is off, whereas
                            // seeing none means nothing is aimed at this camera.
                            // They call for opposite reactions.
                            <Metric
                                label="Seen but unread"
                                value=Signal::derive(move || {
                                    let m = status.get().metrics;
                                    format!(
                                        "{}",
                                        m.frames_with_code.saturating_sub(m.frames_decoded),
                                    )
                                })
                            />
                            <Metric
                                label="Capture"
                                value=Signal::derive(move || {
                                    format!("{:.1} ms", capture_ms.get())
                                })
                            />
                            <Metric
                                label="Transport"
                                value=Signal::derive(move || {
                                    format!("{:.1} ms", transport_ms.get())
                                })
                            />
                            <Metric
                                label="Scan"
                                value=Signal::derive(move || {
                                    format!("{:.1} ms", status.get().metrics.decode_ms)
                                })
                            />
                            <Metric
                                label="Codes displayed"
                                value=Signal::derive(move || {
                                    format!("{}", status.get().metrics.frames_displayed)
                                })
                            />
                        </div>
                    </div>
                </section>

                <canvas node_ref=canvas_ref class="hidden"></canvas>

                <details class="rounded-xl border border-line bg-panel px-3 py-2 text-sm">
                    <summary class="cursor-pointer text-dim">"History"</summary>
                    <ul class="mt-2 max-h-48 list-disc overflow-y-auto pl-5">
                        {move || {
                            status
                                .get()
                                .log
                                .into_iter()
                                .rev()
                                .map(|line| {
                                    view! { <li class="py-0.5 text-dim">{line}</li> }
                                })
                                .collect_view()
                        }}
                    </ul>
                </details>
            </div>
        </main>
    }
}
