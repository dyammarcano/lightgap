//! The application's state: a `Modem`, a clock, and the display pacing.
//!
//! Everything that decides *what* to transmit lives in the `modem` crate and is
//! tested without hardware. What lives here is the part that genuinely needs a
//! running application — when to change the code on screen, what the camera just
//! saw, and how the link is performing.

use std::time::{Duration, Instant};

use link_calibration::adaptive::Aimd;
use modem::{Event, Modem};
use optical_codec::decode::FrameScan;
// Only the test-only end-to-end path decodes here now; the application decodes
// in front of the IPC boundary instead.
#[cfg(test)]
use optical_codec::decode::scan_greyscale;
use optical_codec::device::{suggest_profile, FormFactor, VisualCapabilities};
use optical_codec::encode::{encode, max_payload, Ecc};
use optical_codec::geometry::{advise, Advice, QrGeometry};
use optical_protocol::session::PeerId;

/// How long one code stays on screen, in milliseconds.
///
/// This is the single most consequential number in the application, and it is a
/// balance rather than a maximum. Change the code too fast and the peer's camera
/// never gets a clean exposure of any one of them; change it too slowly and the
/// link runs below what the hardware could do.
///
/// 120 ms means a 30 fps camera gets roughly three chances at every code. Fewer
/// than two and a single blurred frame loses the code entirely.
pub const DEFAULT_HOLD_MS: u64 = 80;

/// How long the pairing code stays on screen, in milliseconds.
///
/// Two hundred and fifty times the data hold, and deliberately so. A code that
/// changes every 120 ms can only be read by another instance of this
/// application running a capture loop; nothing else — no phone, no photograph,
/// no person — gets a chance at it. The pairing code is the one frame a human
/// may need to point something else at, so it stands still for as long as its
/// ephemeral key is valid, and the session rotates both together.
pub const PAIRING_HOLD_MS: u64 = 30_000;

/// Error correction for displayed codes.
///
/// Q rather than L. The extra redundancy costs payload, but this medium's
/// failures are physical — a hand moves, a reflection lands, the autofocus hunts
/// — and those damage regions of a code rather than scattering single bits.
/// Region damage is exactly what error correction is for.
pub const DISPLAY_ECC: Ecc = Ecc::Q;

/// Frame size to start with, derived rather than chosen.
///
/// A hardcoded number here would silently disagree with the measured
/// pixels-per-module threshold the moment either changed, and the failure mode
/// is nasty: the application starts by displaying codes its peer cannot read, so
/// calibration never gets a single successful frame to work from and the link
/// never starts at all.
///
/// So it is computed from the same profile logic the calibration layer uses,
/// against a deliberately pessimistic assumption about the peer — a 720p webcam,
/// which is the weaker of the two common cases. Starting low and letting
/// calibration climb costs a little throughput for a few seconds. Starting high
/// costs the session.
///
/// The profile is asked for [`DISPLAY_ECC`] specifically rather than for
/// whichever level carries the most. Asking for the best would return a capacity
/// computed at low correction while the engine draws at high, and the code
/// actually displayed would be denser than the profile assumed — which is how
/// this was wrong the first time.
#[must_use]
pub fn starting_mtu() -> usize {
    let assumed = VisualCapabilities::typical(FormFactor::Laptop);
    suggest_profile(&assumed, &assumed, DISPLAY_ECC)
        .map_or(MINIMUM_MTU, |p| p.payload_bytes.max(MINIMUM_MTU))
}

/// Floor for the frame size.
///
/// Below this a frame cannot carry the transfer metadata, and a link that cannot
/// announce a file cannot transfer one. If the profile logic ever suggests less
/// than this, the honest answer is that the link is unusable rather than that it
/// is very slow.
pub const MINIMUM_MTU: usize = 64;

/// How much the frame grows on each step up.
///
/// Additive on the way up and multiplicative on the way down, which is the
/// asymmetry every congestion control shares and for the same reason: the cost
/// of overshooting is losing the link, and the cost of undershooting is only
/// being slower for a few seconds.
const MTU_STEP: u32 = 32;

/// Window the throughput figures are measured over.
///
/// Short enough to react while someone is still adjusting the aim, long enough
/// that a single dropped frame does not read as the link collapsing. A lifetime
/// average would do neither: it would keep reporting a rate the link had while
/// it was working, long after it stopped.
const RATE_WINDOW: Duration = Duration::from_secs(2);

