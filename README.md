# Lightgap

<!-- rev:001 (RFC 3339) 2026-08-20T16:00:38Z -->

An air-gapped multimodal modem. Two devices exchange files using only the
hardware they already have — a display and a camera, and optionally a speaker
and microphone. No network, no Bluetooth, no cable.

This is not "a QR code with a link in it". It is a full transport layer over an
optical medium: the display emits packets as animated QR codes, the peer's camera
captures them, and on top runs a handshake, sequence numbers, acknowledgements,
retransmission, flow control and encryption.

## Why

Moving a secret, a configuration file or a key between isolated machines, where
plugging in a USB stick or bringing up a network is not acceptable. The display
and camera are already there, already trusted, and already air-gapped.

## Honest expectations

The measured numbers, not aspirational ones.

| Receiver's camera | Bytes per frame |
|---|---|
| 720p (laptop, desktop webcam) | 192 |
| 1080p (phone, tablet) | 520 |

At roughly ten frames per second that is 1.9-5.2 KB/s. **A 5 MB file takes tens
of minutes.** This is a tool for keys and configuration, not for video.

These figures have been revised downward twice, both times by removing optimism
from the model rather than by finding a bug in the code: once when the
pixels-per-module threshold turned out to have been measured under ideal capture
rather than realistic capture, and once when the quiet zone turned out not to be
counted against the camera pixels it actually consumes.

Camera resolution is the dominant lever: per-frame payload grows with the
*square* of linear resolution, whereas raising the frame rate scales linearly and
lowering error correction buys barely a factor of 2.5.

The practical consequence is that **a phone paired with a laptop is the best
common combination** — the laptop brings the large display, the phone brings the
good camera. Laptop-to-laptop is the weakest, because both ends contribute a
mediocre webcam.

**No device can see its own screen.** Camera and display face opposite ways on
every current form factor, so every pairing needs two physical devices.

## Structure

```
crates/
  optical-protocol/   sans-io core: wire format, session, reliability, mux, crypto
  optical-codec/      QR encode/decode, framing geometry, synthetic camera
  acoustic-codec/     2-FSK modulation, framing, calibration, synthetic impairment
  link-calibration/   probe ladders, goodput scoring, adaptive control
  channel-sim/        lossy channel for testing the core without hardware
  modem/              the transfer engine: all of the above, composed
tauri-app/            desktop and mobile application
```

The protocol core opens no cameras, no sockets and no audio devices. It is a pure
state machine — hand it incoming packets, ask what to transmit, tell it time has
passed. That is what lets a 5 MB transfer at 40% packet loss be tested without
turning on a single camera.

The consequence that matters: the protocol does not know which medium it travels
over. Adding audio, LEDs or a TCP socket means implementing a trait, not editing
the state machine.

## Building and testing

```bash
# Everything, no hardware needed.
cargo test --workspace

# Desktop.
cd tauri-app && cargo tauri dev

# Android.
cd tauri-app && cargo tauri android dev
```

iOS needs macOS and Xcode; see [docs/mobile.md](docs/mobile.md).

Some figures are printed rather than asserted, because they are the answer to
"what should I expect?":

```bash
# Per-frame capacity for every device pairing.
cargo test -p optical-codec --test device -- --nocapture

# Where the acoustic channel gives out.
cargo test -p acoustic-codec --test acoustic -- --nocapture

# How many camera pixels per QR module are actually needed, under ideal,
# typical and harsh capture.
cargo run -p optical-codec --example threshold
```

The engine itself is exercised end to end without hardware, including one test
that renders real QR codes and photographs them with the synthetic camera:

```bash
cargo test -p modem -- --nocapture
```

## How it works

Two ideas do most of the work.

**Fountain coding instead of acknowledging every frame.** The sender emits coded
symbols continuously and waits for nothing; the receiver reconstructs once it has
gathered enough, regardless of *which* ones arrived. This removes the optical
round trip — display a code, capture it, decode it, answer with another code —
which is the dominant latency in this medium. Measured over 5 MB at 40% loss,
fountain coding sends only 1.6% more than the theoretical minimum; sliding-window
ARQ at 15% loss sends 42% more.

**Calibration rather than constants.** How much data fits in one frame depends on
the peer's camera, the lighting, how steady the hands are and how squarely the
displays face each other. The application measures instead of assuming: a probe
ladder finds the largest payload the link sustains, scores candidates by what they
actually *deliver* rather than what fits, and keeps adjusting during the transfer.

The acoustic channel, where the hardware supports it, carries acknowledgements
and signalling. It is opportunistic — the application never assumes audio works,
it measures, and it is prepared for the answer to be no. On a lot of hardware,
operating-system echo cancellation and noise suppression remove everything near
the band this modulation uses.

## Security

X25519 pairing with ChaCha20-Poly1305 per packet, separate keys per direction.

The optical channel has a useful property: it is line-of-sight and short range, so
the realistic attack is not passive eavesdropping but a man-in-the-middle
substituting their own key during pairing. **A six-digit authentication string is
shown on both displays for the user to compare, and that is what actually closes
that hole** — the key exchange alone does not. A design that stops at the key
exchange is security theatre.

## Documentation

- [Architecture](docs/ARCHITECTURE.md) — components, a frame's journey, and the
  handshake lifecycle
- [Design](docs/superpowers/specs/2026-08-19-multimodal-modem-design.md) — the
  full design, with an appendix of what measurement contradicted about it
- [Decisions](docs/adr/) — why the core performs no I/O, and why the decode
  moved into the interface
- [Mobile](docs/mobile.md) — Android and iOS, and the pairing table
- [Phase 0 measurement](docs/phase-0-measurement.md) — the camera-path question
  that decided where the decode runs
- [Backlog](docs/BACKLOG.md) · [Issues](docs/ISSUES.md) ·
  [Bugs](docs/BUGS.md) — what is known and undone, with the evidence
- [Contributing](docs/CONTRIBUTORS.md) — toolchain, commands, conventions

## Status

Everything below the application is implemented and tested: the protocol core,
the optical codec, calibration, the acoustic codec, the multiplexer, pairing and
the transfer engine. Two modems move a real file end to end over a channel
losing 70% of frames, and separately through real QR codes photographed by a
synthetic camera, with no hardware involved.

Both applications exist. Desktop and Android build from this tree and hold a
link across a real gap: they find each other, lock, agree on a frame size from
each other's cameras, derive a shared key and display the same six digits.
Throughput on real hardware has been measured at roughly 1.2 kB/s in the good
direction.

What is not finished is the last mile of that link. One direction of one pair
reads only about a tenth of the frames it is shown for reasons not yet
understood, which is enough to hold a session and not enough to complete a large
transfer — see [docs/BUGS.md](docs/BUGS.md). Payloads also still cross the gap
unencrypted: key agreement works, but `seal` and `open` are not yet wired into
the data path ([docs/ISSUES.md](docs/ISSUES.md)).

The acoustic channel is implemented and has never made a sound; there is no
audio driver behind it.

## License

MIT — see [LICENSE](LICENSE).
