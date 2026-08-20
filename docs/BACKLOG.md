# Backlog

Work that is known, wanted and not done. Each entry says what it is and why it
is worth doing, so that picking one up does not start with reconstructing the
reasoning.

## Release artefacts that are signed

`.github/workflows/` now carries CI (fmt, clippy on both targets, the test
suite, and a frontend build that checks the styles survived Tailwind's scan)
and a release workflow producing desktop bundles for the three platforms plus
an Android APK on a tag.

What is deliberately not done there is signing. The desktop bundles are
unsigned and the APK is a debug build — a few hundred megabytes, because
nothing is stripped. Both need certificates or a keystore in the repository's
secrets, which is a decision to make on purpose rather than a step to add while
wiring up a build. A release APK also needs `keyAlias`/`storeFile` wiring in
`gen/android/app/build.gradle.kts`, which is committed and carries deliberate
edits, so it has to be modified rather than regenerated.

**iOS** stays out entirely. It needs macOS and Xcode and the project has never
been generated; see `docs/mobile.md`.

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

## The desktop direction reads one frame in ten

Measured on a laptop webcam looking at a tablet across a desk, after the
resolution, brightness and exposure calibration all settled: 9.8 pixels per
module — comfortably above the 8.5 threshold — and a 9% read rate, with nothing
clipped and nothing counted as seen-but-unread. The other direction, a tablet
camera looking at a monitor, reads 90% of the time.

Good density with bad decodes and no clipping points at time rather than light:
a display that changes every 80 ms smeared across an exposure long enough to
span more than one of them. Shortening the exposure when a code is resolved but
unread was tried and did not visibly move the figure, which weakens that
explanation without replacing it.

What has not been tried: holding each code longer specifically for a peer that
reports reading badly (the hold is one number for both directions today);
autofocus, which is not touched at all and which a screen at close range defeats
routinely; and simply measuring at several distances, which would separate a
focus explanation from a framing one in about ten minutes.

It is the one thing standing between this and a completed transfer over real
hardware. Everything above it in the stack has been measured working.

## A licence

The repository is public with no `LICENSE` file, which means all rights
reserved: readable by anyone, usable by no one. A deliberate choice either way
is better than the default.
