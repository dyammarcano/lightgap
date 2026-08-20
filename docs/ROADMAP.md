# Roadmap

<!-- rev:001 (RFC 3339) 2026-08-20T16:03:24Z -->

The design set out eight phases, each meant to leave the application usable.
Seven are done. What follows records where each one landed, including the two
that landed somewhere other than where they were aimed.

## Phases

### Phase 0 — measure the camera path · **complete, and it changed the design**

The question was whether a raw binary frame could cross the IPC bridge fast
enough to keep the decode in the backend. On desktop, yes. On Android the
answer was not "too slow" but "not at all" — the WebView does not expose a
request body, and the rejection aborted the process on the first frame.
Fallback (b) was taken: the decode moved into the interface, in wasm. See
[phase-0-measurement.md](phase-0-measurement.md) and
[ADR-0002](adr/0002-decode-in-the-interface.md).

### Phase 1 — protocol core · **complete**

Wire format with CRC32, session state machine, leader election by peer id, both
reliability strategies behind one trait, and a simulated channel with seeded
loss, reordering, duplication and corruption. A 5 MB object crosses a channel
losing 40% of frames as a unit test.

### Phase 2 — optical channel and interface · **complete**

QR encode and decode with recovered geometry, the synthetic camera (perspective
warp, blur, sensor noise), and both applications. Two engines find each other
by photographing one another's screens with no hardware involved.

### Phase 3 — visual calibration · **complete**

Probe ladder, goodput scoring, AIMD, and a channel lifecycle. In the shipped
application this became a set of continuous loops rather than a one-off probe:
exposure, brightness and frame size all settle while the link is up, each sized
from the *peer's* report rather than from our own reading.

### Phase 4 — acoustic channel · **codec complete, no driver**

2-FSK modulation, Goertzel detection, preamble correlation and framing, tested
against synthetic AWGN, band-pass and clipping. There is no `cpal` behind it,
so it has never made a sound. See [ISSUES.md](ISSUES.md).

### Phase 5 — acoustic calibration · **logic complete, unexercised**

Per-band noise floor, viability grading and the commit/verify handshake are
implemented and tested. They have never met a real microphone, and the honest
expectation is that some hardware will report `Unavailable`.

### Phase 6 — multiplexer · **complete**

Priority classes, health-driven channel selection, duplication of critical
traffic with deduplication by `(session, seq)`.

### Phase 7 — pairing · **key agreement complete, payloads still in the clear**

X25519 over the optical channel, HKDF, and the six-digit authentication string
displayed on both screens — the part that actually closes a man-in-the-middle.
`seal` and `open` exist and are tested but are not yet wired into the modem's
data path. This is the single largest gap in the project; it is item E-1 in
[IMPLEMENTATION_TASKS.md](IMPLEMENTATION_TASKS.md).

## Where the work actually is now

Not in the phase list. Everything above holds together across a real gap: two
devices find each other, lock, negotiate a frame size from each other's
cameras, and derive the same key. What does not yet work is one direction of
one real pair reading reliably enough to finish a large transfer — about a
tenth of frames, for reasons the exposure fix did not explain. That is
[BUGS.md](BUGS.md), and it blocks the rest of v0.2.

## Test coverage

`cargo llvm-cov --workspace --summary-only`, measured 2026-08-20.

| Crate | Regions | Region cover | Lines | Line cover |
|---|---:|---:|---:|---:|
| `channel-sim` | 214 | 97.2% | 179 | 96.6% |
| `acoustic-codec` | 697 | 93.7% | 469 | 91.5% |
| `optical-codec` | 982 | 93.4% | 644 | 91.6% |
| `link-calibration` | 297 | 91.6% | 246 | 91.1% |
| `optical-protocol` | 1504 | 90.3% | 1151 | 90.7% |
| `modem` | 552 | 85.5% | 413 | 83.8% |
| `tauri-app/src-tauri` | 988 | 47.1% | 665 | 46.0% |
| `tauri-app/src` | 1965 | 0.0% | 1097 | 0.0% |
| **Workspace** | **7199** | **60.4%** | **4864** | **64.0%** |

The library crates alone — everything that is sans-io — are at **91.4%** of
regions and **90.5%** of lines. The workspace figure is dragged down by the two
application crates, and that is a deliberate boundary rather than neglect:

- `tauri-app/src` is the interface. It is 1,965 regions of camera capture,
  canvas work and `applyConstraints` calls that only exist inside a browser,
  compiled for a target the test harness does not run. Its testable logic —
  frame sizing, exposure decisions, advice — lives behind the boundary in the
  engine, which is where it is tested.
- `tauri-app/src-tauri` at 47% is mostly `engine.rs` at 62%. The uncovered
  remainder is `commands.rs` (Tauri command handlers, 0%) and `brightness.rs`
  (an Android plugin call, 0%), neither of which can run without a Tauri
  runtime and a device.

The number worth watching is the library figure. If it falls, something that
could have been tested was not.
