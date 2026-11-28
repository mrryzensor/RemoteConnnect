package __PACKAGE_NAME__

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.graphics.Path
import android.view.accessibility.AccessibilityEvent
import org.json.JSONObject
import kotlin.math.hypot

class RemoteControlAccessibilityService : AccessibilityService() {
  companion object {
    @Volatile
    var instance: RemoteControlAccessibilityService? = null
  }

  private var isLeftDown = false
  private var downX = 0f
  private var downY = 0f
  private var lastX = 0f
  private var lastY = 0f

  override fun onServiceConnected() {
    super.onServiceConnected()
    instance = this
  }

  override fun onAccessibilityEvent(event: AccessibilityEvent?) {
    // No-op: service is used strictly for remote gesture injection
  }

  override fun onInterrupt() {
    // No-op
  }

  override fun onDestroy() {
    if (instance === this) {
      instance = null
    }
    super.onDestroy()
  }

  fun handleRemoteCommand(json: JSONObject) {
    val cmd = json.optString("cmd")
    val dm = resources.displayMetrics
    val screenW = dm.widthPixels.toFloat().coerceAtLeast(1f)
    val screenH = dm.heightPixels.toFloat().coerceAtLeast(1f)

    when (cmd) {
      "mouse_move" -> {
        val nx = json.optDouble("x", 0.5).toFloat().coerceIn(0f, 1f)
        val ny = json.optDouble("y", 0.5).toFloat().coerceIn(0f, 1f)
        lastX = nx * screenW
        lastY = ny * screenH
      }
      "mouse_button" -> {
        val btn = json.optString("button", "left")
        val pressed = json.optBoolean("pressed", false)
        val nx = json.optDouble("x", 0.5).toFloat().coerceIn(0f, 1f)
        val ny = json.optDouble("y", 0.5).toFloat().coerceIn(0f, 1f)
        val px = nx * screenW
        val py = ny * screenH

        if (btn == "right") {
          if (!pressed) {
            performGlobalAction(GLOBAL_ACTION_BACK)
          }
          return
        }
        if (btn == "middle") {
          if (!pressed) {
            performGlobalAction(GLOBAL_ACTION_HOME)
          }
          return
        }

        if (pressed) {
          isLeftDown = true
          downX = px
          downY = py
          lastX = px
          lastY = py
        } else if (isLeftDown) {
          isLeftDown = false
          val dist = hypot((px - downX).toDouble(), (py - downY).toDouble())
          if (dist < 14.0) {
            performTap(px, py)
          } else {
            performSwipe(downX, downY, px, py, 220L)
          }
        }
      }
      "mouse_scroll" -> {
        val dy = json.optInt("dy", 0)
        if (dy != 0) {
          val cx = if (lastX > 1f) lastX else screenW * 0.5f
          val cy = if (lastY > 1f) lastY else screenH * 0.5f
          val delta = if (dy > 0) -screenH * 0.22f else screenH * 0.22f
          val toY = (cy + delta).coerceIn(40f, screenH - 40f)
          performSwipe(cx, cy, cx, toY, 180L)
        }
      }
      "key_event" -> {
        val pressed = json.optBoolean("pressed", false)
        if (!pressed) return
        val code = json.optString("code", "")
        val key = json.optString("key", "")
        when {
          code == "Escape" || key == "Escape" || code == "BrowserBack" -> {
            performGlobalAction(GLOBAL_ACTION_BACK)
          }
          code == "Home" || code == "MetaLeft" || key == "Meta" -> {
            performGlobalAction(GLOBAL_ACTION_HOME)
          }
          code == "Tab" && key == "Recents" -> {
            performGlobalAction(GLOBAL_ACTION_RECENTS)
          }
        }
      }
    }
  }

  private fun performTap(x: Float, y: Float) {
    val path = Path().apply { moveTo(x, y) }
    val stroke = GestureDescription.StrokeDescription(path, 0L, 40L)
    dispatchGesture(GestureDescription.Builder().addStroke(stroke).build(), null, null)
  }

  private fun performSwipe(x0: Float, y0: Float, x1: Float, y1: Float, durationMs: Long) {
    val path = Path().apply {
      moveTo(x0, y0)
      lineTo(x1, y1)
    }
    val stroke = GestureDescription.StrokeDescription(path, 0L, durationMs)
    dispatchGesture(GestureDescription.Builder().addStroke(stroke).build(), null, null)
  }
}
