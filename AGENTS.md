# Lightgap — agent instructions

<!-- rev:002 (RFC 3339) 2026-08-20T16:03:45Z -->

An air-gapped multimodal modem: two devices exchange files using only a display
and a camera. A full transport layer over an optical medium — handshake,
sequence numbers, fountain coding, retransmission, X25519 pairing.

Rust workspace, Tauri 2 + Leptos 0.8 (CSR, Trunk). Desktop and Android from one
source tree.

## Build and test

```bash
cargo test --workspace                  # everything, no hardware needed
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p lightgap-ui --target wasm32-unknown-unknown -- -D warnings
cargo fmt --all
```

Clippy runs **twice**. The interface crate compiles for no target but wasm, so a
native-only run goes green with a broken frontend.

```bash
cd tauri-app && cargo tauri dev                              # desktop
cd tauri-app && cargo tauri android build --apk --debug --target aarch64
```

Coverage: `cargo llvm-cov --workspace --summary-only`.

## What matters in this codebase

**Measure, do not reason.** Nearly every bug found here was found by running
something, and each pointed at the wrong layer first: a synthetic camera that
read blurred images and failed sharp ones, an acoustic delivery rate that *rose*
with noise, a fountain symbol size silently rounded to a multiple of eight. When
a number is surprising, print it before explaining it.

**Per-frame capacity has been revised down three times**, every time by removing
optimism rather than fixing a defect. Treat any figure in the docs as measured
under stated conditions, and say which.

**The link is measured separately in each direction.** How well this end reads
the peer says nothing about how well the peer reads this end — different camera,
different display. Anything that sizes, dims or paces a transmission must use
the peer's report, never our own reading.

**The display is the transmitter.** Its brightness, its size and the code's
white level are link parameters, not presentation. Shrinking the code to make
room for a caption costs throughput.

## Hard rules

- `KDF_CONTEXT` and `SAS_CONTEXT` in `optical-protocol/src/crypto.rs` still read
  `qr_comm`. They are protocol constants, not branding: changing the text
  changes every session key, and two builds that disagree cannot talk at all.
  If they ever change, bump `v1` in the same edit.
- `SessionKeys` must not derive `Debug`. A test asserts this.
- `tauri-app/src-tauri/gen/android/` is committed and carries deliberate edits —
  camera and audio permissions, `WAKE_LOCK`, keep-screen-on, the brightness
  plugin. Build it; never run `android init` over it.
- A wire-format change makes rebuilding **both** apps mandatory, not tidy. An
  older build rejects a new PDU kind as unknown.

## Style

Comments explain *why*, and name the failure the code prevents. Tests assert the
invariant, not the current behaviour — several here were written against a buggy
implementation and had to be corrected to state the real rule.

Constants carry the measurement that produced them. `MIN_PIXELS_PER_MODULE = 8.5`
is not a preference.

## Commits

Subject in the imperative, no type prefix beyond the conventional
`feat:`/`fix:`/`perf:`/`ci:`. The body records what was learned, especially when
a change corrects earlier reasoning. Trailer:

```
Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
```

## Further reading

- [Architecture](docs/ARCHITECTURE.md) — components and flows
- [Design](docs/superpowers/specs/2026-08-19-multimodal-modem-design.md) — with
  an appendix of what measurement contradicted
- [Roadmap](docs/ROADMAP.md) — where each phase landed, and coverage by crate
- [Backlog](docs/BACKLOG.md) — what is known and undone, with the evidence
- [Bugs](docs/BUGS.md) — read this before diagnosing a link that will not read
- [Mobile](docs/mobile.md) — Android and iOS
