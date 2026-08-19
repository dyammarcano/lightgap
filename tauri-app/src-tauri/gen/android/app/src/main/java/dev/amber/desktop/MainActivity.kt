package dev.amber.desktop

import android.os.Bundle
import android.view.WindowManager
import androidx.activity.enableEdgeToEdge

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
 * Screen brightness is raised to maximum for the same reason. The optical
 * channel's signal-to-noise ratio is literally the contrast the peer's camera
 * sees, so a display at 30% brightness is a link operating 70% below what the
 * hardware can do. The user's brightness preference is restored by the system
 * when the activity goes away, since the override is per-window.
 */
class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)

    window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)

    val params = window.attributes
    params.screenBrightness = WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_FULL
    window.attributes = params
  }
}
