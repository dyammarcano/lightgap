# Phase 0 — measurement procedure

Working document. Deleted along with the spike once the decision is made.

## What is being decided

The design chose the hybrid camera path: preview via `getUserMedia` in the
WebView, QR decoding in the Rust backend. That choice pays an IPC cost for every
frame. Phase 0 measures that cost before anything is built on top of it.

**Pass criterion:** 10 or more decodes per second, sustained, at under 30% of one
core.

If it does not pass, the dominant segment says which fallback applies — which is
why the loop is timed in three parts rather than one:

| Dominant segment | What it means | Fallback |
|---|---|---|
| **Capture** | the GPU-to-CPU readback of `getImageData` dominates | lower the resolution or use `OffscreenCanvas`. Moving decode to WASM does **not** help |
| **IPC** | crossing the bridge costs more than decoding | **(b)** decode in WASM: it removes the crossing entirely |
| **Decode** | `rqrr` is the bottleneck | **(a)** crop to the region of interest after first lock. WASM does **not** help either |

## Setup

The camera has to see a display showing the QR code. A laptop's built-in webcam
faces the user and **cannot see its own screen**, so you need one of:

- a second monitor showing the spike window, with the laptop looking at it;
- a USB webcam aimed at the display;
- the second machine, if it is already in front of you;
- a phone running the mobile build — usually the easiest option, and its rear
  camera is better than most laptop webcams.

With no QR code in view the capture and IPC segments remain valid — they are what
decides the go/no-go — but the decode segment reads optimistically: `rqrr` still
runs full detection but skips decoding when it finds no grid.

## Running it

```bash
cd tauri-app
cargo tauri dev
```

Grant camera permission when the WebView asks. Frame the QR code, press
**Start**, and let it run for about 30 seconds so the rolling means settle (a
30-sample window).

For the CPU criterion, with the spike already measuring, in another terminal:

```bash
pwsh -File scripts/spike-cpu.ps1 -Seconds 30
```

It counts the `msedgewebview2` processes too: `getImageData` and the canvas live
there, not in the Rust process. Counting only the parent would understate the
hybrid path.

## Matrix to fill in

Each row is a run of about 30 seconds. The two IPC paths at 1280x720 are the
comparison that justifies (or refutes) the design decision; the lower resolutions
show how much headroom there is.

| Path | Resolution | Decodes/s | Capture ms | IPC ms | Decode ms | Bytes/frame | Frames with QR | % of 1 core |
|---|---|---|---|---|---|---|---|---|
| Raw binary | 1280x720 | | | | | | | |
| Raw binary | 960x540 | | | | | | | |
| Raw binary | 640x480 | | | | | | | |
| JSON (control) | 1280x720 | | | | | | | |

Size reference: 1280x720 greyscale is 921,608 B per frame (8 B header plus w*h).
Over the JSON path those same bytes travel as numbers in text — on the order of
4 MB.

## Result

The table above was never filled in, and the reason is worth recording: the
question it was designed to answer was settled before the harness could answer
it, and settled the other way.

- **Verdict:** the hybrid path is **viable on desktop and impossible on
  Android.** Android's WebView does not expose the body of an intercepted
  request, so Tauri falls back to a text channel and a raw binary body cannot
  cross the IPC bridge at all. The failure is not a slow frame — `Rust_ipc` is
  `extern "C"` and cannot unwind, so the rejection became `panic_cannot_unwind`
  and aborted the process on the first camera frame, before one had ever been
  displayed.
- **Dominant segment:** on the path actually shipped, **decode**. Transport
  fell to roughly 3 ms once what crossed the bridge was the decode's output
  (under a hundred bytes) rather than its input (921,608 B). Capture and decode
  together cost 75–100 ms per scan.
- **Decision:** **fallback (b) — move the decode to wasm.** Not chosen for
  throughput, chosen because (a) and (c) could not have helped: region tracking
  reduces pixels but they still could not cross, and `nokhwa` has no Android
  camera behind it. Fallback (a) was then adopted **as well**, on top of (b):
  the interface scans the whole frame while searching and only the code's
  neighbourhood once locked, which is what makes a wasm decode fast enough.
- **Operating point chosen for Phase 2:** capture at 1920x1080, scan a budget
  of 950,000 px while searching and 420,000 px while tracking, at roughly 10–13
  scans per second. Raising the search budget with the capture resolution was
  tried and reverted — it took a scan from 130 ms to 285 ms and read no more
  codes.

The spike code (`tauri-app/src/spike.rs`, `tauri-app/src-tauri/src/spike.rs`,
`scripts/`, the `.spike` styles and the Phase 0 dependencies) has been removed.
