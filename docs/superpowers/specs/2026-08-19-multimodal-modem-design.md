# Air-gapped multimodal modem — design

## Context

Two machines need to exchange files with no network, no Bluetooth and no cable:
only the display and camera they already have, and optionally the speaker and
microphone. The real use case is air-gapped — moving secrets, configuration or
keys between isolated machines, where plugging in a USB stick or bringing up a
network is not acceptable.

The idea is not "a QR code with a link in it". It is **a full transport layer
over an optical medium**: the display emits packets as animated QR codes, the
camera on the other side captures them, and on top of that runs a handshake,
sequence numbers, acknowledgements, retransmission and flow control. The acoustic
channel (FSK in a near-inaudible band) joins as a second physical medium when the
hardware proves it viable, mainly for signalling and acknowledgement, because an
acoustic confirmation avoids a full optical round trip.

The central design point: **the protocol must not know which medium it travels
over**. Adding audio, LEDs or a TCP socket later should mean implementing a
trait, not editing the state machine.

Starting point: a clean Tauri 2 + Leptos (CSR, Trunk) scaffold with no commits.
Rust frontend and Rust backend. Toolchain verified (Rust 1.96, Tauri CLI 2.11.4,
Trunk 0.21.14, `wasm32-unknown-unknown` present).

## Decisions taken

| Decision | Choice |
|---|---|
| Scope | All eight subsystems, delivered in phases with functional cut points |
| Reliability | RaptorQ **and** ARQ, selectable per profile behind a trait |
| Camera | Hybrid: preview via `getUserMedia` in the WebView, decode in the backend |
| Structure | Cargo workspace with separate crates |
| Platforms | Desktop and mobile, any pairing: desktop-desktop, desktop-mobile, mobile-mobile |

Additional decisions this design introduces (they were not in the original
sketch):

- **Sans-io core.** The protocol is a pure state machine: `handle_incoming`,
  `poll_transmit`, `handle_timeout`. It opens no sockets, cameras or audio
  devices. It is the `quinn`/`rustls` pattern, and it is what lets a full
  transfer at 40% loss be tested without turning on a single camera.
- **Layered state machines.** The original sketch put `AudioNoiseMeasurement`,
  `AudioFrequencySweep` and friends in as *session* states. That couples the
  session to audio: adding a third channel would force an edit to the session
  machine. Here the session has a small machine
  (`Discovering → Peered → Negotiating → Active → Closing`) and **each channel
  carries its own independent lifecycle** (`Down → Probing → Up{profile} →
  Degraded → Down`). Calibration is a channel concern, not a session one.
- **Leader election.** Two symmetric applications have a tie-break problem: who
  starts calibration, who emits the sweep first? Resolved by comparing the
  `peer_id` values (16 random bytes) lexicographically. The lower one leads,
  sequences calibration and fixes the `session_id`.
- **Frequency-division audio, no echo cancellation.** Every microphone hears its
  own speaker. Rather than AEC (hard) or TDMA (slow), calibration already
  discovers viable bands per direction: assign **disjoint bands** (leader low,
  follower high). Acoustic full duplex without AEC.
- **Raw binary IPC.** Frames travel via `tauri::ipc::Request` with a binary body,
  not as JSON arrays. A greyscale frame passed as a JSON array is about 4 MB of
  text per frame; as raw bytes it is 900 KB. This is the decision that makes the
  chosen hybrid path viable.
- **Encryption with a derived nonce.** The ChaCha20-Poly1305 nonce is derived
  from `(session_id, direction, seq)` on both sides. It is never transmitted. On
  a channel where every byte costs, saving 12 bytes per PDU matters.

## Architecture

