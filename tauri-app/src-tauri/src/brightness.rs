//! Display brightness, which on this application is transmit power.
//!
//! Nothing else in the interface changes the physics of the link. The optical
//! channel's signal-to-noise ratio is the contrast the peer's camera resolves,
//! and that is set by how much light this display puts out — so this is a
//! transmitter gain control that happens to look like a screen setting.
//!
//! It is not a monotonic "more is better" dial, which is why it is exposed at
//! all rather than pinned at maximum. Held close to a camera, a display at full
//! output blooms: the sensor clips, white bleeds across module boundaries and
//! the code stops resolving. When two devices are nearly touching, turning the
//! transmitter *down* is the fix.
//!
//! # Why this goes through a mobile plugin
//!
//! The first version of this file reached the activity with raw JNI, taking the
//! `JavaVM` and context from `ndk-context`. It aborted the entire process on the
//! very first command the interface sent, with `android context was not
//! initialized`: nothing in a Tauri application populates that crate's statics,
//! because Tauri has its own JNI glue. The panic then crossed the `extern "C"`
//! boundary of wry's IPC entry point, which cannot unwind, so a recoverable
//! mistake became `SIGABRT` before the first frame was ever drawn.
//!
//! Two lessons are baked into the shape below. Reach the platform through the
//! accessor the framework actually provides, and never let a fallible call sit
//! where a panic would cross an FFI boundary.

use tauri::Runtime;

/// Whether this platform's display brightness is the application's to set.
///
/// False on desktop, and deliberately: a desktop display's brightness belongs
/// to the monitor and the operating system, not to one window sitting on it.
#[must_use]
pub const fn controllable() -> bool {
    cfg!(target_os = "android")
}

#[cfg(target_os = "android")]
mod platform {
    use serde::Serialize;
    use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
    use tauri::{AppHandle, Manager, Runtime};

    /// Must match the application identifier: the plugin class is looked up as
    /// `<identifier with / for .>/<class name>`.
    const IDENTIFIER: &str = "dev.lightgap.desktop";

    struct Brightness<R: Runtime>(PluginHandle<R>);

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Level {
        level: f32,
    }

    pub fn init<R: Runtime>() -> TauriPlugin<R> {
        Builder::new("brightness")
            .setup(|app, api| {
                let handle = api.register_android_plugin(IDENTIFIER, "BrightnessPlugin")?;
                app.manage(Brightness(handle));
                Ok(())
            })
            .build()
    }

    pub fn set<R: Runtime>(app: &AppHandle<R>, level: f32) -> Result<(), String> {
        app.try_state::<Brightness<R>>()
            .ok_or_else(|| "the brightness plugin did not register".to_owned())?
            .0
            .run_mobile_plugin::<()>("setBrightness", Level { level })
            .map_err(|e| format!("setBrightness did not land: {e}"))
    }
}

#[cfg(not(target_os = "android"))]
mod platform {
    use tauri::plugin::{Builder, TauriPlugin};
    use tauri::{AppHandle, Runtime};

    pub fn init<R: Runtime>() -> TauriPlugin<R> {
        Builder::new("brightness").build()
    }

    pub fn set<R: Runtime>(app: &AppHandle<R>, level: f32) -> Result<(), String> {
        let _ = (app, level);
        Err("this display's brightness belongs to the system, not to this window".to_owned())
    }
}

/// The plugin that carries the platform half of this control.
///
/// Registered on every platform so the builder reads the same everywhere; on
/// desktop it carries nothing.
pub fn init<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    platform::init()
}

/// Sets the display brightness, 0..1.
///
/// # Errors
///
/// Fails where the platform does not hand out this control, or if the call into
/// the activity does not land.
pub fn set<R: Runtime>(app: &tauri::AppHandle<R>, level: f32) -> Result<(), String> {
    platform::set(app, level)
}
