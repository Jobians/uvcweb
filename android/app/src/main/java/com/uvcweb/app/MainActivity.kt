package com.uvcweb.app

import android.Manifest
import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.hardware.usb.UsbManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import java.io.File

/**
 * Main screen: choose what to serve, press Start. The capture itself runs in [CaptureService].
 *
 * Start = (1) camera + microphone permission (Android insists on them for USB video / audio devices),
 *         (2) USB permission for the card, (3) start the service.
 */
class MainActivity : Activity() {

  private lateinit var usb: UsbManager
  private val handler = Handler(Looper.getMainLooper())

  private lateinit var deviceText: TextView
  private lateinit var webCheck: CheckBox
  private lateinit var webPortEdit: EditText
  private lateinit var rtspCheck: CheckBox
  private lateinit var rtspPortEdit: EditText
  private lateinit var lanCheck: CheckBox
  private lateinit var audioCheck: CheckBox
  private lateinit var widthEdit: EditText
  private lateinit var heightEdit: EditText
  private lateinit var fpsEdit: EditText
  private lateinit var startButton: Button
  private lateinit var viewerButton: Button
  private lateinit var statusText: TextView
  private lateinit var urlText: TextView
  private lateinit var logText: TextView
  private lateinit var logScroll: ScrollView

  private var pendingDeviceName: String? = null
  private var lastLog = ""