/// What the interface needs to draw itself.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Status {
    pub session_state: String,
    pub role: Option<String>,
    pub peer_found: bool,
    /// Whether this end can see the peer's code.
    pub sees_peer: bool,
    /// Whether the peer has proved it can see this end's code.
    ///
    /// Separate from the above because the link fails one direction at a time,
    /// and the two answers send you to opposite ends of the table.
    pub peer_sees_us: bool,
    pub sending: Option<String>,
    pub send_progress: f32,
    pub receiving: Option<String>,
    pub receive_progress: f32,
    pub received_name: Option<String>,
    pub received_len: Option<usize>,
    /// Guidance for whoever is holding the devices.
    pub advice: String,
    /// Pixels per module the camera is currently resolving.
    pub pixels_per_module: f32,
    pub payload_per_frame: usize,
    /// Payload bytes per second this end is putting on screen.
    ///
    /// Offered, not confirmed: showing a code says nothing about whether the
    /// peer read it. The honest counterpart is `delivered_bps`, which counts
    /// only what this end actually decoded.
    pub offered_bps: f32,
    /// Payload bytes per second arriving and decoding successfully.
    pub delivered_bps: f32,
    /// How well the peer says it is reading this end, once it has measured.
    ///
    /// The figure that should size and dim this end's transmitter — not our own
    /// read rate, which measures the opposite direction.
    pub peer_read_quality: Option<f32>,
    /// The digits both users compare, once key agreement has completed.
    pub sas: Option<String>,
    /// Seconds until the pairing code on screen is replaced. `None` once a peer
    /// has been found, because rotation stops there.
    pub pairing_expires_in: Option<u64>,
    pub metrics: Metrics,
    pub log: Vec<String>,
}

/// How the link is actually performing.
///
/// These are the numbers the Phase 0 spike existed to produce. Collecting them
/// in the real application instead means they describe the real workload rather
/// than a benchmark, and they keep describing it as conditions change.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct Metrics {
    /// Camera frames handed to the decoder.
    pub frames_captured: u64,
    /// Frames a code was found in.
    pub frames_with_code: u64,
    /// Frames that yielded a valid protocol packet.
    pub frames_decoded: u64,
    /// Mean time spent decoding one frame, in milliseconds. Backend only,
    /// excluding transport.
    pub decode_ms: f32,
    /// Codes shown on screen.
    pub frames_displayed: u64,
    /// Fraction of displayed codes the peer appears to be reading.
    pub decode_rate: f32,
}

/// What one camera frame produced.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FrameOutcome {
    pub found_code: bool,
    pub decoded: bool,
    pub advice: String,
    pub pixels_per_module: f32,
    pub sharpness: f32,
    /// Backend decode time in milliseconds, so the caller can separate it from
    /// capture and transport cost.
    pub decode_ms: f32,
    /// Corners of the detected code, for drawing an overlay.
    pub corners: Option<[[f32; 2]; 4]>,
}

pub struct Engine {
    modem: Modem,
    started: Instant,
    /// The frame currently on screen, and when it went up.
    current: Option<(Vec<u8>, Instant)>,
    current_svg: String,
    hold_ms: u64,
    metrics: Metrics,
    decode_ms_total: f64,
    /// Start of the current throughput window, and the counters as they stood
    /// at that moment.
    rate_since: Instant,
    displayed_at_mark: u64,
    decoded_at_mark: u64,
    offered_bps: f32,
    delivered_bps: f32,
    rate_ticks: u32,
    /// Frame-size control, driven by what the *peer* says it is reading.
    mtu: Aimd,
    with_code_at_mark: u64,
    last_geometry: Option<QrGeometry>,
    last_advice: Advice,
    received: Option<(String, Vec<u8>)>,
    log: Vec<String>,
}

/// How many log lines to keep.
///
/// Bounded because a transfer runs for tens of thousands of frames and this is
/// serialized to the interface on every status poll.
const LOG_CAPACITY: usize = 40;

