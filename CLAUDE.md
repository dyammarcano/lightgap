# Lightgap — Claude Code

<!-- rev:001 (RFC 3339) 2026-08-20T16:05:00Z -->

@AGENTS.md

## Claude-Code specifics

Two devices and two cameras are involved, and neither is visible from here.
Verify optical behaviour by capturing both screens rather than by reasoning
about what should be on them:

```bash
# Tablet.
adb exec-out screencap -p > shot.png

# Desktop, via PowerShell.
# System.Windows.Forms + CopyFromScreen into a bitmap.
```

`adb shell` mangles Unix paths under Git Bash — `/sdcard/...` becomes a Windows
path. Use PowerShell for anything that pushes or pulls a device file.

The engine logs to stdout, to a rotating file, and on Android to `logcat`:

```bash
adb logcat -b main -d | Select-String "lightgap_lib::engine"
```

One line every ten seconds carries the whole link state — read rate, pixels per
module, frame size, both throughputs, scan time. It is faster than a screenshot
and it is what a transfer's history looks like afterwards.
