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

use js_sys::{Object, Reflect};
use leptos::ev;
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
const CAPTURE_W: u32 = 1920;
const CAPTURE_H: u32 = 1080;

/// Pixel budget for the wide search, when there is no code to track.
///
/// About a 1280x720 frame, which is what this link is measured to acquire at:
/// a code filling half the frame resolves roughly seven pixels per module there
/// and does get read. It cannot go far below that — the search must succeed
/// once before tracking has anything to track, and searching cheaper would be
/// searching for something that can never be found.
///
/// Nor should it go far above. Raising it to 1.3 megapixels, to match what the
/// camera was newly being asked for, took the search from about 130 ms a frame
/// to 285 ms and bought nothing: acquisition already worked at the lower
/// figure. The extra resolution is worth paying for only once there is a region
/// to spend it on, which is what the tracking budget below is for.
const SEARCH_BUDGET_PX: f64 = 950_000.0;

/// Pixel budget once a code is being tracked.
///
/// Small, and deliberately applied to a small region: the point of tracking is
/// to spend the budget on the part of the image that has the code in it. The
/// same number of pixels over a fifth of the frame is five times the pixels per
/// module, which is the measurement that decides whether anything decodes.
const TRACK_BUDGET_PX: f64 = 420_000.0;

/// How much wider than the code the tracked region is drawn.
///
/// The devices are held by hand or propped on a table; the code moves between
/// frames. Too tight and it walks out of the region, which costs a full
/// re-acquisition — far more than the margin ever costs.
const ROI_MARGIN: f64 = 0.45;

/// Missed frames tolerated before giving up on the tracked region.
///
/// Not one. A single miss is ordinary — a blink of focus hunting, a hand
/// moving — and throwing the region away for it would mean re-acquiring
/// constantly at exactly the moment the link is working.
const ROI_PATIENCE: u32 = 4;

/// How long the scan loop yields between frames, in milliseconds.
///
/// A yield, not a throttle. The loop is paced by what capture and decode
/// actually cost; this only hands control back so the interface can paint and
/// the code on screen can advance. It used to be sixty, which made sense when a
/// full-frame scan dominated the cycle — now that a tracked region costs a
/// fraction of that, sixty milliseconds of deliberate waiting was the largest
/// single item in the budget.
const SCAN_INTERVAL_MS: u32 = 10;

/// How often the on-screen code is refreshed, in milliseconds.
///
/// Faster than the code actually changes. The engine owns the hold time and
/// simply returns the same code until it is due to advance, so polling more
/// often costs a string copy and keeps the display in step with the engine
/// rather than with this timer.
const DISPLAY_INTERVAL_MS: u32 = 40;

/// How often the status panel refreshes.
const STATUS_INTERVAL_MS: u32 = 250;

/// A throughput figure, in the unit that keeps it readable.
///
/// Bytes per second up to a kilobyte, because on this link that is the range
/// most of the interesting numbers live in and rounding them to "0.1 kB/s"
/// would throw away the part worth watching.
fn rate(bps: f32) -> String {
    if bps < 1000.0 {
        format!("{bps:.0} B/s")
    } else {
        format!("{:.1} kB/s", bps / 1000.0)
    }
}

/// Spaces the authentication digits out.
///
/// They exist to be read aloud and compared by two people, and an unbroken run
/// of six digits is exactly the shape people lose their place in halfway
/// through. This is the whole defence against a man in the middle, so how easy
/// it is to say is not cosmetic.
fn spaced(digits: &str) -> String {
    digits
        .chars()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// How often the clock is refreshed.
///
/// Ten seconds for a display that only shows minutes: the point is that it
/// never sits visibly wrong, not that it is precise.
const CLOCK_INTERVAL_MS: u32 = 10_000;

/// Fraction of the frame that may sit at the top of the sensor's range.
///
/// Above this the picture is not bright, it is clipped: the peer's screen has
/// pushed past what the sensor can distinguish and the modules inside it have
/// merged into one white shape. A person looking at the preview sees a lit
/// screen and nothing wrong.
const CLIPPED_LIMIT: f32 = 0.06;

/// Below this mean the frame is too dark to hold a code at all.
const DARK_MEAN: f32 = 28.0;

/// How much of the exposure range one step covers.
const EXPOSURE_STEP: f64 = 0.12;

/// How often the transmitter is reconsidered while calibrating down.
///
/// Slower than the status poll, and far slower than the frame rate. Each step
/// has to survive long enough for the peer to measure it and say so, and its
/// report only refreshes when its own window closes. Stepping faster would be
/// stepping on evidence that had not arrived yet.
const CALIBRATE_INTERVAL_MS: u32 = 2_000;

/// How much output one step gives up.
const DESCEND_STEP: f32 = 0.06;

/// The peer must be reading this well before anything is given up.
const DESCEND_ABOVE: f32 = 0.90;

/// Below this, the last step went too far and is taken back.
const RECOVER_BELOW: f32 = 0.75;

/// How often the battery is re-read.
const BATTERY_INTERVAL_MS: u32 = 60_000;

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The wall clock, as `23:18` and `Wed 19 Aug`.
///
/// Built here rather than left to the system furniture because this window runs
/// fullscreen: on desktop that hides the taskbar and its clock outright, and a
/// transfer over this link is measured in tens of minutes, which is long enough
/// that whoever set it going will want to know how long they have been waiting.
fn wall_clock() -> (String, String) {
    let now = js_sys::Date::new_0();
    let time = format!("{:02}:{:02}", now.get_hours(), now.get_minutes());
    let date = format!(
        "{} {} {}",
        WEEKDAYS[(now.get_day() as usize) % 7],
        now.get_date(),
        MONTHS[(now.get_month() as usize) % 12],
    );
    (time, date)
}

/// Battery charge as a fraction, and whether it is on power.
///
/// Reached through `Reflect` rather than a typed binding: the Battery Status API
/// is optional, and a platform that does not offer it should leave the readout
/// absent rather than fail to build.
async fn battery() -> Option<(f64, bool)> {
    let nav = window().navigator();
    let get: js_sys::Function = Reflect::get(&nav, &"getBattery".into())
        .ok()?
        .dyn_into()
        .ok()?;
    let promise: js_sys::Promise = get.call0(&nav).ok()?.dyn_into().ok()?;
    let manager = JsFuture::from(promise).await.ok()?;

    let level = Reflect::get(&manager, &"level".into()).ok()?.as_f64()?;
    let charging = Reflect::get(&manager, &"charging".into())
        .ok()?
        .as_bool()
        .unwrap_or(false);
    Some((level, charging))
}

/// Where the chosen camera is remembered between runs.
const CAMERA_KEY: &str = "lightgap.camera";

/// And its label, which is what actually survives a restart.
///
/// Device ids do not. A browser may re-salt them between sessions, so the id
/// saved yesterday can name nothing today — and asking for it with `exact`
/// throws rather than degrading. The label ("camera 1, facing front") stays put,
/// so it is what the preference is really keyed on; the id is a fast path for
/// when it happens to still be valid.
const CAMERA_LABEL_KEY: &str = "lightgap.camera.label";

/// Where the display brightness is remembered between runs.
const BRIGHTNESS_KEY: &str = "lightgap.brightness";

/// Where the code's white level is remembered between runs.
const CODE_LIGHT_KEY: &str = "lightgap.codelight";

/// Dimmest the code is allowed to go, as a fraction of full white.
///
/// Deliberately far below what usually reads. Every step down narrows the gap
/// between a light module and a dark one, and that gap is the decoder's entire
/// signal — but where the useful bottom sits depends on the room, the sensor
/// and how close the two devices are, and none of those are knowable from here.
/// The range therefore goes well past the point of usefulness and says so,
/// rather than stopping somewhere that only looks principled.
const CODE_LIGHT_FLOOR: f32 = 0.08;

/// Below this, the mask is usually too heavy to decode through.
///
/// A guide rather than a limit: a very dark room or a very close camera can go
/// lower, which is exactly why this marks the value instead of capping it.
const CODE_LIGHT_SAFE: f32 = 0.45;

/// Moves this end's output by one step, and says whether there was room.
///
/// The backlight goes first where it exists, because it moves the light and the
/// dark together and so keeps the contrast ratio that the mask cannot. The mask
/// is what is left once there is no backlight to give back.
///
/// Returns false when the requested direction has nowhere left to go, which is
/// what tells a blind sweep it has reached the end and should start over.
fn step_light(
    by: f32,
    dimmable: bool,
    code_light: ReadSignal<f32>,
    set_code_light: WriteSignal<f32>,
    brightness: ReadSignal<f32>,
    set_brightness: WriteSignal<f32>,
) -> bool {
    if dimmable {
        let level = brightness.get_untracked();
        let next = (level + by).clamp(0.05, 1.0);
        if (next - level).abs() > f32::EPSILON {
            set_brightness.set(next);
            apply_brightness(next);
            return true;
        }
    }

    let light = code_light.get_untracked();
    let next = (light + by).clamp(CODE_LIGHT_FLOOR, 1.0);
    if (next - light).abs() > f32::EPSILON {
        set_code_light.set(next);
        return true;
    }
    false
}

/// Pushes a brightness level down to the platform.
///
/// Fire and forget: the interface already shows the level it asked for, and a
/// platform that refuses is one where the control is hidden anyway.
fn apply_brightness(level: f32) {
    spawn_local(async move {
        let args = Object::new();
        let _ = Reflect::set(&args, &"level".into(), &JsValue::from_f64(f64::from(level)));
        invoke("set_brightness", args.into()).await;
    });
}

fn remember(key: &str, value: &str) {
    if let Ok(Some(store)) = window().local_storage() {
        let _ = store.set_item(key, value);
    }
}

fn recall(key: &str) -> Option<String> {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|store| store.get_item(key).ok().flatten())
}

