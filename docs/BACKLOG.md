# Backlog

Work that is known, wanted and not done. Each entry says what it is and why it
is worth doing, so that picking one up does not start with reconstructing the
reasoning.

## Continuous integration and release artefacts

GitHub Actions is not set up at all. The repository is public and there is
nothing checking that a push still builds.

**CI.** On push and pull request: `cargo fmt --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo clippy -p lightgap-ui --target
wasm32-unknown-unknown -- -D warnings`, and `cargo test --workspace`. The wasm
target needs to be installed in the job and is easy to forget — the interface
crate only compiles for it, so a native-only run would pass while the frontend
was broken.

The test suite is the reason this is worth having rather than a formality: the
5 MB transfer at 40% loss and the two engines discovering each other through a
synthetic camera both run without hardware, so CI exercises the parts that are
otherwise only ever tested by hand with two devices on a table.

**Desktop binaries.** `cargo tauri build` for Windows, macOS and Linux on a
release tag. Unsigned to begin with; signing is a separate decision with
certificates attached to it.

**Android APK.** `cargo tauri android build --apk`. Two things this needs that
the desktop jobs do not: the Android SDK and NDK in the runner, and a signing
key. A debug APK is 373 MB because nothing is stripped — a release build is a
fraction of that, and worth doing properly rather than publishing the debug one.

**iOS** stays out. It needs macOS and Xcode and the project has never been
generated; see `docs/mobile.md`.

Notes for whoever writes this:

- Cache `~/.cargo` and `target/` or the wasm and Android builds will dominate
  the run time.
- `trunk` and `tauri-cli` both need installing in the job.
- The Android project under `gen/android` is committed and carries deliberate
  edits — camera permissions, `keepScreenOn`, the brightness plugin. CI must
  build it, never regenerate it.

## Encryption on the data path

Key agreement works and the authentication string is displayed, but payloads
still travel in the clear. `seal` and `open` exist in `optical-protocol` and are
tested; what is missing is wiring them into the modem's send and receive paths,
with the nonce derived from `(session_id, direction, seq)` as designed.

Worth doing carefully rather than quickly: the failure mode of getting the
direction or sequence wrong is a session where each side can encrypt and neither
can decrypt, which on this link is indistinguishable from a dirty lens.

## Calibration wired into the engine

`link-calibration` is implemented and tested — probe ladder, goodput scoring,
AIMD control — and the running engine uses none of it. The frame size is fixed
at whatever `starting_mtu()` returns and never climbs, so a link with room to
spare is never used.

Measured evidence that this matters: a real link ran at 82 bytes per frame while
resolving 7.1 pixels per module. The profile that produced that number was
computed for an assumed laptop, not for the two devices actually facing each
other.

## An acoustic driver

`acoustic-codec` modulates, demodulates, frames and calibrates, all tested
against synthetic impairment. It has never made a sound: there is no `cpal`
driver behind it. Until there is, the second medium exists only in tests.

## Advice before observation

`engine.rs` initialises `last_advice` to `Advice::MoveCloser`, so the interface
confidently tells you to move the devices closer before its camera has seen
anything at all — including while the camera is off. Guidance derived from zero
observations should say nothing.

## Every log line appears twice on Android

`adb logcat` shows each line from `tauri-plugin-log` exactly twice, with
identical timestamps, pids and thread ids — 52 entries for 26 messages.

What has been ruled out: it is not the `Webview` target (removing it changed
nothing), and it is not `LogDir`, which writes to a rotating file rather than to
logcat. On Android the plugin maps `TargetKind::Stdout` to `android_logger::log`
and that is the only path to logcat in its dispatch.

Cosmetic — the content is correct and the file log is unaffected — but it doubles
the length of every capture and would be worth ten minutes with only one target
registered to find.

## A licence

The repository is public with no `LICENSE` file, which means all rights
reserved: readable by anyone, usable by no one. A deliberate choice either way
is better than the default.