impl Engine {
    #[must_use]
    pub fn new() -> Self {
        let mut seed = [0u8; 16];
        getrandom::fill(&mut seed).expect("the OS must provide entropy");

        Self {
            modem: Modem::new(PeerId::from_bytes(seed), starting_mtu()),
            started: Instant::now(),
            current: None,
            current_svg: String::new(),
            hold_ms: DEFAULT_HOLD_MS,
            metrics: Metrics::default(),
            decode_ms_total: 0.0,
            rate_since: Instant::now(),
            displayed_at_mark: 0,
            decoded_at_mark: 0,
            offered_bps: 0.0,
            delivered_bps: 0.0,
            rate_ticks: 0,
            mtu: Aimd::new(
                starting_mtu() as u32,
                MINIMUM_MTU as u32,
                // The encoder's ceiling, not a guess at the link's. A code that
                // large is unreadable in practice and the controller will never
                // get near it — but the limit that stops it should be the
                // measured quality coming back, not a number chosen here.
                max_payload(DISPLAY_ECC) as u32,
                MTU_STEP,
            ),
            with_code_at_mark: 0,
            last_geometry: None,
            last_advice: Advice::MoveCloser,
            received: None,
            log: Vec::new(),
        }
    }

    /// Records one line for the history panel, and emits it.
    ///
    /// Both, deliberately. The panel is bounded and lives only as long as the
    /// session, which is right for someone glancing at it mid-transfer and
    /// useless afterwards; the log survives, is timestamped, and can be read on
    /// a machine that is not the one that ran it.
    fn note(&mut self, line: impl Into<String>) {
        let line = line.into();
        log::info!("{line}");
        self.log.push(line);
        if self.log.len() > LOG_CAPACITY {
            self.log.remove(0);
        }
    }

    /// Resizes what this end transmits, from what the peer reports.
    ///
    /// The peer's number, never our own. Our read rate measures their display
    /// against our camera; theirs measures our display against their camera,
    /// and those are different pieces of hardware pointed in opposite
    /// directions. Sizing our transmissions from our own reading would be
    /// tuning one direction using a measurement of the other.
    fn adapt_frame_size(&mut self) {
        let Some(quality) = self.modem.peer_read_quality() else {
            return;
        };
        // Nothing has been reported until a peer has been seen; a fresh session
        // reports zero, and acting on that would shrink the frame before the
        // link has had a chance to say anything.
        if !self.modem.sees_peer() {
            return;
        }

        let before = self.mtu.current();
        let verdict = self.mtu.observe(quality);
        let after = self.mtu.current();
        if after == before {
            return;
        }

        if self.modem.set_mtu(after as usize) {
            self.note(format!(
                "frame {before} -> {after} B ({verdict:?}, peer reads {:.0}%)",
                quality * 100.0
            ));
        } else {
            // Refused, which mid-transfer is correct: the symbol size is pinned
            // for the object being sent. Put the controller back where the
            // modem actually is so the next window does not compound a change
            // that never happened.
            self.mtu = Aimd::new(
                before,
                MINIMUM_MTU as u32,
                max_payload(DISPLAY_ECC) as u32,
                MTU_STEP,
            );
        }
    }

    /// Writes one line describing the state of the link.
    ///
    /// The message is one line in the source on purpose: `cargo fmt` collapses
    /// a `\`-continued string literal by turning the newline and its indent
    /// into real spaces, which lands in the log as a gap in the middle of a
    /// sentence.
    ///
    /// Emitted on a slow cadence rather than per frame: at eight frames a
    /// second a per-frame log is unreadable within a minute and hides the
    /// thing it was written to expose. What matters when a transfer goes wrong
    /// is the shape of the numbers over minutes — whether the read rate decayed
    /// or fell off a cliff, and whether it did so while the code was still
    /// being seen.
    fn note_link(&mut self) {
        let m = self.metrics;
        self.note(format!(
            "link: {:?} · read {:.0}% ({}/{}) · unread {} · {:.1} px/mod · {} B/frame · up {:.0} down {:.0} B/s · scan {:.0} ms",
            self.modem.state(),
            m.decode_rate * 100.0,
            m.frames_decoded,
            m.frames_captured,
            m.frames_with_code.saturating_sub(m.frames_decoded),
            self.last_geometry.map_or(0.0, |g| g.pixels_per_module),
            self.modem.payload_per_frame(),
            self.offered_bps,
            self.delivered_bps,
            m.decode_ms,
        ));
        if let Some(q) = self.modem.peer_read_quality() {
            self.note(format!("peer reports reading us at {:.0}%", q * 100.0));
        }
    }