fn forget(key: &str) {
    if let Ok(Some(store)) = window().local_storage() {
        let _ = store.remove_item(key);
    }
}

/// Moves the camera's exposure to a point in its own range, 0..1.
///
/// Fire and forget, and deliberately tolerant: cameras differ in what they
/// accept and a refusal here is information, not a failure.
fn apply_exposure(video_ref: NodeRef<leptos::html::Video>, level: f64) {
    spawn_local(async move {
        let Some(video) = video_ref.get_untracked() else {
            return;
        };
        let Some(source) = video.src_object() else {
            return;
        };
        let stream: web_sys::MediaStream = source.unchecked_into();
        let Ok(track) = stream
            .get_video_tracks()
            .get(0)
            .dyn_into::<web_sys::MediaStreamTrack>()
        else {
            return;
        };

        let Some((min, max)) = exposure_range(&track) else {
            return;
        };
        let step = Object::new();
        let _ = Reflect::set(&step, &"exposureMode".into(), &"manual".into());
        let _ = Reflect::set(
            &step,
            &"exposureTime".into(),
            &JsValue::from_f64(min + (max - min) * level),
        );
        let advanced = js_sys::Array::new();
        advanced.push(&step);
        let constraints = Object::new();
        let _ = Reflect::set(&constraints, &"advanced".into(), &advanced);

        if let Ok(apply) = Reflect::get(&track, &"applyConstraints".into()) {
            if let Ok(apply) = apply.dyn_into::<js_sys::Function>() {
                if let Ok(promise) = apply.call1(&track, &constraints) {
                    if let Ok(promise) = promise.dyn_into::<js_sys::Promise>() {
                        let _ = JsFuture::from(promise).await;
                    }
                }
            }
        }
    });
}

/// The exposure times this camera will accept, if it accepts any.
fn exposure_range(track: &web_sys::MediaStreamTrack) -> Option<(f64, f64)> {
    let caps = Reflect::get(track, &"getCapabilities".into())
        .ok()?
        .dyn_into::<js_sys::Function>()
        .ok()?
        .call0(track)
        .ok()?;
    let range = Reflect::get(&caps, &"exposureTime".into()).ok()?;
    let min = Reflect::get(&range, &"min".into()).ok()?.as_f64()?;
    let max = Reflect::get(&range, &"max".into()).ok()?.as_f64()?;
    (max > min).then_some((min, max))
}

/// Takes the camera off automatic exposure, as far as it will allow.
///
/// This is the one camera setting that matters here, and leaving it alone is
/// what breaks the link in a dark room. Auto-exposure meters the whole frame;
/// a dark room drives it wide open; and the one thing in the frame that is not
/// dark — the peer's screen — clips to flat white with every module inside it
/// run together. The picture looks fine to a person and carries no code at all.
///
/// What a camera will accept varies, and most laptop webcams accept none of it,
/// so every step is attempted separately and a refusal is not an error. The
/// capabilities are logged either way: knowing that a camera offers nothing is
/// worth as much as knowing what it offers, and guessing is what this whole
/// project is trying to stop doing.
async fn restrain_exposure(stream: &web_sys::MediaStream) -> bool {
    let tracks = stream.get_video_tracks();
    let Some(track) = tracks.get(0).dyn_into::<web_sys::MediaStreamTrack>().ok() else {
        return false;
    };

    let caps = Reflect::get(&track, &"getCapabilities".into())
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .and_then(|f| f.call0(&track).ok());
    if let Some(caps) = &caps {
        if let Ok(text) = js_sys::JSON::stringify(caps) {
            web_sys::console::log_1(&format!("camera capabilities: {text}").into());
        }
    }

    let supports = |name: &str| -> bool {
        caps.as_ref()
            .and_then(|c| Reflect::get(c, &name.into()).ok())
            .is_some_and(|v| !v.is_undefined() && !v.is_null())
    };

    let advanced = js_sys::Array::new();
    if supports("exposureMode") {
        let step = Object::new();
        let _ = Reflect::set(&step, &"exposureMode".into(), &"manual".into());
        // Toward the short end of whatever the camera offers. The subject is a
        // lit screen; there is no shortage of light coming from it.
        if let Some(min) = caps
            .as_ref()
            .and_then(|c| Reflect::get(c, &"exposureTime".into()).ok())
            .and_then(|r| Reflect::get(&r, &"min".into()).ok())
            .and_then(|v| v.as_f64())
        {
            let _ = Reflect::set(&step, &"exposureTime".into(), &JsValue::from_f64(min * 4.0));
        }
        advanced.push(&step);
    }
    if supports("exposureCompensation") {
        let step = Object::new();
        if let Some(min) = caps
            .as_ref()
            .and_then(|c| Reflect::get(c, &"exposureCompensation".into()).ok())
            .and_then(|r| Reflect::get(&r, &"min".into()).ok())
            .and_then(|v| v.as_f64())
        {
            let _ = Reflect::set(
                &step,
                &"exposureCompensation".into(),
                &JsValue::from_f64(min),
            );
        }
        advanced.push(&step);
    }

    if advanced.length() == 0 {
        web_sys::console::log_1(
            &"camera offers no exposure control; a dark room will blow out a screen".into(),
        );
        return false;
    }

    let constraints = Object::new();
    let _ = Reflect::set(&constraints, &"advanced".into(), &advanced);
    if let Ok(apply) = Reflect::get(&track, &"applyConstraints".into()) {
        if let Ok(apply) = apply.dyn_into::<js_sys::Function>() {
            if let Ok(promise) = apply.call1(&track, &constraints) {
                if let Ok(promise) = promise.dyn_into::<js_sys::Promise>() {
                    let _ = JsFuture::from(promise).await;
                }
            }
        }
    }

    exposure_range(&track).is_some()
}

