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

- **Verdict:** _(pending)_
- **Dominant segment:** _(pending)_
- **Decision:** _(pending — stay on the hybrid path, or take fallback (a)/(b)/(c))_
- **Operating point chosen for Phase 2:** _(pending)_

Once recorded: delete `tauri-app/src/spike.rs`,
`tauri-app/src-tauri/src/spike.rs`, `scripts/`, the `.spike` styles in
`styles.css`, the dependencies marked as Phase 0 in both manifests, and put
`main.rs` back to mounting `<App/>`.