```
┌──────────────────────────────────────────────┐
│ File transfer                                │
├──────────────────────────────────────────────┤
│ Session  (small state machine, leader election)
├──────────────────────────────────────────────┤
│ Reliability  (trait: RaptorQ | ARQ)          │
├──────────────────────────────────────────────┤
│ Multiplexer  (priority class → channel)      │
├───────────────────────┬──────────────────────┤
│ Visual channel        │ Acoustic channel     │
│ own lifecycle         │ own lifecycle        │
├───────────────────────┼──────────────────────┤
│ display ⇄ camera      │ speaker ⇄ microphone │
└───────────────────────┴──────────────────────┘
```

### Crate layout

```
qr_comm/
├── Cargo.toml                 # [workspace]
├── crates/
│   ├── optical-protocol/      # sans-io: PDU, session FSM, reliability, traits
│   ├── optical-codec/         # QR encode/decode + geometry + synthetic camera
│   ├── acoustic-codec/        # 2-FSK mod/demod, preamble, framing. No audio I/O
│   ├── link-calibration/      # probe ladders, scoring, profiles. Pure logic
│   └── channel-sim/           # lossy channel, for testing without hardware
├── tauri-app/
│   ├── Cargo.toml             # tauri-app-ui (leptos/wasm)
│   ├── src/                   # UI: QrDisplay, CameraPreview, AlignmentOverlay
│   └── src-tauri/             # tauri-app: camera/audio drivers, commands, fs
```

### Core abstractions

PDU wire format — hand-rolled, not `bincode` (bincode is not a stable wire
format):

```rust
version: u8, session_id: u64, kind: u8, flags: u16,
seq: u32, ack: u32, payload_len: u16, payload: [u8], crc32: u32
```

A 22-byte header plus a 4-byte CRC. Over a 900-byte payload that is 2.8%.

```rust
pub trait Channel {
    fn caps(&self) -> ChannelCaps;      // mtu, direction, estimated bps, latency
    fn health(&self) -> ChannelHealth;  // per, quality, last rx
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), ChannelError>;
    fn recv_frame(&mut self) -> Option<Vec<u8>>;
}
```

Drivers (camera, audio) run in their own tasks and talk to the core over
channels. The core never blocks.

## Platforms and pairings

The protocol is symmetric and medium-agnostic, so every pairing works with the
same code: desktop-desktop, desktop-mobile, mobile-mobile. What differs between
form factors is not the protocol but the **profile**, and that is exactly what
calibration exists to discover.

| Form factor | Advantage | Constraint |
|---|---|---|
| Laptop | Large display, so large physical QR codes | Front webcam, often mediocre and fixed-focus |
| Phone/tablet | Excellent rear camera with autofocus | Small display, so less physical area for the code |

Two consequences worth stating up front:

- **A phone paired with a laptop is usually the best combination.** The laptop
  provides the large display and the phone provides the good camera, which suits
  the asymmetry of each device.
- **No device can see its own screen.** Not a laptop, not a phone. Any pairing
  needs two physical devices, and self-test requires a second monitor or a USB
  camera.

## Phases

Each phase leaves the app in a usable state. Phases 4-7 can be re-planned without
touching the core.

### Phase 0 — Spike: IPC throughput  *(throwaway code)*

Measure the hybrid path before building on it. Greyscale 1280x720 frame from
`getUserMedia` → raw binary IPC → backend → decode with `rqrr` → event back.

- **Target: 10 or more decodes per second at under 30% of one core.**
- Also measure the JSON-array path, to quantify the difference.

If it falls short, fall back in order: (a) crop to the region of interest after
first lock, (b) move decode to WASM, (c) native `nokhwa` in the backend.

Output: a number and a go/no-go. None of this code survives.

### Phase 1 — Protocol core  *(no hardware)*  ✅ done

`optical-protocol` + `channel-sim`. PDU encode/decode with CRC32, session state
machine with leader election, `Reliability` trait with both implementations,
`Channel` trait, and a simulator with configurable loss, reordering,
duplication, corruption and latency under a seeded, deterministic RNG.

**Criterion met:** 5 MB transferred at 40% loss with fountain coding and at 15%
with ARQ, entirely in simulation.

