"""Patches the Tauri-generated Android project with native Kotlin host services,
in-app PackageInstaller.Session APK updater, and double-back navigation."""

from pathlib import Path


def main() -> None:
    src_dir = Path("android-host-src")
    android_main = Path("src-tauri/gen/android/app/src/main")
    manifest_path = android_main / "AndroidManifest.xml"
    xml_dir = android_main / "res/xml"
    xml_dir.mkdir(parents=True, exist_ok=True)

    # 1. Copy accessibility config XML
    (xml_dir / "accessibility_service_config.xml").write_text(
        (src_dir / "accessibility_service_config.xml").read_text(encoding="utf-8"),
        encoding="utf-8",
    )

    # 2. Patch AndroidManifest.xml with permissions & services
    manifest = manifest_path.read_text(encoding="utf-8")
    perms = [
        '    <uses-permission android:name="android.permission.REQUEST_INSTALL_PACKAGES" />',
        '    <uses-permission android:name="android.permission.FOREGROUND_SERVICE" />',
        '    <uses-permission android:name="android.permission.FOREGROUND_SERVICE_MEDIA_PROJECTION" />',
        '    <uses-permission android:name="android.permission.POST_NOTIFICATIONS" />',
    ]
    to_add = "\n".join(p for p in perms if p.strip() not in manifest)
    if to_add:
        manifest = manifest.replace("<application", f"{to_add}\n    <application", 1)

    services_xml = """
        <service
            android:name=".ScreenCaptureService"
            android:enabled="true"
            android:exported="false"
            android:foregroundServiceType="mediaProjection" />

        <service
            android:name=".RemoteControlAccessibilityService"
            android:exported="true"
            android:label="RemoteConnect Control Remoto"
            android:permission="android.permission.BIND_ACCESSIBILITY_SERVICE">
            <intent-filter>
                <action android:name="android.accessibilityservice.AccessibilityService" />
            </intent-filter>
            <meta-data
                android:name="android.accessibilityservice"
                android:resource="@xml/accessibility_service_config" />
        </service>
    </application>"""

    if "ScreenCaptureService" not in manifest:
        manifest = manifest.replace("</application>", services_xml, 1)

    manifest_path.write_text(manifest, encoding="utf-8")

    # 3. Locate MainActivity.kt package directory
    main_act = next((android_main / "java").rglob("MainActivity.kt"))
    pkg_dir = main_act.parent
    pkg_line = main_act.read_text(encoding="utf-8").splitlines()[0].strip()
    pkg_name = pkg_line.replace("package ", "").strip()

    # 4. Copy Kotlin services with resolved package name
    for kt_name in ("ScreenCaptureService.kt", "RemoteControlAccessibilityService.kt"):
        content = (src_dir / kt_name).read_text(encoding="utf-8")
        content = content.replace("__PACKAGE_NAME__", pkg_name)
        (pkg_dir / kt_name).write_text(content, encoding="utf-8")

    # 5. Write unified MainActivity.kt (AndroidUpdater + AndroidHost + double-back)
    main_kt = f"""{pkg_line}

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageInstaller
import android.media.projection.MediaProjectionManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.webkit.JavascriptInterface
import android.webkit.WebView
import android.widget.Toast
import androidx.activity.OnBackPressedCallback
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.net.HttpURLConnection
import java.net.URL
import kotlin.concurrent.thread

class MainActivity : TauriActivity() {{
  companion object {{
    private const val REQ_MEDIA_PROJECTION = 47822
  }}

  private var activeWebView: WebView? = null
  private var downloadedApk: File? = null
  private var installReceiverRegistered = false
  private var lastBackPressMs = 0L

  private val installAction: String
    get() = applicationContext.packageName + ".INSTALL_UPDATE_RESULT"

  private val installReceiver = object : BroadcastReceiver() {{
    override fun onReceive(context: Context, intent: Intent) {{
      val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
      when (status) {{
        PackageInstaller.STATUS_PENDING_USER_ACTION -> {{
          @Suppress("DEPRECATION")
          val confirmIntent = intent.getParcelableExtra<Intent>(Intent.EXTRA_INTENT)
          if (confirmIntent != null) {{
            confirmIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            startActivity(confirmIntent)
          }}
        }}
        PackageInstaller.STATUS_SUCCESS -> {{
          evalJs("window.__onAndroidUpdateError && window.__onAndroidUpdateError('Actualización instalada correctamente')")
        }}
        else -> {{
          val msg = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE) ?: "Error de instalación ($status)"
          val safe = msg.replace("'", "")
          evalJs("window.__onAndroidUpdateError && window.__onAndroidUpdateError('$safe')")
        }}
      }}
    }}
  }}

  override fun onWebViewCreate(webView: WebView) {{
    super.onWebViewCreate(webView)
    activeWebView = webView
    webView.addJavascriptInterface(AndroidUpdaterBridge(), "AndroidUpdater")
    webView.addJavascriptInterface(AndroidHostBridge(), "AndroidHost")

    if (!installReceiverRegistered) {{
      val filter = IntentFilter(installAction)
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {{
        registerReceiver(installReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
      }} else {{
        @Suppress("UnspecifiedRegisterReceiverFlag")
        registerReceiver(installReceiver, filter)
      }}
      installReceiverRegistered = true
    }}

    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {{
      override fun handleOnBackPressed() {{
        val wv = activeWebView
        if (wv == null) {{
          isEnabled = false
          onBackPressedDispatcher.onBackPressed()
          return
        }}
        wv.evaluateJavascript("Boolean(window.__remoteConnectInViewer)") {{ res ->
          if (res == "true") {{
            val now = System.currentTimeMillis()
            if (now - lastBackPressMs <= 1800L) {{
              lastBackPressMs = 0L
              wv.evaluateJavascript("window.__onAndroidDoubleBack && window.__onAndroidDoubleBack()", null)
            }} else {{
              lastBackPressMs = now
              Toast.makeText(
                this@MainActivity,
                "Presiona atrás nuevamente para volver al panel",
                Toast.LENGTH_SHORT
              ).show()
            }}
          }} else {{
            isEnabled = false
            onBackPressedDispatcher.onBackPressed()
            isEnabled = true
          }}
        }}
      }}
    }})
  }}

  @Suppress("DEPRECATION")
  override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {{
    super.onActivityResult(requestCode, resultCode, data)
    if (requestCode == REQ_MEDIA_PROJECTION) {{
      if (resultCode == Activity.RESULT_OK && data != null) {{
        val svcIntent = Intent(this, ScreenCaptureService::class.java).apply {{
          putExtra(ScreenCaptureService.EXTRA_RESULT_CODE, resultCode)
          putExtra(ScreenCaptureService.EXTRA_RESULT_DATA, data)
        }}
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {{
          startForegroundService(svcIntent)
        }} else {{
          startService(svcIntent)
        }}
        Toast.makeText(this, "Pantalla compartida activa (Host Android)", Toast.LENGTH_SHORT).show()
        evalJs("window.__onAndroidHostStatus && window.__onAndroidHostStatus(true)")
      }} else {{
        Toast.makeText(this, "Permiso de captura cancelado", Toast.LENGTH_SHORT).show()
        evalJs("window.__onAndroidHostStatus && window.__onAndroidHostStatus(false)")
      }}
    }}
  }}

  override fun onDestroy() {{
    if (installReceiverRegistered) {{
      try {{ unregisterReceiver(installReceiver) }} catch (_: Exception) {{}}
      installReceiverRegistered = false
    }}
    super.onDestroy()
  }}

  inner class AndroidHostBridge {{
    @JavascriptInterface
    fun requestScreenShare() {{
      runOnUiThread {{
        val mgr = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        @Suppress("DEPRECATION")
        startActivityForResult(mgr.createScreenCaptureIntent(), REQ_MEDIA_PROJECTION)
      }}
    }}

    @JavascriptInterface
    fun stopScreenShare() {{
      runOnUiThread {{
        val svcIntent = Intent(this@MainActivity, ScreenCaptureService::class.java).apply {{
          action = ScreenCaptureService.ACTION_STOP
        }}
        startService(svcIntent)
        evalJs("window.__onAndroidHostStatus && window.__onAndroidHostStatus(false)")
      }}
    }}

    @JavascriptInterface
    fun openAccessibilitySettings() {{
      runOnUiThread {{
        val intent = Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS).apply {{
          addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }}
        startActivity(intent)
        Toast.makeText(
          this@MainActivity,
          "Activa RemoteConnect Control Remoto en Servicios instalados",
          Toast.LENGTH_LONG
        ).show()
      }}
    }}

    @JavascriptInterface
    fun getHostStatusJson(): String {{
      val sharing = ScreenCaptureService.isRunning
      val a11y = RemoteControlAccessibilityService.instance != null
      return "{{\\"screenSharing\\":$sharing,\\"accessibilityEnabled\\":$a11y}}"
    }}
  }}

  inner class AndroidUpdaterBridge {{
    @JavascriptInterface
    fun downloadApk(urlStr: String) {{
      thread(start = true) {{
        try {{
          var currentUrl = urlStr
          var conn: HttpURLConnection? = null
          for (i in 0..5) {{
            conn = (URL(currentUrl).openConnection() as HttpURLConnection).apply {{
              instanceFollowRedirects = false
              connectTimeout = 15000
              readTimeout = 30000
              setRequestProperty("User-Agent", "RemoteConnect-Android")
            }}
            val code = conn.responseCode
            if (code in 300..399) {{
              val loc = conn.getHeaderField("Location") ?: break
              currentUrl = URL(URL(currentUrl), loc).toString()
              conn.disconnect()
            }} else {{
              break
            }}
          }}
          if (conn == null || conn.responseCode !in 200..299) {{
            val status = conn?.responseCode ?: -1
            evalJs("window.__onAndroidUpdateError && window.__onAndroidUpdateError('HTTP $status al descargar APK')")
            return@thread
          }}
          val total = conn.contentLengthLong
          val outFile = File(cacheDir, "RemoteConnect-update.apk")
          conn.inputStream.use {{ input ->
            FileOutputStream(outFile).use {{ output ->
              val buf = ByteArray(16384)
              var copied = 0L
              var lastPct = -1
              while (true) {{
                val n = input.read(buf)
                if (n <= 0) break
                output.write(buf, 0, n)
                copied += n
                if (total > 0) {{
                  val pct = ((copied * 100) / total).toInt().coerceIn(0, 100)
                  if (pct != lastPct) {{
                    lastPct = pct
                    evalJs("window.__onAndroidUpdateProgress && window.__onAndroidUpdateProgress($pct)")
                  }}
                }}
              }}
            }}
          }}
          conn.disconnect()
          if (outFile.length() < 100_000) {{
            evalJs("window.__onAndroidUpdateError && window.__onAndroidUpdateError('Descarga incompleta del APK')")
            return@thread
          }}
          downloadedApk = outFile
          evalJs("window.__onAndroidUpdateReady && window.__onAndroidUpdateReady()")
        }} catch (e: Exception) {{
          val safe = (e.message ?: "Error de red").replace("'", "")
          evalJs("window.__onAndroidUpdateError && window.__onAndroidUpdateError('$safe')")
        }}
      }}
    }}

    @JavascriptInterface
    fun installApk() {{
      val file = downloadedApk ?: File(cacheDir, "RemoteConnect-update.apk")
      if (!file.exists() || file.length() < 100_000) {{
        evalJs("window.__onAndroidUpdateError && window.__onAndroidUpdateError('Archivo APK no encontrado o incompleto')")
        return
      }}
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && !packageManager.canRequestPackageInstalls()) {{
        runOnUiThread {{
          val settingsIntent = Intent(
            Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
            Uri.parse("package:" + applicationContext.packageName)
          ).apply {{
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
          }}
          startActivity(settingsIntent)
        }}
        evalJs("window.__onAndroidUpdateError && window.__onAndroidUpdateError('Activa Permitir desde esta fuente y vuelve a pulsar Instalar APK ahora')")
        return
      }}
      thread(start = true) {{
        try {{
          val installer = packageManager.packageInstaller
          val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
          params.setAppPackageName(applicationContext.packageName)
          val sessionId = installer.createSession(params)
          installer.openSession(sessionId).use {{ session ->
            session.openWrite("RemoteConnect-update.apk", 0, file.length()).use {{ out ->
              FileInputStream(file).use {{ input ->
                input.copyTo(out)
              }}
              session.fsync(out)
            }}
            val callbackIntent = Intent(installAction).apply {{
              setPackage(applicationContext.packageName)
            }}
            val flags = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {{
              PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE
            }} else {{
              PendingIntent.FLAG_UPDATE_CURRENT
            }}
            val pendingIntent = PendingIntent.getBroadcast(
              this@MainActivity,
              sessionId,
              callbackIntent,
              flags
            )
            session.commit(pendingIntent.intentSender)
          }}
        }} catch (e: Exception) {{
          val safe = (e.message ?: "Error al iniciar PackageInstaller").replace("'", "")
          evalJs("window.__onAndroidUpdateError && window.__onAndroidUpdateError('$safe')")
        }}
      }}
    }}
  }}

  private fun evalJs(script: String) {{
    runOnUiThread {{
      activeWebView?.evaluateJavascript(script, null)
    }}
  }}
}}
"""
    main_act.write_text(main_kt, encoding="utf-8")
    print(f"Patched Android project cleanly for package {pkg_name}")


if __name__ == "__main__":
    main()
