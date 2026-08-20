package dev.lightgap.desktop

import android.os.Bundle
import android.view.WindowManager
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat

/**
 * Two window flags matter here, and neither is cosmetic.
 *
 * On this app the display is not showing content, it *is* the transmitter. If
 * Android dims the screen or lets it sleep partway through a transfer, the
 * optical link dies mid-file. `FLAG_KEEP_SCREEN_ON` is set in addition to the
 * manifest's `keepScreenOn` because the manifest attribute applies to the
 * activity's root view and the flag applies to the window; belt and braces is
 * cheap here and the failure it prevents is not.
 *
 * Screen brightness starts at maximum for the same reason. The optical
 * channel's signal-to-noise ratio is literally the contrast the peer's camera
 * sees, so a display at 30% brightness is a link operating well below what the
 * hardware can do. The user's own brightness preference is restored by the
 * system when the activity goes away, since the override is per-window.
 *
 * Maximum is the right *default* but not always the right value, which is why
 * [setScreenBrightness] exists. Held close to a camera, a display at full
 * output blooms: the sensor clips, white bleeds across the module boundaries
 * and the code stops resolving. Turning the transmitter down is then the fix,
 * which is the opposite of what anyone expects.
 */
class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)

    window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    setScreenBrightness(WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_FULL)

    hideSystemBars()

    // Whatever is left after the bars are gone — a camera cutout, a gesture
    // handle — still has to be kept clear of. The web side cannot work this out
    // for itself: Android's WebView does not report `env(safe-area-inset-*)` to
    // the page the way Chrome does, so a CSS-only attempt silently resolves to
    // zero and looks exactly like doing nothing.
    val content = findViewById<android.view.View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      val bars = insets.getInsets(
        WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
      )
      view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
      WindowInsetsCompat.CONSUMED
    }
  }

  /**
   * Takes the status and navigation bars off the screen.
   *
   * Not a preference. Every pixel of this display is transmitter: a status bar
   * across the top is area the code cannot use, and the clock and battery it
   * draws are the peer's camera's problem, not decoration. They stay reachable
   * by swiping, and go away again on their own.
   */
  private fun hideSystemBars() {
    WindowInsetsControllerCompat(window, window.decorView).apply {
      hide(WindowInsetsCompat.Type.systemBars())
      systemBarsBehavior =
        WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
    }
  }

  /**
   * Android puts the bars back whenever focus returns — after a notification, a
   * permission dialog, or the recents switcher. Without this the first
   * interruption of a transfer leaves them on screen for the rest of it.
   */
  override fun onWindowFocusChanged(hasFocus: Boolean) {
    super.onWindowFocusChanged(hasFocus)
    if (hasFocus) {
      hideSystemBars()
    }
  }

  /**
   * Sets the window's brightness override, 0..1.
   *
   * Called over JNI from the Rust side, which runs on whatever thread the
   * command handler happens to be on. Window attributes may only be touched
   * from the UI thread, so the hop happens here rather than at the call site:
   * the caller has no way to know which thread it is on, and this class does.
   */
  fun setScreenBrightness(level: Float) {
    val clamped = level.coerceIn(0.05f, 1.0f)
    runOnUiThread {
      val params = window.attributes
      params.screenBrightness = clamped
      window.attributes = params
    }
  }
}