### Phase 2 — Visual channel + UI  *(codec done, UI pending)*

`optical-codec`: encoding via the `qrcode` crate returning a module matrix,
decoding via `rqrr` returning payload **and** `QrGeometry`, plus a **synthetic
distortion bench** that models perspective, blur, sensor noise, contrast and
moiré. That bench turns "hold two laptops face to face" into a test that runs in
CI.

Remaining: frontend integration — camera preview, QR display, alignment overlay
driven by `QrGeometry` events, and a loopback mode for protocol testing without
cameras.

### Phase 3 — Visual calibration  ✅ done

`link-calibration`: probe ladder (double, bisect, apply margin), per-direction
profiles, scoring by real goodput rather than raw capacity, AIMD adaptive
control, and the channel lifecycle machine.

### Phase 4 — Acoustic channel

`acoustic-codec` plus `cpal` drivers. 2-FSK, preamble correlation, Goertzel
detection, ring buffers. Frequency division with disjoint bands per direction,
assigned by the leader.

**Verification:** modulate/demodulate through synthetic AWGN, band-pass filtering
and clipping at varying SNR, with no hardware.

### Phase 5 — Acoustic calibration

Device enumeration, per-band noise floor, a **strictly alternating** sweep
coordinated over the already-up visual channel, real modulation testing (BER/PER,
not just tone detection), scoring, a viability enum, a commit/verify handshake
before enabling, and a runtime supervisor with a degradation policy.

### Phase 6 — Multiplexer

Priority classes (`Control > Metadata > Data`), a scheduler mapping class to
channel by live `ChannelHealth`, and duplication of critical messages across both
channels with deduplication by `(session, seq)`.

### Phase 7 — Encrypted pairing

X25519 with the public key travelling over the visual channel — optical is
line-of-sight, which makes a man-in-the-middle physically awkward.
`HKDF(shared_secret, qr_nonce || audio_nonce)`, ChaCha20-Poly1305 per PDU with a
derived nonce, and a **short authentication string** shown on both displays for
visual comparison.

## Overall verification

The strategy is that **almost everything is testable without two devices**:

1. `cargo test --workspace` — core against the simulated channel, wire round
   trips, synthetic QR distortion, FSK over AWGN.
2. Loopback mode — two instances on one machine, protocol end to end, no camera.
3. `cargo tauri dev` on one machine — UI, camera preview, alignment, decoding a
   QR code shown in another window.
4. Two devices facing each other — the final gate for each phase, manual.

## Risks

| Risk | Mitigation |
|---|---|
| Frame IPC does not deliver the throughput | Phase 0 measures it first; three defined fallbacks |
| Ultrasound does not survive real hardware (OS AEC and noise suppression filter above 16 kHz) | Calibration reports `Unavailable` honestly; audio is always optional |
| Autofocus hunting, display moiré, brightness | Error correction plus a 15% calibration margin plus a degradation policy |
| Deadlock between symmetric peers | Deterministic leader election by `peer_id` |
| Echo: every microphone hears its own speaker | Frequency division with disjoint bands per direction |
| Eight subsystems, the spec goes stale | Functional cut points per phase; 4-7 re-plannable without touching the core |

---

## Appendix: findings measured during implementation

These numbers were not in the original design. They came out of measuring, and
some of them contradict assumptions the design took for granted.

### Pixels per module: eight and a half, not three — and not six either

Sweep over the synthetic camera
(`cargo run -p optical-codec --example threshold`):

| px/module | ideal capture | typical capture |
|---|---|---|
| 2.0-3.0 | 24-40% | 0% |
| 3.0-6.0 | 60-87% | 10-67% |
| 6.0-7.0 | 91-100% | 64-65% |
| 7.0-8.5 | 93-100% | 64-94% |
| 8.5+    | 100%   | 100%  |

The standard gives 2 as an absolute minimum, but it assumes a grid aligned to the
pixel. A camera scales fractionally and the detector gets confused.