    fn now(&self) -> std::time::Duration {
        self.started.elapsed()
    }

    /// Offers a file for transfer.
    pub fn send_file(&mut self, name: &str, bytes: Vec<u8>) {
        self.note(format!("sending {name} ({} B)", bytes.len()));
        self.modem.send_file(name, bytes);
    }

    /// The received file, if one arrived.
    #[must_use]
    pub fn take_received(&mut self) -> Option<(String, Vec<u8>)> {
        self.received.take()
    }

    #[must_use]
    pub fn received_ref(&self) -> Option<&(String, Vec<u8>)> {
        self.received.as_ref()
    }

    /// Puts a taken file back.
    ///
    /// Needed because saving can fail after the bytes have been taken, and
    /// losing a file that arrived — because the chosen destination was not
    /// writable — would throw away a transfer that took minutes over a
    /// destination the user can simply pick again.
    pub fn restore_received(&mut self, name: String, bytes: Vec<u8>) {
        self.received = Some((name, bytes));
    }

    /// The code to display, as an SVG document.
    ///
    /// Advances to the next frame only once the current one has been up for its
    /// hold time. Calling this at the display's refresh rate is fine and
    /// expected: the pacing lives here rather than in the interface, so that the
    /// timing cannot drift with how often the interface happens to poll.
    pub fn current_qr(&mut self) -> &str {
        self.update_rates();
        let now = self.now();
        for e in self.modem.tick(now) {
            self.absorb(e);
        }

        // While still looking, the code on screen is the pairing code and it
        // holds for its whole validity window. Once there is a peer the link is
        // a data stream again and the fast hold applies.
        let hold = if self.modem.rotation_due().is_some() {
            PAIRING_HOLD_MS
        } else {
            self.hold_ms
        };
        let expired = match &self.current {
            None => true,
            Some((_, since)) => since.elapsed().as_millis() as u64 >= hold,
        };
        if !expired {
            return &self.current_svg;
        }

        if let Some(frame) = self.modem.poll_frame() {
            match encode(&frame, DISPLAY_ECC) {
                Ok(modules) => {
                    self.current_svg = svg_from(&modules);
                    self.current = Some((frame, Instant::now()));
                    self.metrics.frames_displayed += 1;
                }
                Err(e) => {
                    // A frame that will not encode is a configuration fault, not
                    // a transient one: the MTU and the correction level disagree,
                    // and every subsequent frame will fail the same way.
                    self.note(format!("frame will not encode: {e}"));
                }
            }
        }

        &self.current_svg
    }

    /// Takes in one greyscale camera frame.
    /// Drops the display hold so a measurement can advance frames as fast as it
    /// can render them.
    ///
    /// The hold is real elapsed time, which is right in the application and
    /// useless in a harness: a hundred thousand frames at eighty milliseconds
    /// each is two hours of sleeping to measure something that has nothing to
    /// do with sleeping. What the harness is counting is *frames*; wall-clock
    /// throughput is that count multiplied by the hold, and the multiplication
    /// does not need to be waited through.
    #[cfg(test)]
    pub fn set_hold_for_measurement(&mut self, ms: u64) {
        self.hold_ms = ms;
    }

    /// Test-only since the decode moved in front of the IPC boundary. It is
    /// kept because it is the only path that exercises the decoder end to end
    /// against the synthetic camera, and that is the half of this worth testing:
    /// the half that has no interface to run inside.
    #[cfg(test)]
    pub fn on_camera_frame(&mut self, width: usize, height: usize, pixels: &[u8]) -> FrameOutcome {
        let t0 = Instant::now();
        let scan = scan_greyscale(width, height, pixels);
        #[allow(clippy::cast_possible_truncation)]
        let decode_ms = (t0.elapsed().as_secs_f64() * 1000.0) as f32;
        self.on_scan(&scan, decode_ms)
    }

