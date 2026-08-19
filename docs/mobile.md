# Mobile: Android and iOS

The protocol is symmetric and medium-agnostic, so every pairing works with the
same code: desktop-desktop, desktop-mobile, mobile-mobile, and mobile-tablet.
What changes between form factors is not the protocol but the **profile**, and
working that out is what `optical-codec`'s `device` module does.

## Why mobile is worth supporting

Measured with `cargo test -p optical-codec --test device -- --nocapture`, using
each form factor's typical hardware:

| Receiver's camera | Modules resolvable | Bytes per frame |
|---|---|---|
| 720p (laptop, desktop webcam) | 57 | 271 |
| 1080p (phone, tablet) | 85 | 644 |

**A 2.4x difference from the receiver's camera alone.** Per-frame payload grows
with the square of linear resolution, so a phone's rear camera on the receiving
end is worth far more than any amount of tuning on the sending end.

The consequence is that **a phone paired with a laptop is usually the best
combination**: the laptop provides the large display and the phone provides the
good camera, so each device is used for its strength. Laptop-to-laptop is the
weakest common pairing, because both ends contribute a mediocre webcam.

## The constraint nobody escapes

**No device can see its own screen.** Not a laptop, not a phone, not a tablet.
The camera and the display face opposite ways on every current form factor.

Any pairing therefore needs two physical devices. For testing on your own you
need a second monitor, a USB webcam, or a phone — which is often the easiest of
the three.

## Android

Tooling required: Android SDK, NDK, and a JDK. This project was set up against
NDK 27.3.13750724 and Temurin JDK 21, with `ANDROID_HOME`, `NDK_HOME` and
`JAVA_HOME` set.

```bash
cd tauri-app

# One-off: generates src-tauri/gen/android. Already done and committed.
cargo tauri android init

# Run on a connected device or emulator.
cargo tauri android dev

# Build an APK.
cargo tauri android build --debug
```

### What was customised in the generated project

`cargo tauri android init` produces a working skeleton, but three things had to
be added for this particular app. They live in
`src-tauri/gen/android/app/src/main/`, which is committed rather than generated
at build time.

**Permissions** in `AndroidManifest.xml`:

- `CAMERA` — without it the optical channel cannot receive anything.
- `RECORD_AUDIO` and `MODIFY_AUDIO_SETTINGS` — the acoustic channel needs both.
  wry's `onPermissionRequest` asks for `MODIFY_AUDIO_SETTINGS` alongside
  `RECORD_AUDIO` when the WebView requests `AUDIO_CAPTURE`; without it declared,
  the runtime prompt fails and `getUserMedia({audio: true})` is denied.
- `WAKE_LOCK` — backs the window flag described below.

`android.hardware.camera` is declared as `required="true"`. Without a camera this
app has no way to receive anything, so shipping to a device without one would be
a guaranteed failure at first use.

**Screen behaviour** in `MainActivity.kt`:

- `FLAG_KEEP_SCREEN_ON`. The display is not showing content here, it **is** the
  transmitter. If Android dims the screen or lets it sleep partway through a
  transfer, the optical link dies mid-file.
- `BRIGHTNESS_OVERRIDE_FULL`. The optical channel's signal-to-noise ratio is
  literally the contrast the peer's camera sees, so a display at 30% brightness
  is a link operating well below what the hardware can do. The override is
  per-window, so the system restores the user's preference when the activity goes
  away.

### Camera permissions in the WebView

This works without extra code. wry's `RustWebChromeClient` already implements
`onPermissionRequest`, mapping `VIDEO_CAPTURE` to `CAMERA` and `AUDIO_CAPTURE` to
`RECORD_AUDIO` plus `MODIFY_AUDIO_SETTINGS`, and launching the runtime prompt.
Verified by reading `wry-0.55.1/src/android/kotlin/RustWebChromeClient.kt` rather
than assuming it.

The frontend asks for `facingMode: "environment"` so a phone uses its rear
camera, which is the good one and the one that will be aimed at the peer.

## iOS

**Not generated in this repository, because it cannot be.** `cargo tauri ios
init` requires macOS and Xcode; this project was set up on Windows. The steps
below are what to run on a Mac, not something that has been verified here.

```bash
cd tauri-app
cargo tauri ios init
cargo tauri ios dev
```

Two things will need adding to the generated Xcode project, mirroring what
Android needed:

- `NSCameraUsageDescription` and `NSMicrophoneUsageDescription` in `Info.plist`.
  iOS refuses to even prompt without a usage string, and the app is terminated if
  it asks for a device it has not declared.
- Disabling the idle timer (`UIApplication.shared.isIdleTimerDisabled = true`) for
  the same reason Android needs `FLAG_KEEP_SCREEN_ON`: the display is the
  transmitter.

WKWebView has supported `getUserMedia` since iOS 14.3, so the camera path should
work the same way it does on Android. That is an expectation from the platform
documentation, not something measured on a device.

## Testing a mixed pairing

The protocol layers need no hardware to test, and that does not change on mobile:

```bash
cargo test --workspace
```

For the profile logic specifically, including every form-factor pairing:

```bash
cargo test -p optical-codec --test device -- --nocapture
```

That prints the full pairing table, which is also the honest answer to "how long
will this take?" before anyone starts holding two devices up.

Real hardware testing still requires two devices facing each other. There is no
way around it, for the reason at the top of this document.