  private val usbPermissionReceiver = object : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
      if (intent.action != ACTION_USB_PERMISSION) return
      val granted = intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)
      val name = pendingDeviceName
      if (granted && name != null) {
        startCapture(name)
      } else {
        toast("USB permission was refused")
      }
    }
  }

  private val ticker = object : Runnable {
    override fun run() {
      refreshStatus()
      handler.postDelayed(this, 1000)
    }
  }

  // ------------------------------------------------------------------ life cycle

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    setContentView(R.layout.activity_main)
    usb = getSystemService(Context.USB_SERVICE) as UsbManager

    deviceText = findViewById(R.id.deviceText)
    webCheck = findViewById(R.id.webCheck)
    webPortEdit = findViewById(R.id.webPortEdit)
    rtspCheck = findViewById(R.id.rtspCheck)
    rtspPortEdit = findViewById(R.id.rtspPortEdit)
    lanCheck = findViewById(R.id.lanCheck)
    audioCheck = findViewById(R.id.audioCheck)
    widthEdit = findViewById(R.id.widthEdit)
    heightEdit = findViewById(R.id.heightEdit)
    fpsEdit = findViewById(R.id.fpsEdit)
    startButton = findViewById(R.id.startButton)
    viewerButton = findViewById(R.id.viewerButton)
    statusText = findViewById(R.id.statusText)
    urlText = findViewById(R.id.urlText)
    logText = findViewById(R.id.logText)
    logScroll = findViewById(R.id.logScroll)

    showSettings(Settings.load(this))

    startButton.setOnClickListener {
      onStartStopClicked()
    }
    viewerButton.setOnClickListener {
      startActivity(Intent(this, ViewerActivity::class.java))
    }
    registerPrivateReceiver(usbPermissionReceiver, IntentFilter(ACTION_USB_PERMISSION))
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent) // e.g. the card was plugged in and this app was offered
  }

  override fun onResume() {
    super.onResume()
    handler.post(ticker)
  }

  override fun onPause() {
    handler.removeCallbacks(ticker)
    readSettings().save(this)
    super.onPause()
  }

  override fun onDestroy() {
    try {
      unregisterReceiver(usbPermissionReceiver)
    } catch (e: Exception) {
      // not registered
    }
    super.onDestroy()
  }

  // ------------------------------------------------------------------ settings <-> screen

  private fun showSettings(s: Settings) {
    webCheck.isChecked = s.web
    webPortEdit.setText(s.webPort.toString())
    rtspCheck.isChecked = s.rtsp
    rtspPortEdit.setText(s.rtspPort.toString())
    lanCheck.isChecked = s.lan
    audioCheck.isChecked = s.audio
    widthEdit.setText(s.width.toString())
    heightEdit.setText(s.height.toString())
    fpsEdit.setText(s.fps.toString())
  }

  private fun readSettings(): Settings {
    val d = Settings()
    return Settings(
      web = webCheck.isChecked,
      webPort = webPortEdit.text.toString().toIntOrNull() ?: d.webPort,
      rtsp = rtspCheck.isChecked,
      rtspPort = rtspPortEdit.text.toString().toIntOrNull() ?: d.rtspPort,
      lan = lanCheck.isChecked,
      audio = audioCheck.isChecked,
      width = widthEdit.text.toString().toIntOrNull() ?: 0,
      height = heightEdit.text.toString().toIntOrNull() ?: 0,
      fps = fpsEdit.text.toString().toIntOrNull() ?: 0,
    )
  }

  // ------------------------------------------------------------------ start / stop

  private fun onStartStopClicked() {
    if (CaptureService.state != CaptureService.State.STOPPED) {
      startService(Intent(this, CaptureService::class.java).setAction(CaptureService.ACTION_STOP))
      return
    }
    val settings = readSettings()
    if (!settings.web && !settings.rtsp) {
      toast("Switch on the web viewer or the RTSP server")
      return
    }
    settings.save(this)

    val missing = missingRuntimePermissions()
    if (missing.isNotEmpty()) {
      requestPermissions(missing.toTypedArray(), REQUEST_PERMISSIONS)
      return // continues in onRequestPermissionsResult
    }
    continueWithUsb()
  }

  private fun missingRuntimePermissions(): List<String> {
    val wanted = ArrayList<String>()
    wanted.add(Manifest.permission.CAMERA)
    wanted.add(Manifest.permission.RECORD_AUDIO)
    if (Build.VERSION.SDK_INT >= 33) {
      wanted.add(Manifest.permission.POST_NOTIFICATIONS)
    }
    return wanted.filter {
      checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED
    }
  }

  override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
    super.onRequestPermissionsResult(requestCode, permissions, grantResults)
    if (requestCode != REQUEST_PERMISSIONS) return
    val cameraOk = checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED
    val micOk = checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
    if (!cameraOk || !micOk) {
      // Try anyway: some devices don't insist. If USB permission fails the message below tells why.
      toast("Camera and microphone permission are needed by Android to open a USB capture card")
    }
    continueWithUsb()
  }

  private fun continueWithUsb() {
    val device = Util.findCaptureDevice(usb)
    if (device == null) {
      toast("No USB capture card found - plug it in")
      return
    }
    if (usb.hasPermission(device)) {
      startCapture(device.deviceName)
      return
    }
    pendingDeviceName = device.deviceName
    val intent = Intent(ACTION_USB_PERMISSION).setPackage(packageName)
    // Android fills in extras, so the PendingIntent must be mutable on Android 12+.
    val flags = if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0
    val pending = PendingIntent.getBroadcast(this, 0, intent, flags)
    try {
      usb.requestPermission(device, pending)
    } catch (e: SecurityException) {
      toast("Android refused: grant the Camera and Microphone permission in the app settings, then try again")
    }
  }

  private fun startCapture(deviceName: String) {
    val intent = Intent(this, CaptureService::class.java).putExtra(CaptureService.EXTRA_DEVICE_NAME, deviceName)
    startForegroundService(intent)
  }

  // ------------------------------------------------------------------ status display

  private fun refreshStatus() {
    val device = Util.findCaptureDevice(usb)
    deviceText.text = if (device != null) "Capture card: " + Util.describe(device) else "No capture card plugged in"

    val state = CaptureService.state
    startButton.text = if (state == CaptureService.State.STOPPED) "Start" else "Stop"
    statusText.text = when (state) {
      CaptureService.State.STOPPED -> "Stopped" + messageSuffix()
      CaptureService.State.STARTING -> "Starting..."
      CaptureService.State.RUNNING -> "Running"
    }

    val running = state == CaptureService.State.RUNNING
    val settings = readSettings()
    viewerButton.isEnabled = running && settings.web
    urlText.text = if (running) buildUrlText(settings) else ""

    val log = Util.tail(File(filesDir, CaptureService.LOG_NAME))
    if (log != lastLog) {
      lastLog = log
      logText.text = log
      logScroll.post {
        logScroll.fullScroll(ScrollView.FOCUS_DOWN)
      }
    }
  }

  private fun messageSuffix(): String {
    val m = CaptureService.message
    return if (m.isEmpty() || m == "Stopped") "" else ": $m"
  }

  private fun buildUrlText(s: Settings): String {
    val sb = StringBuilder()
    if (s.web) {
      sb.append("http://127.0.0.1:").append(s.webPort).append("/\n")
    }
    if (s.rtsp) {
      sb.append("rtsp://127.0.0.1:").append(s.rtspPort).append("/live\n")
    }
    if (s.lan) {
      for (ip in Util.localIpv4Addresses()) {
        if (s.web) sb.append("http://").append(ip).append(':').append(s.webPort).append("/\n")
        if (s.rtsp) sb.append("rtsp://").append(ip).append(':').append(s.rtspPort).append("/live\n")
      }
    }
    return sb.toString().trimEnd()
  }

  private fun toast(text: String) {
    Toast.makeText(this, text, Toast.LENGTH_LONG).show()
  }

  companion object {
    private const val ACTION_USB_PERMISSION = "com.uvcweb.app.USB_PERMISSION"
    private const val REQUEST_PERMISSIONS = 1
  }
}