    /// Recomputes throughput if the window has elapsed.
    ///
    /// Counted in payload bytes rather than frames, because frames are not what
    /// anyone is waiting for.
    fn update_rates(&mut self) {
        let elapsed = self.rate_since.elapsed();
        if elapsed < RATE_WINDOW {
            return;
        }

        let seconds = elapsed.as_secs_f32();
        let payload = self.modem.payload_per_frame() as f32;
        let displayed = self.metrics.frames_displayed - self.displayed_at_mark;
        let decoded = self.metrics.frames_decoded - self.decoded_at_mark;

        self.offered_bps = displayed as f32 * payload / seconds;
        self.delivered_bps = decoded as f32 * payload / seconds;

        // How well this end read what it could actually see, over this window
        // alone. Not the lifetime rate: that divides by every frame captured
        // while nothing was pointed at the camera, so it reads as a broken link
        // during setup and then lags for minutes once one is working. And not
        // frames-captured either — a frame with no code in it is not a failure
        // to read, it is nothing to read.
        let seen = self.metrics.frames_with_code - self.with_code_at_mark;
        if seen > 0 {
            let quality = decoded as f32 / seen as f32;
            self.modem.set_read_quality(quality);
        }

        self.adapt_frame_size();

        self.rate_since = Instant::now();
        self.displayed_at_mark = self.metrics.frames_displayed;
        self.decoded_at_mark = self.metrics.frames_decoded;
        self.with_code_at_mark = self.metrics.frames_with_code;

        // Every fifth window, so roughly every ten seconds.
        self.rate_ticks = self.rate_ticks.wrapping_add(1);
        if self.rate_ticks.is_multiple_of(5) {
            self.note_link();
        }
    }

    /// Takes in the result of a scan someone else performed.
    ///
    /// Split out because the decode does not happen here in the running
    /// application. Android's WebView cannot carry a raw binary body across the
    /// IPC bridge, so nine hundred kilobytes of pixels per frame cannot cross it
    /// at all; the interface decodes instead and sends what came out, which is
    /// under a hundred bytes. Everything below this line is the same wherever
    /// the decode ran.
    ///
    /// `on_camera_frame` stays, and the engine's tests still drive it: they
    /// exercise the decoder against a synthetic camera, which is the half worth
    /// testing and the half that has no interface to run inside.
    pub fn on_scan(&mut self, scan: &FrameScan, decode_ms: f32) -> FrameOutcome {
        let decode_ms = f64::from(decode_ms);

        self.metrics.frames_captured += 1;
        self.decode_ms_total += decode_ms;
        self.metrics.decode_ms =
            (self.decode_ms_total / self.metrics.frames_captured as f64) as f32;

        let geometry = scan.best_geometry();
        self.last_geometry = geometry;
        let advice = geometry.map_or(Advice::MoveCloser, |g| advise(&g, scan.sharpness));
        self.last_advice = advice;

        let mut decoded = false;
        if !scan.detections.is_empty() {
            self.metrics.frames_with_code += 1;
        }
        for detection in &scan.detections {
            let events = self.modem.handle_frame(&detection.payload);
            if !events.is_empty() {
                decoded = true;
            }
            for e in events {
                self.absorb(e);
            }
        }
        // A code that decoded but produced no event is still a successful read —
        // most data symbols produce nothing visible until the last one.
        if !scan.detections.is_empty() {
            self.metrics.frames_decoded += 1;
            decoded = true;
        }

        if self.metrics.frames_captured > 0 {
            self.metrics.decode_rate =
                self.metrics.frames_decoded as f32 / self.metrics.frames_captured as f32;
        }

        FrameOutcome {
            found_code: !scan.detections.is_empty() || !scan.failed.is_empty(),
            decoded,
            advice: advice.message().to_owned(),
            pixels_per_module: geometry.map_or(0.0, |g| g.pixels_per_module),
            sharpness: scan.sharpness,
            decode_ms: decode_ms as f32,
            corners: geometry.map(|g| g.corners.map(|p| [p.x, p.y])),
        }
    }

    fn absorb(&mut self, event: Event) {
        match event {
            Event::PeerFound { peer, role } => {
                self.note(format!("peer {peer} found, this side is the {role:?}"));
            }
            Event::IncomingFile { name, total_len } => {
                self.note(format!("incoming: {name} ({total_len} B)"));
            }
            Event::FileReceived { name, bytes } => {
                self.note(format!("received {name} ({} B), hash matched", bytes.len()));
                self.received = Some((name, bytes));
            }
            Event::FileCorrupt { name } => {
                // Worth its own message rather than a generic failure: every
                // frame passed its checksum, so this points at reassembly, and
                // saying "transfer failed" would send someone looking at the
                // camera.
                self.note(format!("{name} arrived but the hash did not match"));
            }
            Event::Paired => {
                // The digits go in the log as well as on screen: if the two
                // sides ever disagree, what matters afterwards is which value
                // each one showed and when.
                match self.modem.short_auth_string() {
                    Some(sas) => self.note(format!("paired — compare {sas} on both screens")),
                    None => self.note("paired"),
                }
            }
            Event::PairingRotated => {
                self.note("nobody answered, new pairing code");
                // The code on screen is stale the instant its key is: drop it so
                // the next poll draws the new one rather than waiting out a hold
                // that belongs to a key nobody can use any more.
                self.current = None;
            }
            Event::SendComplete => self.note("the peer has the whole file"),
            Event::PeerLost => self.note("peer lost"),
            Event::Closed => self.note("session closed"),
            Event::Progress { .. } => {}
        }
    }

