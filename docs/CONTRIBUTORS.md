# Contributing

<!-- rev:001 (RFC 3339) 2026-08-20T16:05:00Z -->

## Maintainer

Dyam Marcano — <https://github.com/dyammarcano>

## Toolchain

No pin file. Built and tested against Rust 1.96 stable, Tauri CLI 2.11, Trunk
0.21. The interface needs the `wasm32-unknown-unknown` target:

```bash
rustup target add wasm32-unknown-unknown
```

Android additionally needs a JDK 21, the Android SDK, NDK 27, and the four
Android Rust targets.

## Before opening a pull request

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p lightgap-ui --target wasm32-unknown-unknown -- -D warnings
cargo test --workspace
```

CI runs exactly these. Clippy runs twice on purpose: the interface crate
compiles for no target but wasm, so a native-only run goes green with a broken
frontend.

## What review will ask about

**Where is the measurement?** Almost every bug in this project was found by
running something, and each pointed at the wrong layer first. A change justified
by reasoning about how the optics ought to behave will be asked for the number.

**Which direction?** The link is measured separately each way. Anything that
sizes, dims or paces a transmission must act on the peer's report, not on our
own reading — those measure opposite directions and are routinely different.

**Does the comment name the failure?** Comments here explain why the code is
shaped as it is, usually by naming what goes wrong otherwise. A comment
restating the code will be asked to say something else or go.

**Does the test state the invariant?** Several tests here were originally
written against a buggy implementation and had to be corrected to assert the
real rule. Assert what must be true, not what currently happens.

## Things that will break the link if changed carelessly

- `KDF_CONTEXT` and `SAS_CONTEXT` still read `qr_comm`. Protocol constants, not
  branding: changing the text changes every session key.
- `tauri-app/src-tauri/gen/android/` is committed with deliberate edits. Build
  it; never regenerate it.
- Any wire-format change makes rebuilding both applications mandatory. An older
  build rejects a new PDU kind as unknown.

## Commits

Imperative subject with a conventional prefix. The body records what was
learned, especially where a change corrects earlier reasoning — several commits
here exist mainly to explain why the previous approach was wrong.