/// Asks for a camera stream, optionally pinning one specific device.
///
/// Split out from the caller so the same request can be retried without the
/// device pinned, which is what makes a remembered camera safe to ask for.
async fn request_stream(device: Option<&str>) -> Result<web_sys::MediaStream, JsValue> {
    let devices = window().navigator().media_devices()?;

    let video_cfg = Object::new();
    // Asked for as ideals, not exact sizes. What actually matters is pixels per
    // module on the peer's code, so more sensor is better — but a camera that
    // cannot do this should hand back what it has rather than refuse to open.
    // Whatever arrives is read back off the video element and used as-is.
    let width = Object::new();
    Reflect::set(&width, &"ideal".into(), &JsValue::from(CAPTURE_W))?;
    Reflect::set(&video_cfg, &"width".into(), &width.into())?;
    let height = Object::new();
    Reflect::set(&height, &"ideal".into(), &JsValue::from(CAPTURE_H))?;
    Reflect::set(&video_cfg, &"height".into(), &height.into())?;
    match device {
        // An explicitly chosen device wins.
        Some(id) => {
            let exact = Object::new();
            Reflect::set(&exact, &"exact".into(), &JsValue::from_str(id))?;
            Reflect::set(&video_cfg, &"deviceId".into(), &exact.into())?;
        }
        // The front camera, and not because it is the better sensor — it is
        // not. It is the one on the same face as the display.
        //
        // This application aims a *screen* at the peer, so that screen faces
        // them and the rear camera necessarily faces away. Defaulting to
        // "environment" pointed the only camera that matters at whatever
        // happened to be behind the device.
        //
        // facingMode is only a hint in any case: on a desktop with several
        // cameras attached it selects nothing at all, which is what the picker
        // is for.
        None => {
            Reflect::set(&video_cfg, &"facingMode".into(), &"user".into())?;
        }
    }

    let constraints = Object::new();
    Reflect::set(&constraints, &"video".into(), &video_cfg.into())?;
    Reflect::set(&constraints, &"audio".into(), &JsValue::FALSE)?;
    let constraints: web_sys::MediaStreamConstraints = constraints.unchecked_into();

    let stream = JsFuture::from(devices.get_user_media_with_constraints(&constraints)?).await?;
    Ok(stream.unchecked_into())
}

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
    sees_peer: bool,
    peer_sees_us: bool,
    sending: Option<String>,
    send_progress: f32,
    receiving: Option<String>,
    receive_progress: f32,
    received_name: Option<String>,
    received_len: Option<usize>,
    advice: String,
    pixels_per_module: f32,
    payload_per_frame: usize,
    offered_bps: f32,
    delivered_bps: f32,
    peer_read_quality: Option<f32>,
    peer_sees_anything: Option<bool>,
    sas: Option<String>,
    pairing_expires_in: Option<u64>,
    metrics: Metrics,
    log: Vec<String>,
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
fn rgba_to_grey(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let px = (width as usize) * (height as usize);
    let mut out = Vec::with_capacity(px);
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

/// One half of the link check.
///
/// Two of these rather than one, because the link fails one direction at a time
/// and a single "connected" light would hide which. Ticked on one side and not
/// the other says plainly which device to move.
#[component]
fn LinkCheck(label: &'static str, #[prop(into)] ok: Signal<bool>) -> impl IntoView {
    view! {
        <span
            class="flex items-center gap-1.5 transition-colors"
            class=("text-verified", move || ok.get())
            class=("text-dim", move || !ok.get())
        >
            <span class="text-sm leading-none">
                {move || if ok.get() { "✓" } else { "○" }}
            </span>
            {label}
        </span>
    }
}

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
    // Distinct from `camera_on`, which turns true the moment opening *starts*.
    // A video element with no source yet paints its own oversized play control,
    // so the gap between asking for a camera and getting one is exactly the
    // window in which it must stay hidden.
    let (streaming, set_streaming) = signal(false);
    let (message, set_message) = signal(String::new());
    let (capture_ms, set_capture_ms) = signal(0.0f64);
    let (transport_ms, set_transport_ms) = signal(0.0f64);
    let (scan_area, set_scan_area) = signal(String::from("—"));
    let (clipped, set_clipped) = signal(0.0f32);
    let (frame_mean, set_frame_mean) = signal(0.0f32);
    let (exposure, set_exposure) = signal(-1.0f64);
    let (cameras, set_cameras) = signal(Vec::<(String, String)>::new());
    let (camera_id, set_camera_id) = signal(Option::<String>::None);
    let (dimmable, set_dimmable) = signal(false);
    let (code_light, set_code_light) = signal(1.0f32);
    let (clock, set_clock) = signal(wall_clock());
    let (charge, set_charge) = signal(Option::<(f64, bool)>::None);
    let (brightness, set_brightness) = signal(1.0f32);

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
        loop {
            set_clock.set(wall_clock());
            gloo_timers::future::TimeoutFuture::new(CLOCK_INTERVAL_MS).await;
        }
    });

    spawn_local(async move {
        loop {
            set_charge.set(battery().await);
            gloo_timers::future::TimeoutFuture::new(BATTERY_INTERVAL_MS).await;
        }
    });

    // Asked once. Where the platform does not hand out the control there is no
    // slider, rather than a slider that silently does nothing.
    spawn_local(async move {
        let ok = invoke("brightness_controllable", JsValue::UNDEFINED)
            .await
            .as_bool()
            .unwrap_or(false);
        set_dimmable.set(ok);
        if ok {
            // Always full, never the value this ran at last time.
            //
            // The link has to exist before anything about it can be measured,
            // and the surest way to be seen is to be as bright as the hardware
            // allows. Restoring a dim setting from a previous session means
            // starting invisible and hoping to climb out of it — and if the
            // peer starts dim too, neither can see the other to know that it
            // should. Calibration goes the other way: begin somewhere known to
            // work, then give output back while it still does.
            set_brightness.set(1.0);
            apply_brightness(1.0);
        }
    });

    let on_code_light = move |ev| {
        let raw = event_target_value(&ev);
        let Ok(level) = raw.parse::<f32>() else {
            return;
        };
        set_code_light.set(level);
        remember(CODE_LIGHT_KEY, &raw);
    };

    let on_brightness = move |ev| {
        let raw = event_target_value(&ev);
        let Ok(level) = raw.parse::<f32>() else {
            return;
        };
        set_brightness.set(level);
        remember(BRIGHTNESS_KEY, &raw);
        apply_brightness(level);
    };

    // The receiving half of the calibration, and the mirror of the one below.
    //
    // That one adjusts what this device emits, from what the peer reports. This
    // one adjusts what this device's camera admits, from what it can measure
    // for itself — and it can, because a clipped frame is visible in the pixels
    // without anyone having to say so.
    //
    // It only acts while nothing is being read. A link that works is not
    // improved by moving the exposure, and every move costs a re-acquisition.
    spawn_local(async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(CALIBRATE_INTERVAL_MS).await;
            if !streaming.get_untracked() {
                continue;
            }
            let s = status.get_untracked();
            if s.sees_peer {
                continue;
            }

            let level = exposure.get_untracked();
            if level < 0.0 {
                continue; // this camera offers no exposure control
            }

            let next = if clipped.get_untracked() > CLIPPED_LIMIT {
                // Saturated. Nothing in the bright part of this frame can be
                // told apart, which is exactly where the code is.
                (level - EXPOSURE_STEP).max(0.0)
            } else if frame_mean.get_untracked() < DARK_MEAN {
                (level + EXPOSURE_STEP).min(1.0)
            } else {
                continue;
            };

            if (next - level).abs() > f64::EPSILON {
                set_exposure.set(next);
                apply_exposure(video_ref, next);
            }
        }
    });

    // The one closed loop in the interface, and it only ever acts on this end's
    // own transmitter.
    //
    // It starts at full output, because a link that does not exist cannot be
    // measured and being as bright as the hardware allows is the surest way to
    // be read. What happens next depends on what the peer can tell us, and the
    // three cases are genuinely different.
    //
    // The peer reports it is reading well: give output back a step at a time
    // and find the floor. Cheaper, cooler, and further from the point where a
    // close-up sensor clips.
    //
    // The peer reports it is finding a code and failing to read it: that is too
    // much light for that camera, not too little. Step down.
    //
    // The peer reports nothing at all — which is the case that matters most,
    // because it is what happens when this display is unreadable enough that
    // the peer never discovers us and so has nothing to say. Full output is not
    // automatically right there: a screen at maximum in a dark room saturates a
    // camera that has auto-exposed for the dark, and every module runs together
    // into one white rectangle. With no information, sweep: walk down from full
    // to the floor, and if the floor changes nothing, jump back to full and walk
    // down again. One of those levels is the one that works, and trying them is
    // the only way to find out which.
    //
    // An earlier version of this comment asserted that being seen is never
    // evidence of too much light. That is true; what is not true, and what this
    // got wrong, is that *not* being seen is evidence of too little.
    spawn_local(async move {
        let mut settled = false;
        loop {
            gloo_timers::future::TimeoutFuture::new(CALIBRATE_INTERVAL_MS).await;
            let s = status.get_untracked();
            let linked = s.sees_peer && s.peer_sees_us;

            if linked && settled {
                continue;
            }

            // Whether to give output back, and why.
            let descend = if linked {
                match s.peer_read_quality {
                    Some(q) if q < RECOVER_BELOW => {
                        // One step too far. Take it back and stop, rather than
                        // hunting across the threshold just found.
                        step_light(
                            DESCEND_STEP,
                            dimmable.get_untracked(),
                            code_light,
                            set_code_light,
                            brightness,
                            set_brightness,
                        );
                        settled = true;
                        continue;
                    }
                    Some(q) => q >= DESCEND_ABOVE,
                    None => continue,
                }
            } else {
                settled = false;
                match s.peer_sees_anything {
                    // Looking and finding nothing: too faint, not too bright.
                    Some(false) => false,
                    // Finding a code and unable to read it, or nothing said at
                    // all. Both are answered by trying less light.
                    _ => true,
                }
            };

            if descend {
                let reached_floor = !step_light(
                    -DESCEND_STEP,
                    dimmable.get_untracked(),
                    code_light,
                    set_code_light,
                    brightness,
                    set_brightness,
                );
                // At the bottom with nothing to show for it, so start again
                // from the top: the level that works is somewhere in between
                // and the only way to find it is to pass through it.
                if reached_floor && !linked {
                    set_code_light.set(1.0);
                    if dimmable.get_untracked() {
                        set_brightness.set(1.0);
                        apply_brightness(1.0);
                    }
                }
            } else {
                let _ = step_light(
                    DESCEND_STEP,
                    dimmable.get_untracked(),
                    code_light,
                    set_code_light,
                    brightness,
                    set_brightness,
                );
            }
        }
    });

    // The window opens fullscreen every time, so there has to be a way out that
    // is not killing the process.
    let _ = window_event_listener(ev::keydown, move |e| match e.key().as_str() {
        "F11" => {
            e.prevent_default();
            spawn_local(async {
                invoke("toggle_fullscreen", JsValue::UNDEFINED).await;
            });
        }
        "Escape" => {
            spawn_local(async {
                invoke("leave_fullscreen", JsValue::UNDEFINED).await;
            });
        }
        _ => {}
    });

    let open_camera = move || {
        if camera_on.get_untracked() {
            return;
        }
        set_camera_on.set(true);
        set_message.set("opening camera...".into());

        spawn_local(async move {
            let wanted = camera_id.get_untracked();
            let stream = match request_stream(wanted.as_deref()).await {
                Ok(s) => Some(s),
                Err(first) => {
                    if wanted.is_some() {
                        // The remembered camera would not open — unplugged, in
                        // use, or named by an id that has since gone stale.
                        // Fall back for this session, but keep the preference:
                        // erasing it here is what made the setting look like it
                        // never saved at all, because one bad start was enough
                        // to lose it permanently. The label is still on file and
                        // the next start resolves it again.
                        set_camera_id.set(None);
                        request_stream(None).await.ok()
                    } else {
                        set_message.set(format!("camera unavailable: {first:?}"));
                        None
                    }
                }
            };

            let Some(stream) = stream else {
                set_camera_on.set(false);
                set_streaming.set(false);
                if message.get_untracked().is_empty() {
                    set_message.set("camera unavailable".into());
                }
                return;
            };

            let Some(video) = video_ref.get_untracked() else {
                set_message.set("the video element is missing".into());
                set_camera_on.set(false);
                return;
            };
            // A quarter of the way up the range: the subject is a lit screen,
            // and there is no shortage of light coming from it.
            set_exposure.set(if restrain_exposure(&stream).await {
                0.25
            } else {
                -1.0
            });
            video.set_src_object(Some(&stream));
            let _ = video.play();
            set_streaming.set(true);
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

            // The region currently being tracked, in video pixels. `None` means
            // searching the whole frame.
            let mut roi: Option<(f64, f64, f64, f64)> = None;
            let mut misses = 0u32;

            while camera_on.get_untracked() {
                let (Some(canvas), Some(video)) =
                    (canvas_ref.get_untracked(), video_ref.get_untracked())
                else {
                    break;
                };

                // Whatever the camera actually gave us, which is not
                // necessarily what was asked for.
                let vw = f64::from(video.video_width());
                let vh = f64::from(video.video_height());
                if vw < 1.0 || vh < 1.0 {
                    gloo_timers::future::TimeoutFuture::new(SCAN_INTERVAL_MS).await;
                    continue;
                }

                let (sx, sy, sw, sh) = roi.unwrap_or((0.0, 0.0, vw, vh));

                // Downscale only as far as the budget demands, never past 1:1.
                // Scanning a region larger than it was captured invents no
                // detail and costs real time.
                let budget = if roi.is_some() {
                    TRACK_BUDGET_PX
                } else {
                    SEARCH_BUDGET_PX
                };
                let scale = (budget / (sw * sh)).sqrt().min(1.0);
                let dw = (sw * scale).round().max(1.0);
                let dh = (sh * scale).round().max(1.0);

                canvas.set_width(dw as u32);
                canvas.set_height(dh as u32);

                // `willReadFrequently` matters here more than it usually does.
                // This loop does nothing but read the canvas back out again,
                // and without the hint the browser keeps the surface on the GPU
                // and pays a stall on every `getImageData`.
                let opts = Object::new();
                let _ = Reflect::set(&opts, &"willReadFrequently".into(), &JsValue::TRUE);
                let ctx = canvas
                    .get_context_with_context_options("2d", &opts)
                    .ok()
                    .flatten()
                    .and_then(|c| c.dyn_into::<web_sys::CanvasRenderingContext2d>().ok());
                let Some(ctx) = ctx else {
                    set_message.set("no 2d canvas context".into());
                    break;
                };

                let t_capture = now_ms();
                if ctx
                    .draw_image_with_html_video_element_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
                        &video, sx, sy, sw, sh, 0.0, 0.0, dw, dh,
                    )
                    .is_err()
                {
                    gloo_timers::future::TimeoutFuture::new(SCAN_INTERVAL_MS).await;
                    continue;
                }
                let Ok(image_data) = ctx.get_image_data(0.0, 0.0, dw, dh) else {
                    gloo_timers::future::TimeoutFuture::new(SCAN_INTERVAL_MS).await;
                    continue;
                };
                let rgba = image_data.data();
                let grey = rgba_to_grey(&rgba, dw as u32, dh as u32);
                r_capture.push(now_ms() - t_capture);

                // How much of this frame the sensor could not tell apart.
                //
                // Sampled rather than counted in full: a sixty-fourth of the
                // pixels answers the question to far better precision than the
                // decision needs, and the decision is only made every couple of
                // seconds.
                {
                    let mut clipped = 0u32;
                    let mut total = 0u32;
                    let mut sum = 0u32;
                    for px in grey.iter().step_by(64) {
                        if *px >= 250 {
                            clipped += 1;
                        }
                        sum += u32::from(*px);
                        total += 1;
                    }
                    if total > 0 {
                        set_clipped.set(clipped as f32 / total as f32);
                        set_frame_mean.set(sum as f32 / total as f32);
                    }
                }

                // The decode happens here, in front of the boundary rather than
                // behind it. Android's WebView does not expose request bodies,
                // so a raw binary body cannot cross the IPC bridge at all; what
                // crosses is what the decode produced, which is under a hundred
                // bytes.
                let t_decode = now_ms();
                let scan = optical_codec::decode::scan_greyscale(dw as usize, dh as usize, &grey);
                let decode_ms = now_ms() - t_decode;

                // Aim the next frame at where this one found something. A code
                // seen once is almost certainly still nearly there, and scanning
                // only that neighbourhood is what makes a high capture
                // resolution affordable rather than ruinous.
                match scan.best_geometry() {
                    Some(g) => {
                        let back = 1.0 / scale;
                        let xs: Vec<f64> = g.corners.iter().map(|p| f64::from(p.x)).collect();
                        let ys: Vec<f64> = g.corners.iter().map(|p| f64::from(p.y)).collect();
                        let lo_x = xs.iter().copied().fold(f64::MAX, f64::min) * back + sx;
                        let hi_x = xs.iter().copied().fold(f64::MIN, f64::max) * back + sx;
                        let lo_y = ys.iter().copied().fold(f64::MAX, f64::min) * back + sy;
                        let hi_y = ys.iter().copied().fold(f64::MIN, f64::max) * back + sy;

                        let pad_x = (hi_x - lo_x) * ROI_MARGIN;
                        let pad_y = (hi_y - lo_y) * ROI_MARGIN;
                        let nx = (lo_x - pad_x).max(0.0);
                        let ny = (lo_y - pad_y).max(0.0);
                        let nw = (hi_x + pad_x).min(vw) - nx;
                        let nh = (hi_y + pad_y).min(vh) - ny;

                        if nw > 16.0 && nh > 16.0 {
                            roi = Some((nx, ny, nw, nh));
                            misses = 0;
                        }
                    }
                    None => {
                        misses += 1;
                        if misses >= ROI_PATIENCE {
                            roi = None;
                            misses = 0;
                        }
                    }
                }

                let t_transport = now_ms();
                let args = Object::new();
                let Ok(scan_js) = serde_wasm_bindgen::to_value(&scan) else {
                    gloo_timers::future::TimeoutFuture::new(SCAN_INTERVAL_MS).await;
                    continue;
                };
                let _ = Reflect::set(&args, &"scan".into(), &scan_js);
                let _ = Reflect::set(&args, &"decodeMs".into(), &JsValue::from_f64(decode_ms));
                let _ = invoke("on_scan", args.into()).await;

                // Transport is transport alone: the decode is timed on this side
                // and reported separately, so neither hides inside the other.
                r_transport.push(now_ms() - t_transport);

                tick += 1;
                // What this device is, measured rather than assumed, and sent
                // whenever it could have changed. The capture size settles once
                // the camera opens and moves again if a different one is
                // chosen; the code's pixel size moves whenever the window does.
                if tick.is_multiple_of(40) {
                    let pane = window()
                        .document()
                        .and_then(|d| d.query_selector(".qr-pane").ok().flatten())
                        .map_or(0, |el| el.client_width().max(0) as u32);
                    let args = Object::new();
                    let _ = Reflect::set(&args, &"cameraW".into(), &JsValue::from(vw as u32));
                    let _ = Reflect::set(&args, &"cameraH".into(), &JsValue::from(vh as u32));
                    let _ = Reflect::set(&args, &"displayW".into(), &JsValue::from(pane));
                    let _ = Reflect::set(&args, &"displayH".into(), &JsValue::from(pane));
                    let _ = invoke("set_capabilities", args.into()).await;
                }
                if tick.is_multiple_of(5) {
                    set_capture_ms.set(r_capture.mean());
                    set_transport_ms.set(r_transport.mean());
                    set_scan_area.set(format!(
                        "{}x{}{}",
                        dw as u32,
                        dh as u32,
                        if roi.is_some() { " tracked" } else { "" }
                    ));
                }

                gloo_timers::future::TimeoutFuture::new(SCAN_INTERVAL_MS).await;
            }
        });
    };

    let close_camera = move || {
        set_camera_on.set(false);
        set_streaming.set(false);
        if let Some(video) = video_ref.get_untracked() {
            release_stream(&video);
        }
    };

    let start_camera = move |_| open_camera();
    let stop_camera = move |_| close_camera();

    // Restore the camera, then open it, in that order and in one task.
    //
    // The order is the point. `open_camera` reads the remembered device, so
    // splitting these across two tasks would race: whichever resolved first
    // would win, and the app would open the right camera or the default one
    // depending on how quickly the device list came back.
    //
    // It opens by itself because there is no version of using this application
    // where the camera should stay off — without it there is no link, only half
    // a link that cannot hear the answer. The button remains, as a way to
    // restart the camera rather than as a step to remember.
    spawn_local(async move {
        let found = enumerate_cameras().await;

        // Resolve the remembered camera against what is actually attached, by
        // label first. Once permission has been granted the labels come back
        // real, and matching on one recovers a device whose id has changed
        // underneath us — which is the ordinary case, not the exotic one.
        let wanted = recall(CAMERA_LABEL_KEY)
            .and_then(|label| {
                found
                    .iter()
                    .find(|(_, l)| *l == label)
                    .map(|(id, _)| id.clone())
            })
            .or_else(|| {
                // No label match. Fall back to the saved id, but only if the
                // list actually contains it: asking for one that is gone throws
                // instead of degrading.
                let saved = recall(CAMERA_KEY)?;
                let (id, label) = found.iter().find(|(id, _)| *id == saved)?;
                // Heal a preference written before labels were stored, so it
                // survives the next time this id is re-salted instead of
                // silently reverting to the default one more time.
                remember(CAMERA_LABEL_KEY, label);
                Some(id.clone())
            });

        set_cameras.set(found);
        if wanted.is_some() {
            set_camera_id.set(wanted);
        }
        open_camera();
    });

    let pick_camera = move |ev| {
        let chosen = event_target_value(&ev);
        if chosen.is_empty() {
            forget(CAMERA_KEY);
            forget(CAMERA_LABEL_KEY);
            set_camera_id.set(None);
        } else {
            remember(CAMERA_KEY, &chosen);
            if let Some((_, label)) = cameras.get_untracked().iter().find(|(id, _)| *id == chosen) {
                remember(CAMERA_LABEL_KEY, label);
            }
            set_camera_id.set(Some(chosen));
        }

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
        // The page itself never scrolls. On this application the code is the
        // transmitter, and a transmitter that can be scrolled off screen is one
        // that stops transmitting because somebody brushed the trackpad.
        <main class="h-screen overflow-hidden bg-ground text-ink antialiased">
            // Padded past the system bars. The Android activity draws edge
            // to edge, so without this the status bar sits on top of the
            // header rather than beside it.
            <div
                class="mx-auto flex h-full max-w-[110rem] flex-col gap-2 p-2 sm:gap-3 sm:p-4 \
                       pt-[max(0.5rem,env(safe-area-inset-top))] \
                       pb-[max(0.5rem,env(safe-area-inset-bottom))] \
                       pl-[max(0.5rem,env(safe-area-inset-left))] \
                       pr-[max(0.5rem,env(safe-area-inset-right))]"
            >

                <header class="flex flex-none flex-wrap items-baseline gap-x-3 gap-y-1">
                    <span class="h-2.5 w-2.5 shrink-0 self-center rounded-full bg-gold"></span>
                    <h1 class="text-sm font-semibold tracking-wide sm:text-base">"Lightgap"</h1>
                    <span
                        class="rounded-full border border-line px-2 py-0.5 text-xs tabular-nums \
                               text-dim transition-colors"
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

                    // The one line worth reading while holding two devices up
                    // at each other, so it sits on the top line rather than
                    // below the fold.
                    <p
                        class="text-xs text-dim transition-colors sm:text-sm"
                        class=("text-verified", move || status.get().peer_found)
                    >
                        {move || status.get().advice}
                    </p>

                    // Pushed to the right: the clock, the charge and the way
                    // out of fullscreen. None of it is about the link, and all
                    // of it is what the system furniture would have shown if
                    // this window were not covering it.
                    <div
                        class="ml-auto flex items-baseline gap-x-3 text-xs \
                               tabular-nums text-dim/70"
                    >
                        <Show when=move || charge.get().is_some()>
                            {move || {
                                let (level, charging) = charge.get().unwrap_or((0.0, false));
                                let mark = if charging { "⚡" } else { "" };
                                view! {
                                    <span
                                        class="text-dim"
                                        class=(
                                            "text-misread",
                                            move || !charging && level < 0.15,
                                        )
                                    >
                                        {format!("{mark}{:.0}%", level * 100.0)}
                                    </span>
                                }
                            }}
                        </Show>
                        <span class="hidden text-dim sm:inline">
                            {move || clock.get().1}
                        </span>
                        <span class="text-sm font-medium text-ink">
                            {move || clock.get().0}
                        </span>
                    </div>
                </header>

                // Portrait stacks with the code on top; landscape puts the code
                // beside everything else. The split is by orientation and not by
                // width because what actually constrains the code is the shorter
                // side of the screen, whatever the device calls itself.
                <section
                    class="grid min-h-0 flex-1 gap-2 sm:gap-3 \
                           portrait:grid-rows-[minmax(0,1fr)_auto] \
                           landscape:grid-cols-[minmax(0,1fr)_minmax(17rem,21rem)]"
                >

                    <div class="flex min-h-0 flex-col items-center justify-center gap-2">
                        // Sized against the viewport height, not just the column
                        // width. Payload per frame rises with how many camera
                        // pixels land on each module, so leaving the code small
                        // while the screen has room to spare costs throughput
                        // directly — which is why nothing here caps the layout
                        // at a comfortable reading width.
                        // Both directions at once, above the code and matched
                        // to its width.
                        //
                        // They do not mean the same thing and are not labelled
                        // as though they do. Down is measured: those bytes
                        // arrived and decoded. Up is only offered — putting a
                        // code on screen says nothing about whether anyone read
                        // it, and calling that "sent" would be the interface
                        // telling a story the link has not confirmed.
                        <div
                            class="flex w-[min(100%,46vh)] items-baseline justify-between \
                                   gap-3 text-xs tabular-nums landscape:w-[min(100%,88vh)]"
                        >
                            <span class="text-dim">
                                "↑ "
                                {move || rate(status.get().offered_bps)}
                                <span class="text-dim/60">" offered"</span>
                            </span>
                            <span
                                class="transition-colors"
                                class=(
                                    "text-verified",
                                    move || status.get().delivered_bps > 0.0,
                                )
                                class=("text-dim", move || status.get().delivered_bps <= 0.0)
                            >
                                <span class="text-dim/60">"received "</span>
                                {move || rate(status.get().delivered_bps)}
                                " ↓"
                            </span>
                        </div>

                        // Three layers, in this order: the code, then a mask
                        // in front of the whole of it.
                        //
                        // The mask is a separate element rather than a change to
                        // the pane's own colour because it has to sit over the
                        // modules as well as the field they are drawn on — the
                        // code is handed in as markup through `inner_html`,
                        // which replaces whatever children this element has, so
                        // the mask cannot live inside it.
                        <div
                            // Sized against the shorter side of the screen, and as close to
                            // all of it as the rest of the layout allows.
                            //
                            // The old cap left the code using well under half a 1600-pixel
                            // tall panel while the header and the two caption rows needed
                            // about 7vh between them. Those spare pixels are not decoration:
                            // the code is the thing the peer has to resolve, and every one of
                            // them puts its modules on more of that camera's sensor.
                            class="relative aspect-square w-[min(100%,52vh)] shrink-0 \
                                   landscape:w-[min(100%,88vh)]"
                        >
                            <div
                                // No padding. The encoded image already carries
                                // its own quiet zone — four modules a side, with
                                // the white to draw them — so a margin here was
                                // a second one stacked on the first, and every
                                // pixel of it was area the code could not use.
                                // On this link that is not tidiness: the code
                                // shrinks, its modules land on fewer of the
                                // peer's sensor pixels, and the read rate falls
                                // for no reason at all.
                                class="qr-pane absolute inset-0 rounded-xl bg-white \
                                       [&>svg]:block [&>svg]:h-full [&>svg]:w-full"
                                inner_html=move || qr.get()
                            ></div>

                            // Never intercepts a click: it covers the one thing
                            // on screen that must not stop being a display.
                            <div
                                class="pointer-events-none absolute inset-0 rounded-xl"
                                style=move || {
                                    format!(
                                        "background-color: rgba(0, 0, 0, {:.2})",
                                        1.0 - code_light.get(),
                                    )
                                }
                            ></div>
                        </div>

                        <Show when=move || status.get().sas.is_some()>
                            <div
                                class="w-full max-w-md rounded-xl border border-gold/50 \
                                       bg-gold/10 px-3 py-2 text-center"
                            >
                                <p class="text-[0.7rem] tracking-wide text-dim">
                                    "Compare these digits on both screens"
                                </p>
                                <p
                                    class="font-mono text-xl font-semibold tracking-[0.3em] \
                                           tabular-nums text-gold sm:text-2xl"
                                >
                                    {move || spaced(&status.get().sas.unwrap_or_default())}
                                </p>
                                <p class="text-[0.7rem] text-dim">
                                    "If they differ, something is between you."
                                </p>
                            </div>
                        </Show>

                        <Show when=move || status.get().pairing_expires_in.is_some()>
                            <p class="text-center text-xs text-dim">
                                "Pairing code · new key in "
                                {move || {
                                    format!("{} s", status.get().pairing_expires_in.unwrap_or(0))
                                }}
                            </p>
                        </Show>
                    </div>

                    // Everything that is not the transmitter. Scrolls on its
                    // own so that running out of room here can never shrink the
                    // code.
                    <aside
                        class="flex min-h-0 flex-col gap-2 overflow-y-auto \
                               portrait:max-h-[42vh]"
                    >
                        <div
                            class="relative aspect-video w-full shrink-0 overflow-hidden \
                                   rounded-xl border border-line bg-black"
                        >
                            // Hidden rather than merely covered while off: an
                            // empty video element paints its own oversized play
                            // control, which reads as a button to press and sits
                            // on top of the text explaining what to do instead.
                            <video
                                class="h-full w-full object-cover"
                                class=("hidden", move || !streaming.get())
                                node_ref=video_ref
                                autoplay=true
                                muted=true
                                playsinline=true
                            ></video>
                            // While the stream is still being negotiated, say so.
                            // `camera_on` turns true the moment opening starts, which
                            // is not the same as having a picture yet.
                            <Show when=move || !streaming.get()>
                                <div class="absolute inset-0 flex items-center justify-center p-3 text-center text-xs text-dim sm:text-sm">
                                    {move || {
                                        if camera_on.get() {
                                            "Opening the camera…"
                                        } else {
                                            "Point this camera at the other screen"
                                        }
                                    }}
                                </div>
                            </Show>

                            // Once both ends can see each other the preview has done its
                            // job — aiming is over — and what matters is that the link is
                            // up.
                            //
                            // Laid over the video rather than replacing it. The scan loop
                            // draws its frames from that element, and a preview that is
                            // merely covered keeps feeding it while one that is removed
                            // stops the link. It also means the camera returns the instant
                            // either check drops, which is exactly when someone needs to
                            // see what they are pointing at.
                            <Show when=move || {
                                let s = status.get();
                                s.sees_peer && s.peer_sees_us
                            }>
                                <div class="absolute inset-0 flex flex-col items-center justify-center gap-1 bg-panel">
                                    <svg
                                        class="h-10 w-10 text-verified"
                                        viewBox="0 0 24 24"
                                        fill="none"
                                        stroke="currentColor"
                                        stroke-width="1.8"
                                        stroke-linecap="round"
                                    >
                                        <rect x="4" y="10.5" width="16" height="10" rx="2.5"></rect>
                                        <path d="M8 10.5V7a4 4 0 0 1 8 0v3.5"></path>
                                        <circle cx="12" cy="15.5" r="1.2" fill="currentColor"></circle>
                                    </svg>
                                    <p class="text-sm font-medium text-verified">"Linked"</p>
                                    <p class="text-[0.7rem] text-dim">"Both ends are reading each other"</p>
                                </div>
                            </Show>
                        </div>

                        // Sits directly under the preview because this is
                        // what you read while moving the devices, not after.
                        <div
                            class="flex items-center justify-between gap-3 rounded-xl border \
                                   border-line bg-panel px-3 py-2 text-xs"
                            class=(
                                "border-verified/50",
                                move || {
                                    let s = status.get();
                                    s.sees_peer && s.peer_sees_us
                                },
                            )
                        >
                            <LinkCheck
                                label="You see them"
                                ok=Signal::derive(move || status.get().sees_peer)
                            />
                            <LinkCheck
                                label="They see you"
                                ok=Signal::derive(move || status.get().peer_sees_us)
                            />
                        </div>

                        <div class="flex flex-wrap gap-2">
                            <button
                                class=BTN_PRIMARY
                                on:click=start_camera
                                disabled=move || camera_on.get()
                            >
                                "Start camera"
                            </button>
                            <button
                                class=BTN
                                on:click=stop_camera
                                disabled=move || !camera_on.get()
                            >
                                "Stop"
                            </button>
                            <select
                                class="min-h-10 min-w-0 flex-1 cursor-pointer rounded-lg border \
                                       border-line bg-panel px-2.5 text-sm text-ink \
                                       transition-colors hover:border-beam"
                                on:change=pick_camera
                                // Reads `cameras` as well as the value itself,
                                // and not by accident. A select ignores a value
                                // that names no option it has yet, and the
                                // options arrive from an async enumeration —
                                // so without a dependency on the list this runs
                                // once against an empty select, silently does
                                // nothing, and never runs again. The remembered
                                // camera was being used all along and just
                                // never shown.
                                prop:value=move || {
                                    let _ = cameras.get();
                                    camera_id.get().unwrap_or_default()
                                }
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
                        </div>

                        // Two light controls, and they are not the same lever.
                        //
                        // This one drives a mask laid over the whole code. It
                        // works on every display and touches nothing outside the
                        // window, and it is aimed squarely at blooming, because
                        // the light half of the image is what a close-up sensor
                        // clips on. It costs contrast, so it is a fix for a
                        // specific failure rather than a dial to leave low.
                        //
                        // The one below moves the panel backlight, which lowers
                        // white and black together and so keeps the ratio. That
                        // is the better lever where it exists — but on desktop
                        // it exists only sometimes: this machine can set its
                        // built-in panel and cannot touch the external monitor
                        // beside it, so offering it there would be a control
                        // that works or not depending on which screen the window
                        // happens to be on.
                        <div class="rounded-xl border border-line bg-panel px-3 py-2">
                            <div class="flex items-baseline justify-between text-xs">
                                <span class="text-dim">"Code brightness"</span>
                                <span
                                    class="tabular-nums text-ink"
                                    class=(
                                        "text-misread",
                                        move || code_light.get() < CODE_LIGHT_SAFE,
                                    )
                                >
                                    {move || format!("{:.0}%", code_light.get() * 100.0)}
                                </span>
                            </div>
                            <input
                                class="mt-1 w-full accent-gold"
                                type="range"
                                min=CODE_LIGHT_FLOOR.to_string()
                                max="1"
                                step="0.01"
                                prop:value=move || code_light.get().to_string()
                                on:input=on_code_light
                            />
                            <p
                                class="mt-1 text-[0.7rem] text-dim/70"
                                class=(
                                    "text-verified",
                                    move || {
                                        let s = status.get();
                                        s.sees_peer && s.peer_sees_us
                                    },
                                )
                            >
                                {move || {
                                    let s = status.get();
                                    match s.peer_read_quality {
                                        Some(q) if s.sees_peer && s.peer_sees_us => {
                                            format!("Peer reads this screen at {:.0}%", q * 100.0)
                                        }
                                        _ if code_light.get() < CODE_LIGHT_SAFE => {
                                            "Below what usually decodes — watch the read rate"
                                                .to_owned()
                                        }
                                        _ => "Full while searching · settles once linked"
                                            .to_owned(),
                                    }
                                }}
                            </p>
                        </div>

                        <Show when=move || dimmable.get()>
                            <div class="rounded-xl border border-line bg-panel px-3 py-2">
                                <div class="flex items-baseline justify-between text-xs">
                                    <span class="text-dim">"Screen backlight"</span>
                                    <span class="tabular-nums text-ink">
                                        {move || format!("{:.0}%", brightness.get() * 100.0)}
                                    </span>
                                </div>
                                <input
                                    class="mt-1 w-full accent-gold"
                                    type="range"
                                    min="0.05"
                                    max="1"
                                    step="0.05"
                                    prop:value=move || brightness.get().to_string()
                                    on:input=on_brightness
                                />
                            </div>
                        </Show>

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
                            <p class="rounded border-l-2 border-beam bg-panel px-3 py-2 text-xs">
                                {move || message.get()}
                            </p>
                        </Show>

                        <Show when=move || status.get().sending.is_some()>
                            <div class="flex items-center gap-2 text-xs">
                                <span class="min-w-0 flex-1 truncate text-dim">
                                    "Sending " {move || status.get().sending.unwrap_or_default()}
                                </span>
                                <progress
                                    class="h-2 w-20 accent-gold"
                                    max="1"
                                    value=move || status.get().send_progress
                                ></progress>
                                <span class="w-9 text-right tabular-nums text-dim">
                                    {move || percent(status.get().send_progress)}
                                </span>
                            </div>
                        </Show>

                        <Show when=move || status.get().receiving.is_some()>
                            <div class="flex items-center gap-2 text-xs">
                                <span class="min-w-0 flex-1 truncate text-dim">
                                    "Receiving " {move || status.get().receiving.unwrap_or_default()}
                                </span>
                                <progress
                                    class="h-2 w-20 accent-beam"
                                    max="1"
                                    value=move || status.get().receive_progress
                                ></progress>
                                <span class="w-9 text-right tabular-nums text-dim">
                                    {move || percent(status.get().receive_progress)}
                                </span>
                            </div>
                        </Show>

                        <Show when=move || status.get().received_name.is_some()>
                            <div
                                class="rounded border-l-2 border-verified bg-panel px-3 py-2 \
                                       text-xs"
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

                        // Two columns where the panel is wide and one where it
                        // is a narrow side rail. Always visible either way:
                        // these numbers are how anyone tells a link aimed wrong
                        // from one aimed right and too dense, and that is read
                        // while moving the devices, not afterwards.
                        <div
                            class="grid grid-cols-2 gap-x-4 rounded-xl border border-line \
                                   bg-panel px-3 py-2 text-xs landscape:grid-cols-1 \
                                   landscape:text-sm"
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
                                label="Scanning"
                                value=Signal::derive(move || scan_area.get())
                            />
                            <Metric
                                label="Clipped"
                                value=Signal::derive(move || percent(clipped.get()))
                            />
                            <Metric
                                label="Exposure"
                                value=Signal::derive(move || {
                                    let e = exposure.get();
                                    if e < 0.0 {
                                        "camera has none".to_owned()
                                    } else {
                                        format!("{:.0}%", e * 100.0)
                                    }
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
                                label="Codes shown"
                                value=Signal::derive(move || {
                                    format!("{}", status.get().metrics.frames_displayed)
                                })
                            />
                        </div>

                        <details
                            class="shrink-0 rounded-xl border border-line bg-panel px-3 py-2 \
                                   text-xs"
                        >
                            <summary class="cursor-pointer text-dim">"History"</summary>
                            <ul class="mt-2 max-h-40 list-disc overflow-y-auto pl-5">
                                {move || {
                                    status
                                        .get()
                                        .log
                                        .into_iter()
                                        .rev()
                                        .map(|line| view! { <li class="py-0.5 text-dim">{line}</li> })
                                        .collect_view()
                                }}
                            </ul>
                        </details>
                    </aside>
                </section>

                <canvas node_ref=canvas_ref class="hidden"></canvas>
            </div>
        </main>
    }
}