    #[must_use]
    pub fn status(&self) -> Status {
        Status {
            session_state: format!("{:?}", self.modem.state()),
            role: self.modem.role().map(|r| format!("{r:?}")),
            peer_found: self.modem.role().is_some(),
            sees_peer: self.modem.sees_peer(),
            peer_sees_us: self.modem.peer_sees_us(),
            sending: self.modem.sending_file().map(ToOwned::to_owned),
            send_progress: self.modem.send_progress(),
            receiving: self.modem.receiving_file().map(ToOwned::to_owned),
            receive_progress: self.modem.receive_progress(),
            received_name: self.received.as_ref().map(|(n, _)| n.clone()),
            received_len: self.received.as_ref().map(|(_, b)| b.len()),
            advice: self.last_advice.message().to_owned(),
            pixels_per_module: self.last_geometry.map_or(0.0, |g| g.pixels_per_module),
            payload_per_frame: self.modem.payload_per_frame(),
            offered_bps: self.offered_bps,
            delivered_bps: self.delivered_bps,
            peer_read_quality: self.modem.peer_read_quality(),
            sas: self.modem.short_auth_string().map(ToOwned::to_owned),
            pairing_expires_in: self
                .modem
                .rotation_due()
                .map(|due| due.saturating_sub(self.now()).as_secs()),
            metrics: self.metrics,
            log: self.log.clone(),
        }
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders a module matrix as an SVG document.
///
/// Built by hand rather than through the `qrcode` crate's renderer so the
/// matrix stays the single source of truth: the codec produces modules, and
/// every consumer — this, the test bench, a future LED panel — draws them its
/// own way.
///
/// One `<rect>` per dark module would be tens of thousands of elements; a single
/// `<path>` with one move-and-draw per module keeps the document small enough to
/// hand across the interface boundary sixty times a second.
fn svg_from(modules: &optical_codec::encode::Modules) -> String {
    const QUIET: usize = 4;
    let size = modules.size();
    let side = size + QUIET * 2;

    let mut path = String::with_capacity(size * size * 8);
    for y in 0..size {
        for x in 0..size {
            if modules.is_dark(x, y) {
                path.push_str(&format!("M{} {}h1v1h-1z", x + QUIET, y + QUIET));
            }
        }
    }

    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {side} {side}\" \
         shape-rendering=\"crispEdges\">\
         <rect width=\"{side}\" height=\"{side}\" fill=\"#fff\"/>\
         <path d=\"{path}\" fill=\"#000\"/></svg>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use optical_codec::distort::{capture, Conditions};

    /// Photographs a rendered code the way the real camera path does, and hands
    /// the result back as a greyscale frame.
    ///
    /// The engine emits SVG for the display, so this re-encodes the same bytes
    /// rather than rasterising the SVG: what is under test is the engine's frame
    /// handling, not an SVG renderer.
    fn photograph(frame_bytes: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
        let modules = encode(frame_bytes, DISPLAY_ECC).ok()?;
        let cond = Conditions {
            fill: 0.75,
            ..Conditions::typical()
        };
        Some(capture(&modules, &cond))
    }

    /// Drains whatever the engine wants to display right now, as raw bytes.
    fn pending_frame(engine: &mut Engine) -> Option<Vec<u8>> {
        engine.current_qr();
        engine.current.as_ref().map(|(bytes, _)| bytes.clone())
    }

    #[test]
    fn a_code_is_held_long_enough_for_a_camera_to_catch_it() {
        // The commonest way to build a link that transmits nothing is to change
        // the code every display refresh. At 30 fps a camera needs the code to
        // survive at least two of its frames.
        let mut engine = Engine::new();
        let first = pending_frame(&mut engine).expect("something to show");

        // Polling again immediately must not advance it.
        let again = pending_frame(&mut engine).expect("still showing");
        assert_eq!(first, again, "the code must not change on every poll");

        // A compile-time check: the value is a constant, so a runtime assert
        // would be verifying something the compiler already knows, and clippy
        // rightly objects. The intent is to pin the constant, not to test it.
        const _: () = assert!(
            DEFAULT_HOLD_MS >= 66,
            "a hold shorter than two frames of a 30 fps camera would mean a \
             single blurred frame loses the code entirely"
        );
    }

    #[test]
    fn the_engine_shows_something_before_a_peer_exists() {
        // Discovery only works if both sides are already announcing. An engine
        // that waited for a peer before displaying anything would wait forever,
        // since the peer is waiting for the same thing.
        let mut engine = Engine::new();
        let svg = engine.current_qr().to_owned();
        assert!(svg.starts_with("<svg"), "expected an SVG document");
        assert!(svg.contains("<path"), "expected drawn modules");
    }

    #[test]
    fn a_frame_with_no_code_is_reported_as_such() {
        let mut engine = Engine::new();
        let flat = vec![200u8; 640 * 480];
        let outcome = engine.on_camera_frame(640, 480, &flat);

        assert!(!outcome.found_code);
        assert!(!outcome.decoded);
        assert_eq!(engine.status().metrics.frames_captured, 1);
        assert_eq!(engine.status().metrics.frames_decoded, 0);
    }

    #[test]
    fn an_inconsistent_frame_does_not_panic() {
        // The dimensions come across the process boundary and cannot be trusted
        // to match the buffer.
        let mut engine = Engine::new();
        let outcome = engine.on_camera_frame(1000, 1000, &[0u8; 16]);
        assert!(!outcome.found_code);
    }

    /// The application-level version of the end-to-end test: two engines find
    /// each other by photographing one another's screens.
    /// Measures a whole transfer across the synthetic camera.
    ///
    /// Ignored by default because it takes minutes: every frame is encoded,
    /// photographed through a perspective warp with supersampling, blur and
    /// noise, and decoded — twice, once in each direction. Run it deliberately:
    ///
    /// ```text
    /// cargo test -p lightgap --lib -- --ignored --nocapture measure
    /// ```
    ///
    /// What it answers is the half of throughput that optics cannot explain:
    /// how many frames a file actually costs, and whether the frame size climbs
    /// while it is being sent. Wall-clock time is that frame count times the
    /// display hold, which is arithmetic rather than something worth sleeping
    /// through.
    #[test]
    #[ignore = "measurement, minutes long: run with --ignored --nocapture"]
    fn measure_a_file_across_the_synthetic_camera() {
        const BYTES: usize = 100_000;
        const CEILING: u64 = 400_000;

        let mut a = Engine::new();
        let mut b = Engine::new();
        a.set_hold_for_measurement(0);
        b.set_hold_for_measurement(0);

        let mut pair_frames = 0u64;
        while !(a.status().peer_found && b.status().peer_found) && pair_frames < 400 {
            exchange(&mut a, &mut b);
            pair_frames += 1;
        }
        assert!(a.status().peer_found, "the two ends never paired");

        // Idle but linked, which is where calibration actually happens.
        //
        // The frame cannot be resized under a transfer — the symbol size is
        // pinned in the metadata for the whole object — so the size a file goes
        // out at is whatever was settled on beforehand. A harness that pairs and
        // sends in the same breath measures the starting guess and calls it the
        // result; the application spends this time waiting for someone to
        // choose a file.
        let paired_at = a.status().payload_per_frame;
        let idle_until = Instant::now() + Duration::from_secs(14);
        while Instant::now() < idle_until {
            exchange(&mut a, &mut b);
        }
        let settled = a.status().payload_per_frame;

        let object: Vec<u8> = (0..BYTES)
            .map(|i| (i.wrapping_mul(31) ^ 0x5A) as u8)
            .collect();
        let started = Instant::now();
        a.send_file("measurement.bin", object.clone());

        let first_frame = a.status().payload_per_frame;
        let mut frames = 0u64;
        while b.received_ref().is_none() && frames < CEILING {
            exchange(&mut a, &mut b);
            frames += 1;
        }

        let elapsed = started.elapsed();
        let last_frame = a.status().payload_per_frame;
        let received = b.received_ref().map(|(_, bytes)| bytes.len());

        println!("\n--- transfer across the synthetic camera ---");
        println!("object          {BYTES} B");
        println!("frames          {frames}");
        println!("frame at pair   {paired_at} B");
        println!("after 14 s idle {settled} B");
        println!("frame size      {first_frame} B -> {last_frame} B");
        println!("harness time    {:.1} s", elapsed.as_secs_f32());
        if frames > 0 {
            let per_frame = BYTES as f32 / frames as f32;
            println!("carried         {per_frame:.1} B per displayed frame");
            for hold in [DEFAULT_HOLD_MS, 40] {
                let seconds = frames as f32 * hold as f32 / 1000.0;
                println!(
                    "at {hold} ms hold   {:.0} B/s, {:.1} min for this file",
                    BYTES as f32 / seconds,
                    seconds / 60.0
                );
            }
        }
        println!("received        {received:?}");

        assert!(
            settled >= paired_at,
            "calibration should not shrink a link nobody has complained about:              {paired_at} B became {settled} B while idle"
        );
        assert_eq!(
            received,
            Some(BYTES),
            "the file did not arrive within {CEILING} frames"
        );
        assert!(
            b.received_ref().is_some_and(|(_, got)| got == &object),
            "what arrived was not what was sent"
        );
    }

    /// One frame each way through the synthetic camera.
    fn exchange(a: &mut Engine, b: &mut Engine) {
        if let Some(frame) = pending_frame(a) {
            if let Some((w, h, px)) = photograph(&frame) {
                b.on_camera_frame(w, h, &px);
            }
        }
        if let Some(frame) = pending_frame(b) {
            if let Some((w, h, px)) = photograph(&frame) {
                a.on_camera_frame(w, h, &px);
            }
        }
    }

    #[test]
    fn two_engines_discover_each_other_through_a_camera() {
        let mut a = Engine::new();
        let mut b = Engine::new();

        for _ in 0..30 {
            if let Some(frame) = pending_frame(&mut a) {
                if let Some((w, h, px)) = photograph(&frame) {
                    b.on_camera_frame(w, h, &px);
                }
            }
            if let Some(frame) = pending_frame(&mut b) {
                if let Some((w, h, px)) = photograph(&frame) {
                    a.on_camera_frame(w, h, &px);
                }
            }
            // Past the hold time, so the next poll advances the code.
            std::thread::sleep(std::time::Duration::from_millis(DEFAULT_HOLD_MS + 5));

            if a.status().peer_found && b.status().peer_found {
                break;
            }
        }

        assert!(a.status().peer_found, "a should have found b");
        assert!(b.status().peer_found, "b should have found a");
        assert_ne!(
            a.status().role,
            b.status().role,
            "the two sides must take different roles, or neither will drive"
        );
    }

    #[test]
    fn a_received_file_is_restored_when_saving_fails() {
        // Losing a file that arrived, because the chosen destination was not
        // writable, would throw away a transfer that took minutes.
        let mut engine = Engine::new();
        engine.received = Some(("x.bin".into(), vec![1, 2, 3]));

        let taken = engine.take_received().expect("present");
        assert!(engine.received_ref().is_none(), "taken means gone");

        engine.restore_received(taken.0, taken.1);
        assert_eq!(
            engine.received_ref().map(|(n, b)| (n.as_str(), b.len())),
            Some(("x.bin", 3))
        );
    }

    #[test]
    fn the_default_frame_size_fits_a_readable_code() {
        // The engine's MTU and the measured pixels-per-module threshold have to
        // agree, or the application starts by transmitting codes its peer cannot
        // read and calibration has no signal to work from.
        let probe = vec![0u8; starting_mtu()];
        let modules = encode(&probe, DISPLAY_ECC).expect("a full frame must encode");

        // At 720p with the code filling 75% of the height.
        let px_per_module = (720.0 * 0.75) / (modules.size() + 8) as f32;
        assert!(
            px_per_module >= optical_codec::geometry::MIN_PIXELS_PER_MODULE,
            "a full frame is {} modules, giving {px_per_module:.1} px/module at \
             720p — below the measured threshold of {}",
            modules.size(),
            optical_codec::geometry::MIN_PIXELS_PER_MODULE
        );
    }
}
