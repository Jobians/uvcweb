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
import android.text.Editable
import android.text.TextWatcher
import android.view.View
import android.widget.AdapterView
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.ScrollView
import android.widget.Spinner
import android.widget.TextView
import android.widget.Toast
import java.io.File
import java.util.Locale

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
  private lateinit var mdnsCheck: CheckBox
  private lateinit var mdnsNameEdit: EditText
  private lateinit var autoReconnectCheck: CheckBox
  private lateinit var audioCheck: CheckBox
  private lateinit var resolutionLabel: TextView
  private lateinit var resolutionSpinner: Spinner
  private lateinit var fpsSpinner: Spinner
  private lateinit var modeSummary: TextView
  private lateinit var customModeRow: View
  private lateinit var widthEdit: EditText
  private lateinit var heightEdit: EditText
  private lateinit var fpsEdit: EditText
  private lateinit var detectModesButton: Button
  private lateinit var startButton: Button
  private lateinit var viewerButton: Button
  private lateinit var recordButton: Button

  /** True while a recording is being started or stopped, so a second tap waits. */
  @Volatile private var recordingBusy = false
  private lateinit var statusText: TextView
  private lateinit var urlText: TextView
  private lateinit var logText: TextView
  private lateinit var logScroll: ScrollView
  private lateinit var clearLogButton: Button

  /**
   * One resolution the card lists. A width of 0 is the "let the card choose" entry, and
   * [custom] is the hand-typed escape hatch for a card whose list is wrong.
   */
  private class Resolution(
    val width: Int,
    val height: Int,
    val custom: Boolean = false,
    /** The card named this size as the one it would start on by itself. */
    val isDefault: Boolean = false,
  ) {
    val label: String
      get() {
        // Locale.US, so a phone set to a language that groups digits does not turn
        // 1920 into "1.920".
        val size = "%d x %d".format(Locale.US, width, height)
        return when {
          custom -> "Custom (type a size and rate)"
          width == 0 -> "Let the card choose (its own default)"
          isDefault -> "$size  -  the card's default"
          else -> size
        }
      }
  }

  /**
   * One frame rate the card lists, in whole frames a second. A rate of 0 is the
   * placeholder shown before the card has been asked, and means "whatever the engine
   * falls back to" - which is why it is not the same as an offered rate.
   */
  private class Rate(val fps: Int, val isDefault: Boolean = false) {
    val label: String
      get() = when {
        fps == 0 -> "not read from the card yet"
        isDefault -> "%d fps  -  the card's default".format(Locale.US, fps)
        else -> "%d fps".format(Locale.US, fps)
      }
  }

  /** One size-and-rate the card listed, kept so an unlisted pair can be recognised. */
  private class Detected(val width: Int, val height: Int, val fps: Int, val isDefault: Boolean)

  /** What the two spinners are showing, in the same order. */
  private var resolutionList: List<Resolution> = emptyList()
  private var rateList: List<Rate> = emptyList()
  private var detectedList: List<Detected> = emptyList()

  /** True while a probe is in flight, so the card is not opened twice at once. */
  @Volatile private var modesProbing = false

  /** How many times a Start tap has waited for a probe to finish. */
  private var startWaits = 0

  /** The card and engine state the last probe was for, so it is not repeated. */
  private var probedFor: String? = null

  private var pendingDeviceName: String? = null

  /** True when the pending USB permission was asked for to read the mode list, not to stream. */
  private var probeAfterPermission = false

  private var lastLog = ""

  private val usbPermissionReceiver = object : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
      if (intent.action != ACTION_USB_PERMISSION) return
      val granted = intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)
      val name = pendingDeviceName
      if (!granted) {
        toast("USB permission was refused")
        return
      }
      if (probeAfterPermission) {
        // The permission was asked for to read the card's mode list, not to stream.
        probeAfterPermission = false
        probedFor = null
        probeModes(asked = true)
        return
      }
      if (name != null) startCapture(name)
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
    mdnsCheck = findViewById(R.id.mdnsCheck)
    mdnsNameEdit = findViewById(R.id.mdnsNameEdit)
    autoReconnectCheck = findViewById(R.id.autoReconnectCheck)
    audioCheck = findViewById(R.id.audioCheck)
    resolutionLabel = findViewById(R.id.resolutionLabel)
    resolutionSpinner = findViewById(R.id.resolutionSpinner)
    fpsSpinner = findViewById(R.id.fpsSpinner)
    modeSummary = findViewById(R.id.modeSummary)
    customModeRow = findViewById(R.id.customModeRow)
    widthEdit = findViewById(R.id.widthEdit)
    heightEdit = findViewById(R.id.heightEdit)
    fpsEdit = findViewById(R.id.fpsEdit)
    detectModesButton = findViewById(R.id.detectModesButton)
    startButton = findViewById(R.id.startButton)
    viewerButton = findViewById(R.id.viewerButton)
    recordButton = findViewById(R.id.recordButton)
    statusText = findViewById(R.id.statusText)
    urlText = findViewById(R.id.urlText)
    logText = findViewById(R.id.logText)
    logScroll = findViewById(R.id.logScroll)
    clearLogButton = findViewById(R.id.clearLogButton)

    showSettings(Settings.load(this))

    val onPicked = object : AdapterView.OnItemSelectedListener {
      override fun onItemSelected(parent: AdapterView<*>?, view: View?, position: Int, id: Long) {
        showSelectedMode()
      }

      override fun onNothingSelected(parent: AdapterView<*>?) {}
    }
    resolutionSpinner.onItemSelectedListener = onPicked
    fpsSpinner.onItemSelectedListener = onPicked

    // The summary line has to follow a hand-typed mode as it is typed, or it would
    // still be describing the last one while the fields show something else.
    val typed = object : TextWatcher {
      override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}

      override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {}

      override fun afterTextChanged(s: Editable?) {
        if (customModeRow.visibility == View.VISIBLE) showSelectedMode()
      }
    }
    widthEdit.addTextChangedListener(typed)
    heightEdit.addTextChangedListener(typed)
    fpsEdit.addTextChangedListener(typed)
    detectModesButton.setOnClickListener {
      probeModes(asked = true)
    }
    mdnsCheck.setOnCheckedChangeListener {
      _, checked ->
      mdnsNameEdit.isEnabled = checked
    }
    clearLogButton.setOnClickListener {
      Util.clearLog(this)
      lastLog = ""
      logText.text = ""
    }
    startButton.setOnClickListener {
      onStartStopClicked()
    }
    viewerButton.setOnClickListener {
      startActivity(Intent(this, ViewerActivity::class.java))
    }
    recordButton.setOnClickListener {
      onRecordClicked()
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
    // The card is asked what it can do as soon as it is there to be asked, so the list is
    // there before Start is pressed. It is only possible with USB permission in hand and
    // while nothing is streaming; the button covers the rest.
    val device = Util.findCaptureDevice(usb)
    val forWhat = device?.deviceName + "/" + CaptureService.state
    if (device != null && forWhat != probedFor) {
      probedFor = forWhat
      probeModes(asked = false)
    }
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
    mdnsCheck.isChecked = s.mdns
    mdnsNameEdit.setText(s.mdnsName)
    mdnsNameEdit.isEnabled = s.mdns
    autoReconnectCheck.isChecked = s.autoReconnect
    audioCheck.isChecked = s.audio
    widthEdit.setText(s.width.toString())
    heightEdit.setText(s.height.toString())
    fpsEdit.setText(s.fps.toString())
    // Until the card has said what it can do, the lists hold the card's own default and
    // the custom entry; the saved size and rate are what they are matched against once
    // the real list arrives.
    showModes(
        listOf(Resolution(0, 0), Resolution(0, 0, custom = true)),
        listOf(Rate(0)),
        emptyList(),
        s.width, s.height, s.fps,
    )
  }

  private fun readSettings(): Settings {
    val d = Settings()
    val mode = selectedResolution()
    val custom = mode?.custom == true
    val rate = if (custom) (fpsEdit.text.toString().toIntOrNull() ?: 0) else (selectedRate()?.fps ?: 0)
    return Settings(
      web = webCheck.isChecked,
      webPort = webPortEdit.text.toString().toIntOrNull() ?: d.webPort,
      rtsp = rtspCheck.isChecked,
      rtspPort = rtspPortEdit.text.toString().toIntOrNull() ?: d.rtspPort,
      lan = lanCheck.isChecked,
      mdns = mdnsCheck.isChecked,
      mdnsName = mdnsNameEdit.text.toString().trim().ifEmpty {
        d.mdnsName
      },
      autoReconnect = autoReconnectCheck.isChecked,
      audio = audioCheck.isChecked,
      // A chosen mode is a size and a rate. "Let the card choose" is all zeros, which is
      // how the engine is told to pick the card's own default. A custom size is passed as
      // typed, so a card that will not take it says so instead of being second-guessed.
      width = if (custom) (widthEdit.text.toString().toIntOrNull() ?: 0) else (mode?.width ?: 0),
      height = if (custom) (heightEdit.text.toString().toIntOrNull() ?: 0) else (mode?.height ?: 0),
      fps = rate,
    )
  }

  // ------------------------------------------------------------------ video mode

  /** The resolution entry the spinner is on, or null if the list is empty. */
  private fun selectedResolution(): Resolution? =
      resolutionList.getOrNull(resolutionSpinner.selectedItemPosition)

  /** The rate entry the spinner is on, or null if the list is empty. */
  private fun selectedRate(): Rate? = rateList.getOrNull(fpsSpinner.selectedItemPosition)

  /**
   * Fills both lists and puts each one on the entry the saved settings ask for, when
   * the card offers it - so a choice survives the list being asked for again, and a
   * size or rate the card does not list falls back to the first entry instead of
   * being silently kept.
   *
   * The two are chosen independently, which is the point: any listed resolution can be
   * paired with any listed rate. [found] is the card's own list of size-and-rate pairs,
   * kept so [showSelectedMode] can say when a pairing was never offered.
   */
  private fun showModes(
    resolutions: List<Resolution>,
    rates: List<Rate>,
    found: List<Detected>,
    width: Int,
    height: Int,
    fps: Int,
  ) {
    resolutionList = resolutions
    rateList = rates
    detectedList = found

    resolutionSpinner.adapter = spinnerAdapter(resolutions.map { it.label })
    fpsSpinner.adapter = spinnerAdapter(rates.map { it.label })

    val wantedResolution = resolutions.indexOfFirst {
      !it.custom && it.width == width && it.height == height
    }
    resolutionSpinner.setSelection(if (wantedResolution >= 0) wantedResolution else 0, false)

    val wantedRate = rates.indexOfFirst { it.fps == fps }
    fpsSpinner.setSelection(if (wantedRate >= 0) wantedRate else 0, false)

    resolutionLabel.text = "Resolution"
    showSelectedMode()
  }

  private fun spinnerAdapter(labels: List<String>): ArrayAdapter<String> {
    val adapter = ArrayAdapter(this, android.R.layout.simple_spinner_item, labels)
    adapter.setDropDownViewResource(android.R.layout.simple_spinner_dropdown_item)
    return adapter
  }

  /**
   * Says what the two spinners add up to, which is the one line that matters: the
   * card takes a size and a rate together, so a pairing it never listed is very likely
   * to be refused, and that is worth saying before Start is pressed rather than after.
   *
   * It also shows the manual size fields, which only apply while "Custom" is chosen -
   * until then the two spinners are the whole of the choice.
   */
  private fun showSelectedMode() {
    val custom = selectedResolution()?.custom == true
    customModeRow.visibility = if (custom) View.VISIBLE else View.GONE
    resolutionSpinner.isEnabled = !custom
    fpsSpinner.isEnabled = !custom

    val resolution = selectedResolution()
    val rate = selectedRate()
    val size = when {
      custom -> "%s x %s".format(
          widthEdit.text.toString().ifEmpty { "?" },
          heightEdit.text.toString().ifEmpty { "?" },
      )
      resolution == null || resolution.width == 0 -> "the card's own size"
      else -> "%d x %d".format(Locale.US, resolution.width, resolution.height)
    }
    val frames = when {
      custom -> fpsEdit.text.toString().ifEmpty { "?" } + " fps"
      rate == null || rate.fps == 0 ->
          if (resolution?.width == 0) "its own rate" else "30 fps (this app's fallback)"
      else -> "%d fps".format(Locale.US, rate.fps)
    }
    val unlisted = !custom && resolution != null && resolution.width > 0 && rate != null && rate.fps > 0 &&
        detectedList.none { it.width == resolution.width && it.height == resolution.height && it.fps == rate.fps }
    modeSummary.text = "MJPEG $size @ $frames" +
        if (unlisted) "\nthe card does not list that size at that rate - it may refuse to start" else ""
  }

  /**
   * Asks the card which sizes and rates it can do, and lists each of them.
   *
   * The card has to be opened to be asked, and it can only be opened when nothing is
   * streaming from it - so this is skipped while the camera runs, and a probe asked for
   * then says so instead of interrupting the stream. Asking is a fraction of a second,
   * but it is not instant, so it happens on a thread of its own and every word it says
   * comes back through [onMain].
   *
   * [asked] is true when the user pressed the button, which is when being refused for
   * lack of USB permission is worth a message: otherwise the list is simply left as it is.
   */
  private fun probeModes(asked: Boolean) {
    if (modesProbing) return
    if (CaptureService.state != CaptureService.State.STOPPED) {
      if (asked) toast("The mode list is read from the card itself, so it can only be asked while it is not streaming")
      return
    }
    val device = Util.findCaptureDevice(usb)
    if (device == null) {
      if (asked) toast("No USB capture card found - plug it in")
      return
    }
    if (!usb.hasPermission(device)) {
      // Without permission the card's descriptors cannot be read at all. Ask for it: the
      // list is worth having, and the same permission is needed to stream anyway. The
      // answer comes back to the receiver, which then asks for the list and no more.
      if (asked) {
        pendingDeviceName = device.deviceName
        probeAfterPermission = true
        val intent = Intent(ACTION_USB_PERMISSION).setPackage(packageName)
        val flags = if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0
        try {
          usb.requestPermission(device, PendingIntent.getBroadcast(this, 1, intent, flags))
        } catch (e: SecurityException) {
          probeAfterPermission = false
          toast("Android refused: grant the Camera and Microphone permission in the app settings, then try again")
        }
      }
      return
    }

    modesProbing = true
    resolutionLabel.text = "Resolution (asking the card...)"
    Thread {
      var found: List<Detected> = emptyList()
      var problem: String? = null
      try {
        val conn = usb.openDevice(device)
        try {
          // Four numbers per size-and-rate the card listed, and it is asked again with a
          // longer array if this one turns out to be too short. 256 is far more than any
          // card lists.
          var longs = 64
          while (longs <= 1024) {
            val into = LongArray(longs)
            val count = Native.listModes(conn.fileDescriptor, into)
            if (count > 0) {
              found = ArrayList(count)
              for (i in 0 until count) {
                found.add(
                    Detected(
                        width = into[i * 4].toInt(),
                        height = into[i * 4 + 1].toInt(),
                        fps = into[i * 4 + 2].toInt(),
                        isDefault = into[i * 4 + 3] == 1L,
                    )
                )
              }
              break
            }
            if (count < 0) {
              // The codes are the ones start() uses, but they mean something else here:
              // nothing was asked of the card's video modes, they could not be read at all.
              problem = when (-count) {
                1 -> "libuvc could not start"
                2 -> "could not open the capture card (permission missing, or unplugged?)"
                3 -> "the card lists no MJPEG mode (this app only handles MJPEG)"
                else -> "error $count"
              }
              break
            }
            longs *= 4
          }
          if (found.isEmpty() && problem == null) {
            problem = "the card's list of sizes did not fit in $longs numbers"
          }
        } finally {
          conn.close()
        }
      } catch (e: Exception) {
        problem = "could not ask the card what it can do ($e)"
      }
      val modes = found
      val message = problem
      onMain {
        modesProbing = false
        if (modes.isEmpty()) {
          if (message != null) {
            Util.appendLog(this, "no mode list: $message")
            if (asked) toast(message)
          }
          resolutionLabel.text = "Resolution (not read from the card - press Detect modes)"
          return@onMain
        }
        // The card's own default comes first, so a card that offers nothing useful still
        // leaves a choice that works; the custom entry is the way out of a card whose
        // list is wrong.
        val own = modes.firstOrNull { it.isDefault }
        val resolutions = ArrayList<Resolution>()
        resolutions.add(Resolution(0, 0))
        val seenSizes = HashSet<Pair<Int, Int>>()
        for (m in modes) {
          if (seenSizes.add(m.width to m.height)) {
            resolutions.add(
                Resolution(m.width, m.height, isDefault = own?.width == m.width && own?.height == m.height)
            )
          }
        }
        resolutions.add(Resolution(0, 0, custom = true))

        // The rates on their own, fastest first. Every rate the card listed at any size
        // is offered at every size, which is what makes the two choices independent.
        val rates = ArrayList<Rate>()
        val seenRates = HashSet<Int>()
        for (m in modes.sortedByDescending { it.fps }) {
          if (seenRates.add(m.fps)) {
            rates.add(Rate(m.fps, isDefault = own?.fps == m.fps))
          }
        }

        val wanted = readSettings()
        showModes(resolutions, rates, modes, wanted.width, wanted.height, wanted.fps)
        Util.appendLog(
            this,
            "the card lists ${modes.size} size-and-rate combination(s): " +
                modes.joinToString(", ") { "${it.width}x${it.height}@${it.fps}" } +
                (if (own == null) ", and names none of them as its own default"
                else ", its own default is ${own.width} x ${own.height} @ ${own.fps} fps"),
        )
        Util.appendLog(
            this,
            "showing ${resolutions.size - 2} resolution(s) and ${rates.size} rate(s) as separate choices",
        )
      }
    }.start()
  }

  // ------------------------------------------------------------------ start / stop

  private fun onStartStopClicked() {
    if (CaptureService.state != CaptureService.State.STOPPED) {
      startService(Intent(this, CaptureService::class.java).setAction(CaptureService.ACTION_STOP))
      return
    }
    if (modesProbing) {
      // A probe in flight is holding the card open to read it. Streaming would have the
      // two compete for the same interfaces, so the tap waits - a fraction of a second -
      // and goes ahead anyway if the probe somehow never comes back.
      if (startWaits++ < 30) {
        handler.postDelayed({ if (!isFinishing && !isDestroyed) onStartStopClicked() }, 100)
        return
      }
      startWaits = 0
    }
    startWaits = 0
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
    if (Build.VERSION.SDK_INT <= 28) {
      // A finished recording goes into the public Movies folder on these versions.
      wanted.add(Manifest.permission.WRITE_EXTERNAL_STORAGE)
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
    probeAfterPermission = false // this permission is for streaming, not for a mode list
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

  // ------------------------------------------------------------------ recording

  private fun onRecordClicked() {
    if (recordingBusy) return
    if (CaptureService.state != CaptureService.State.RUNNING) {
      toast("Start the camera first - there is nothing to record yet")
      return
    }
    if (H264Recorder.isActive || Native.isRecording()) stopRecording() else startRecording()
  }

  /**
   * Starts recording, as an MP4 with the phone's own encoder. Setting the
   * encoders up takes a moment, so it happens off the main thread and the button
   * waits for the answer.
   */
  private fun startRecording() {
    val dir = Util.recordDir(this)
    recordingBusy = true
    recordButton.isEnabled = false
    Thread {
      val problem = H264Recorder.start(this, dir)
      if (problem == null) {
        // It is written in the app's own folder while it is being made, and put
        // where other apps can see it when it is done.
        Util.appendLog(this, "recording as MP4 (H.264) into ${Util.MOVIE_PATH}")
        show("Recording into ${Util.MOVIE_PATH}")
      } else {
        // A phone without a usable encoder still gets a recording, just a bigger
        // one: the card's own pictures, written untouched into an AVI.
        Util.appendLog(this, "MP4 recording would not start ($problem); writing AVI instead")
        val code = Native.startRecord()
        show(
            if (code == 0) "Recording ${dir.absolutePath} as AVI"
            else Native.describeError(code))
      }
      recordingBusy = false
      onMain { refreshStatus() }
    }.start()
  }

  /** Stops whatever is running, whichever way it was started. */
  private fun stopRecording() {
    val wasEncoded = H264Recorder.isActive
    recordingBusy = true
    recordButton.isEnabled = false
    Thread {
      if (wasEncoded) {
        val done = H264Recorder.stop()
        val message =
            when {
              done.error != null -> done.error
              done.file != null ->
                  "Saved ${done.file.name} to ${Util.MOVIE_PATH}: " +
                      "${done.seconds}s, ${done.frames} pictures"
              else -> "Nothing was captured, so no file was written"
            }
        if (done.skipped > 0) {
          Util.appendLog(this, "this phone was too slow for ${done.skipped} picture(s); the rest played in step")
        }
        show(message)
      } else {
        val dir = Util.recordDir(this)
        val frames = Native.stopRecord()
        val message =
            if (frames >= 0) "Saved $frames pictures to ${dir.absolutePath}"
            else Native.describeError(frames)
        show(message)
      }
      recordingBusy = false
      onMain { refreshStatus() }
    }.start()
  }

  /**
   * Hands something to the main thread, which is the only one that may touch a
   * view. The record buttons do their work on a thread of their own, so every
   * word they say about it comes back through here.
   */
  private fun onMain(what: () -> Unit) {
    handler.post(what)
  }

  private fun show(message: String) {
    onMain { toast(message) }
  }

  private fun recordSuffix(): String {
    val encoded = H264Recorder.progress()
    if (encoded != null) {
      val mins = encoded.seconds / 60
      return ": recording %d:%02d, %d pictures, %d MB (H.264)"
          .format(mins, encoded.seconds % 60, encoded.frames, encoded.megabytes.toInt())
    }
    if (!Native.isRecording()) return ""
    val total = Native.recordSeconds()
    return ": recording %d:%02d, %d frames, %d MB"
        .format(total / 60, total % 60, Native.recordFrames(), Native.recordMegabytes())
  }

  // ------------------------------------------------------------------ status display

  private fun refreshStatus() {
    val device = Util.findCaptureDevice(usb)
    deviceText.text = if (device != null) "Capture card: " + Util.describe(device) else "No capture card plugged in"

    val state = CaptureService.state
    startButton.text = if (state == CaptureService.State.STOPPED) "Start" else "Stop"
    val running = state == CaptureService.State.RUNNING
    statusText.text = when (state) {
      CaptureService.State.STOPPED -> "Stopped" + messageSuffix()
      CaptureService.State.STARTING -> "Starting..."
      CaptureService.State.RUNNING -> "Running" + recordSuffix()
      CaptureService.State.WAITING -> CaptureService.message
    }

    val recording = running && (H264Recorder.isActive || Native.isRecording())
    recordButton.isEnabled = running && !recordingBusy
    recordButton.text = if (recording) "Stop recording" else "Record"
    val settings = readSettings()
    viewerButton.isEnabled = running && settings.web
    urlText.text = if (running) buildUrlText(settings) else ""

    val log = Util.tail(File(filesDir, Util.LOG_NAME))
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
      // While mDNS is on but not yet confirmed, show a placeholder so it's clear it's still
      // working, not stuck or broken - see CaptureService.mdnsRegistered.
      if (s.mdns) {
        if (CaptureService.mdnsRegistered) {
          val host = s.mdnsName.trim().ifEmpty {
            "uvcweb"
          } + ".local"
          if (s.web) sb.append("http://").append(host).append(':').append(s.webPort).append("/\n")
          if (s.rtsp) sb.append("rtsp://").append(host).append(':').append(s.rtspPort).append("/live\n")
        } else if (CaptureService.mdnsFailed) {
          sb.append("mDNS: unavailable\n")
        } else {
          sb.append("mDNS: connecting...\n")
        }
      }
    }
    return sb.toString().trimEnd()
  }

  // A toast disappears in a few seconds; every one of these is also worth keeping in the
  // on-screen log, since it explains why nothing else seems to be happening.
  private fun toast(text: String) {
    Util.appendLog(this, text)
    Toast.makeText(this, text, Toast.LENGTH_LONG).show()
  }

  companion object {
    private const val ACTION_USB_PERMISSION = "com.uvcweb.app.USB_PERMISSION"
    private const val REQUEST_PERMISSIONS = 1
  }
}