**The instructive part is that this was measured wrong once.** The first pass
swept only the ideal column and concluded 6.0. That number is real, and it
describes a capture with no blur, no noise and no tilt — which is not a capture.
At 6.0 px/module under conditions a webcam on a desk actually produces, barely
two frames in three read, and a link dropping a third of its frames is a retry
loop rather than a link.

It surfaced when the end-to-end optical test never converged: control frames read
perfectly and data frames never did, at 7.0 px/module — comfortably above the
threshold that had been recorded. Taking a measurement in the favourable case and
treating it as the general number is a method error, not an arithmetic one, and
it is worth naming because it is easy to repeat.

### Real per-frame capacity at 720p

Payload that decodes **reliably** (every repetition), with the code filling 75% of
the height, sized from the corrected 8.5 px/module threshold:

| receiver | modules | bytes per frame |
|---|---|---|
| 720p (laptop, desktop webcam) | 57 | 271 |
| 1080p (phone, tablet) | 85 | 644 |

The design assumed about 900 B. At 10 frames per second the corrected figures are
2.7-6.4 KB/s, so **a 5 MB file takes between 13 and 31 minutes**. Worth telling
the user before starting, not halfway through.

Mind the distinction between "decodes once" and "decodes every time", and the
one between ideal and realistic capture. Both mistakes inflate the number, and
both were made here before being caught.

### Measured pairing table

Per-frame capacity for each form-factor pairing, using each one's typical
hardware (`cargo test -p optical-codec --test device -- --nocapture`):

| Receiver's camera | Modules resolvable | Bytes per frame |
|---|---|---|
| 720p (laptop, desktop webcam) | 57 | 271 |
| 1080p (phone, tablet) | 85 | 644 |

**A 2.4x difference from the receiver's camera alone.** The sender's display
stops mattering once it is large enough to draw the code the peer can resolve,
which happens well before any modern display runs out of pixels.

This is what makes a phone-laptop pairing the best common combination and
laptop-laptop the weakest: the former uses each device for its strength, the
latter contributes a mediocre webcam at both ends.

The profile is set by **my display and the peer's camera**, never by my own
camera. Getting that backwards is an easy mistake and produces a link that is
mysteriously worse in one direction; there is a test named after it.

### Camera resolution is the dominant lever

Per-frame payload grows with the **square** of linear resolution, whereas raising
the frame rate scales linearly and lowering error correction buys barely a factor
of 2.5. Going from 720p to 1080p multiplies per-frame payload by about 2.25.
Calibration should prioritise negotiating the highest capture resolution the
camera will sustain.

This also explains why a phone paired with a laptop tends to win: the phone's
rear camera comfortably out-resolves a laptop webcam.

### RaptorQ: bounding the source block is not optional

Letting a 5 MB object fall into a single block of about 6000 symbols cost **over
nine minutes of CPU** to reconstruct. Capping at 1024 symbols per block brings it
down to seconds. The cost grows far worse than linearly in K because each block is
solved by Gaussian elimination over GF(256).

This forced a correction to the design: the OTI **does** travel the wire. Twelve
bytes once per transfer, and deriving it on both sides pinned the block splitting
to whatever `with_defaults` decided.

### Fountain versus ARQ, measured

5 MB transfer over the simulator:

| | symbols sent | theoretical minimum | excess |
|---|---|---|---|
| Fountain, 40% loss | 10,210 | 10,047 | **+1.6%** |
| ARQ, 15% loss | 10,095 | 7,091 | +42% |

Fountain coding operates near the theoretical optimum at nearly three times the
loss. This confirms it should be the default for bulk data, with ARQ reserved for
control.

### Known limitation of the sharpness measure

Laplacian variance rises with noise, not only with focus: a noisy, blurry image
can score higher than a clean, slightly blurry one. To use it as a focus
criterion, the noise has to be filtered first, or the measure combined with
another.
