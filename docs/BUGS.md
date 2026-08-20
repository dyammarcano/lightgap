# Bugs

<!-- rev:001 (RFC 3339) 2026-08-20T16:05:00Z -->

Things that behave incorrectly. Constraints that are merely inconvenient are in
[ISSUES.md](ISSUES.md).

## Open

### The desktop reads one frame in ten, and nothing explains it

**Severity:** high — it is the one thing between this and a completed transfer
over real hardware.

A laptop webcam looking at a tablet across a desk, after the resolution,
brightness and exposure calibration have all settled: **9.8 pixels per module**,
comfortably above the measured 8.5 threshold, and a **9% read rate**, with
nothing clipped and nothing counted as seen-but-unread. The other direction — a
tablet camera looking at a monitor — reads 90% of the time.

Good density with bad decodes and no clipping points at time rather than light:
a display changing every 80 ms smeared across an exposure long enough to span
more than one of them. Shortening the exposure when a code is resolved but
unread was tried and did not visibly move the figure, which weakens that
explanation without replacing it.

**Untried:** holding each code longer specifically for a peer reporting bad
reads — the hold is one number for both directions today; autofocus, which is
not touched at all and which a screen at close range defeats routinely; and
measuring at several distances, which would separate a focus explanation from a
framing one in about ten minutes.

### Guidance is offered before anything has been observed

**Severity:** low.

`engine.rs` initialises `last_advice` to `Advice::MoveCloser`, so the interface
tells you to move the devices closer before its camera has seen anything at all
— including while the camera is off. Advice derived from zero observations
should say nothing.

### Every log line appears twice on Android

**Severity:** low — the content is correct and the file log is unaffected, but it
doubles the length of every capture.

`adb logcat` shows each line from `tauri-plugin-log` exactly twice, with
identical timestamps, pids and thread ids. Not the `Webview` target (removing it
changed nothing) and not `LogDir`, which writes to a rotating file rather than to
logcat.

## Resolved

Kept because each was found by running something rather than by reading, and
each pointed at the wrong layer first.

| Bug | Looked like | Actually was |
|---|---|---|
| Every camera frame aborted the process on Android | a crash with no message | a raw binary body cannot cross Android's IPC bridge; the panic crossed an `extern "C"` boundary that cannot unwind |
| A fresh peer collapsed its frame to the minimum mid-handshake | the link giving up | a session that had measured nothing reported a plain zero, read as "you are unreadable" |
| The nonce parse rejected every announcement | corruption | a field cut at `HELLO_LEN - 1` after a byte was appended |
| Synthetic camera failed sharp images and read blurred ones | a broken detector | supersample offsets covering only the middle of each pixel; blur was acting as the anti-alias filter |
| Acoustic delivery *rose* with added noise | impossible | the final symbol dropped on any sync offset, surfacing as truncation |
| Every fountain symbol rejected — "received: 0" | a dead transport | `with_defaults` aligned the symbol size to a multiple of eight; the receiver validated against the requested size |
| A denser code aimed at the camera that could not read the sparse one | acquisition failing at random | escalation tested whether *we* could see the peer, not whether the peer could see us |
