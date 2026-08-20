# Known issues and limitations

<!-- rev:001 (RFC 3339) 2026-08-20T16:05:00Z -->

Constraints and gaps that are understood and not currently being worked on.
Things that behave *incorrectly* are in [BUGS.md](BUGS.md); work that is planned
is in [BACKLOG.md](BACKLOG.md).

## The link is slow, and always will be

192–520 payload bytes per frame depending on the receiving camera, at roughly
eight to twelve frames a second. That is single-digit kilobytes per second at
best. A 5 MB file takes tens of minutes.

This is not a defect to fix — it is what a display and a camera can carry. The
project is for keys, configuration and secrets, not for video, and the README
says so before it says anything else.

## Payloads are not encrypted

Key agreement works and the six-digit authentication string is displayed, so the
peer's identity can be confirmed. But `seal` and `open` exist in
`optical-protocol` and are not wired into the modem's data path: what crosses the
gap crosses in the clear.

**Workaround:** encrypt the file before sending it. The link carries bytes; it
does not care which.

## The acoustic channel has never made a sound

`acoustic-codec` modulates, demodulates, frames, and assigns bands, all tested
against synthetic AWGN, band-pass and clipping. There is no `cpal` driver behind
it, so the second medium exists only in tests.

Even wired up it would carry control traffic and acknowledgements, not bulk. It
would not make transfers faster.

## Ultrasound may simply not survive real hardware

Operating-system echo cancellation and noise suppression routinely filter above
16 kHz. The calibration is built to report `Unavailable` honestly rather than
open a channel that cannot work — but that means the acoustic path may be
unavailable on hardware where everything else is fine.

## iOS has never been generated

`cargo tauri ios init` needs macOS and Xcode. The Android project is committed
and customised; there is no iOS equivalent. See [mobile.md](mobile.md).

## The Android build is a few hundred megabytes

A debug APK carries unstripped Rust symbols for the whole stack. A release build
is a fraction of that and needs a signing key, which is a deliberate decision
rather than a build step — see the backlog.

## Device identifiers are re-salted between sessions

Browsers may issue a different `deviceId` for the same camera on a later run, so
the chosen camera is remembered by its *label* with the id as a fast path. A
camera whose label also changes will not be recognised, and the picker falls back
to the system default.

## The desktop window remembers its geometry but not its mode

Size, position and maximised state persist; fullscreen deliberately does not. It
is a mode entered on purpose, not a setting that should follow you into the next
run. F11 enters it, Escape leaves.
