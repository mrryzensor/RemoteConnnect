package __PACKAGE_NAME__

import android.app.Activity
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Bundle
import android.os.IBinder
import android.os.SystemClock
import org.json.JSONObject
import java.io.BufferedReader
import java.io.DataOutputStream
import java.io.InputStreamReader
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

class ScreenCaptureService : Service() {
  companion object {
    const val EXTRA_RESULT_CODE = "result_code"
    const val EXTRA_RESULT_DATA = "result_data"
    const val ACTION_STOP = "stop_screen_capture"
    private const val CHANNEL_ID = "remoteconnect_screen_share"
    private const val NOTIF_ID = 47821

    @Volatile
    var isRunning = false
  }

  private var mediaProjection: MediaProjection? = null
  private var virtualDisplay: VirtualDisplay? = null
  private var encoder: MediaCodec? = null
  private val active = AtomicBoolean(false)

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent?.action == ACTION_STOP) {
      stopSelf()
      return START_NOT_STICKY
    }

    val resultCode = intent?.getIntExtra(EXTRA_RESULT_CODE, Activity.RESULT_CANCELED)
      ?: Activity.RESULT_CANCELED
    @Suppress("DEPRECATION")
    val resultData = intent?.getParcelableExtra<Intent>(EXTRA_RESULT_DATA)
    if (resultCode != Activity.RESULT_OK || resultData == null) {
      stopSelf()
      return START_NOT_STICKY
    }

    startForegroundNotif()

    val projMgr = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
    val projection = projMgr.getMediaProjection(resultCode, resultData)
    if (projection == null) {
      stopSelf()
      return START_NOT_STICKY
    }
    mediaProjection = projection
    isRunning = true
    active.set(true)

    startCapturePipeline(projection)
    return START_NOT_STICKY
  }

  private fun startForegroundNotif() {
    val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      val ch = NotificationChannel(
        CHANNEL_ID,
        "RemoteConnect Pantalla Compartida",
        NotificationManager.IMPORTANCE_LOW
      )
      nm.createNotificationChannel(ch)
    }
    val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      Notification.Builder(this, CHANNEL_ID)
    } else {
      @Suppress("DEPRECATION")
      Notification.Builder(this)
    }
    val notif = builder
      .setContentTitle("RemoteConnect activo")
      .setContentText("Compartiendo pantalla de este dispositivo Android")
      .setSmallIcon(android.R.drawable.ic_menu_view)
      .setOngoing(true)
      .build()

    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
      startForeground(NOTIF_ID, notif, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION)
    } else {
      startForeground(NOTIF_ID, notif)
    }
  }

  private fun startCapturePipeline(projection: MediaProjection) {
    val dm = resources.displayMetrics
    val rawW = dm.widthPixels.coerceAtLeast(360)
    val rawH = dm.heightPixels.coerceAtLeast(360)
    val maxDim = 1080f
    val scale = if (maxOf(rawW, rawH) > maxDim) maxDim / maxOf(rawW, rawH) else 1f
    val encW = ((rawW * scale).toInt() / 16).coerceAtLeast(16) * 16
    val encH = ((rawH * scale).toInt() / 16).coerceAtLeast(16) * 16
    val dpi = dm.densityDpi

    try {
      val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, encW, encH).apply {
        setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
        setInteger(MediaFormat.KEY_BIT_RATE, 2_000_000)
        setInteger(MediaFormat.KEY_FRAME_RATE, 30)
        setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 2)
      }

      val codec = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
      codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
      val surface = codec.createInputSurface()
      codec.start()
      encoder = codec

      virtualDisplay = projection.createVirtualDisplay(
        "RemoteConnectDisplay",
        encW,
        encH,
        dpi,
        DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
        surface,
        null,
        null
      )

      thread(start = true) {
        streamLoop(codec, encW, encH)
      }
    } catch (_: Exception) {
      stopSelf()
    }
  }

  private fun streamLoop(codec: MediaCodec, width: Int, height: Int) {
    var socket: Socket? = null
    var outStream: DataOutputStream? = null
    var spsPps: ByteArray? = null
    val startMs = SystemClock.elapsedRealtime()
    val bufInfo = MediaCodec.BufferInfo()

    while (active.get()) {
      // Connect/reconnect to Rust loopback bridge on 127.0.0.1:47821
      if (socket == null || socket.isClosed || !socket.isConnected) {
        try {
          val s = Socket("127.0.0.1", 47821)
          s.tcpNoDelay = true
          socket = s
          outStream = DataOutputStream(s.getOutputStream())
          // Spawn reader thread for Rust -> Kotlin commands (input + keyframe requests)
          val readerSocket = s
          thread(start = true) {
            try {
              val br = BufferedReader(InputStreamReader(readerSocket.getInputStream()))
              while (active.get() && !readerSocket.isClosed) {
                val line = br.readLine() ?: break
                val json = JSONObject(line)
                if (json.optString("cmd") == "request_keyframe") {
                  try {
                    val b = Bundle()
                    b.putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0)
                    codec.setParameters(b)
                  } catch (_: Exception) {}
                } else {
                  RemoteControlAccessibilityService.instance?.handleRemoteCommand(json)
                }
              }
            } catch (_: Exception) {}
          }
        } catch (_: Exception) {
          try { Thread.sleep(400) } catch (_: Exception) {}
          continue
        }
      }

      try {
        val outIdx = codec.dequeueOutputBuffer(bufInfo, 15_000)
        if (outIdx >= 0) {
          val outBuf = codec.getOutputBuffer(outIdx)
          if (outBuf != null && bufInfo.size > 0) {
            outBuf.position(bufInfo.offset)
            outBuf.limit(bufInfo.offset + bufInfo.size)
            val data = ByteArray(bufInfo.size)
            outBuf.get(data)

            val isConfig = (bufInfo.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0
            if (isConfig) {
              spsPps = data
            } else {
              val isKey = (bufInfo.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME) != 0
              val h264Payload = if (isKey && spsPps != null) {
                spsPps!! + data
              } else {
                data
              }
              val tsMs = (SystemClock.elapsedRealtime() - startMs).toInt()
              val payloadLen = 1 + 4 + 4 + 4 + h264Payload.size
              val out = outStream
              if (out != null) {
                synchronized(out) {
                  out.writeInt(payloadLen)
                  out.writeByte(if (isKey) 1 else 0)
                  out.writeInt(width)
                  out.writeInt(height)
                  out.writeInt(tsMs)
                  out.write(h264Payload)
                  out.flush()
                }
              }
            }
          }
          codec.releaseOutputBuffer(outIdx, false)
        }
      } catch (_: Exception) {
        try { socket?.close() } catch (_: Exception) {}
        socket = null
        outStream = null
      }
    }

    try { socket?.close() } catch (_: Exception) {}
  }

  override fun onDestroy() {
    active.set(false)
    isRunning = false
    try { virtualDisplay?.release() } catch (_: Exception) {}
    try { encoder?.stop(); encoder?.release() } catch (_: Exception) {}
    try { mediaProjection?.stop() } catch (_: Exception) {}
    super.onDestroy()
  }
}
