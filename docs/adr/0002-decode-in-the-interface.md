# ADR-0002 — The QR decode runs in the interface, not the backend

**Status:** Accepted — supersedes the hybrid camera path chosen in the design

## Context

The design chose a hybrid camera path: preview with `getUserMedia` in the
WebView, decode in the Rust backend, with frames crossing as a raw binary body
because sending them as JSON would turn nine hundred kilobytes of pixels into
four megabytes of text.

That worked on desktop. On Android it could never have worked. Android's WebView
does not expose request bodies to an intercepted request, so Tauri falls back to
a text channel and a raw binary body cannot cross the IPC bridge at all.

The symptom was not a rejected frame. `Rust_ipc` is `extern "C"` and cannot
unwind, so the error became `panic_cannot_unwind` and aborted the process — on
every camera frame, before the first one was ever displayed. The application
could not receive anything.

## Decision

The decode moved in front of the boundary, into wasm. What crosses now is what
the decode produced: under a hundred bytes.

`Engine::on_camera_frame` remains but is `#[cfg(test)]`. It is the only path
that exercises the decoder end to end against the synthetic camera, which is the
half worth testing and the half with no interface to run inside.

## Consequences

The platform difference is removed rather than papered over: both platforms now
take the same path.

The bridge carries four orders of magnitude less. Transport fell from a
significant cost to about 3 ms.

The decode is slower — `rqrr` compiled to wasm costs roughly two to three times
what it costs natively — and it runs on the interface thread. That is paid back
by region tracking: once a code is found, only its neighbourhood is scanned, and
the same pixel budget over a fifth of the frame is five times the pixels per
module.

This was fallback (b) in the original design, reached for a reason nobody
anticipated.
