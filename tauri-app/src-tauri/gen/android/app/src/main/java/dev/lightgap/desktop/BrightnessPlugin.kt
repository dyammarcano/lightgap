package dev.lightgap.desktop

import android.app.Activity
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin

@InvokeArg
class SetBrightnessArgs {
  var level: Float = 1.0f
}

/**
 * Display brightness, which on this application is transmit power.
 *
 * The optical channel's signal-to-noise ratio is the contrast the peer's camera
 * resolves, and that is set by how much light this display puts out. Maximum is
 * the right default but not always the right value: held close to a camera, a
 * display at full output blooms — the sensor clips, white bleeds across module
 * boundaries and the code stops resolving. Turning the transmitter down is then
 * the fix, which is the opposite of what anyone expects.
 *
 * Reached through Tauri's mobile plugin API rather than by calling JNI from
 * Rust. An earlier attempt did the latter through `ndk-context` and aborted the
 * whole process on the first invoke: nothing in a Tauri app initialises that
 * crate's statics, and the panic crossed an `extern "C"` boundary that cannot
 * unwind.
 */
@TauriPlugin
class BrightnessPlugin(private val activity: Activity) : Plugin(activity) {
  @Command
  fun setBrightness(invoke: Invoke) {
    val args = invoke.parseArgs(SetBrightnessArgs::class.java)
    val level = args.level.coerceIn(0.05f, 1.0f)

    // Window attributes may only be touched from the UI thread, and the caller
    // has no way to know which thread it arrived on. This class does.
    activity.runOnUiThread {
      val params = activity.window.attributes
      params.screenBrightness = level
      activity.window.attributes = params
    }

    invoke.resolve()
  }
}
