package com.uvcweb.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.hardware.usb.UsbDeviceConnection
import android.hardware.usb.UsbManager
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.system.Os
import java.io.File

/**
 * Owns the USB connection and the Rust engine. It is a foreground service, so the capture and the
 * web / RTSP servers keep running with the screen off or while another app is in front.
 */
class CaptureService : Service() {

    enum class State { STOPPED, STARTING, RUNNING }

    private var connection: UsbDeviceConnection? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var deviceName: String? = null
    private var nsdManager: NsdManager? = null
    private val mdnsListeners = mutableListOf<NsdManager.RegistrationListener>()
    private val lock = Any()

    @Volatile
    private var stopRequested = false

    private val detachReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            val name = deviceName ?: return
            val usb = getSystemService(Context.USB_SERVICE) as UsbManager
            if (!usb.deviceList.containsKey(name)) {
                stopEverything("The capture card was unplugged")
            }
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        registerPrivateReceiver(detachReceiver, IntentFilter(UsbManager.ACTION_USB_DEVICE_DETACHED))
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent == null || intent.action == ACTION_STOP) {
            stopEverything("Stopped")
            return START_NOT_STICKY
        }
        startInForeground("Starting...")

        synchronized(lock) {
            if (state != State.STOPPED) {
                Util.appendLog(this, "start requested while already $state - ignored")
                return START_NOT_STICKY
            }
            state = State.STARTING
            message = "Starting..."
        }
        stopRequested = false
        deviceName = intent.getStringExtra(EXTRA_DEVICE_NAME)
        val name = deviceName
        Thread { runCapture(name) }.start()
        return START_NOT_STICKY
    }

    /** Background thread: open the card and start the Rust engine. Every path out of this
     * function - success or failure - writes to the app's own on-screen log, never just to
     * Logcat, so what happened is visible without a computer. */
    private fun runCapture(name: String?) {
        Util.appendLog(this, "opening ${name ?: "(no device name given)"}...")
        val usb = getSystemService(Context.USB_SERVICE) as UsbManager
        val device = if (name == null) null else usb.deviceList[name]
        if (device == null) {
            val attached = usb.deviceList.keys.joinToString(", ").ifEmpty { "none" }
            fail("The capture card was not found (attached USB devices: $attached)")
            return
        }
        if (!usb.hasPermission(device)) {
            fail("No USB permission for the capture card")
            return
        }
        val conn = try {
            usb.openDevice(device)
        } catch (e: Exception) {
            Util.appendLog(this, "openDevice threw: ${e}")
            null
        }
        if (conn == null) {
            fail("Could not open the capture card")
            return
        }
        connection = conn
        Util.appendLog(this, "device opened (fd=${conn.fileDescriptor}), starting the engine...")

        val settings = Settings.load(this)

        // The Rust side appends its own log lines to this same file - see Util.appendLog.
        val logFile = File(filesDir, Util.LOG_NAME)
        try {
            Os.setenv("UVCWEB_LOG_FILE", logFile.absolutePath, true)
        } catch (e: Exception) {
            Util.appendLog(this, "could not set UVCWEB_LOG_FILE (${e}); the Rust side will only log to Logcat")
        }

        // Hand the untouched file descriptor to libusb, exactly like `termux-usb -e` does.
        val code = Native.start(
            conn.fileDescriptor,
            settings.width,
            settings.height,
            settings.fps,
            settings.audio,
            48000,
            2,
            settings.lan,
            if (settings.web) settings.webPort else 0,
            if (settings.rtsp) settings.rtspPort else 0,
            0,
        )
        if (code != 0) {
            conn.close()
            connection = null
            fail("Native.start returned $code: ${Native.describeError(code)}")
            return
        }
        if (stopRequested) {
            Util.appendLog(this, "stop was requested while the engine was starting - stopping it now")
            doStop("Stopped")
            return
        }

        // Neither of these is essential to actually serving video/audio, so a problem in either
        // one must never leave the service stuck in "Starting..." forever: log it and carry on.
        try {
            acquireWakeLock()
        } catch (e: Exception) {
            Util.appendLog(this, "could not acquire a wake lock (${e}); the screen turning off may pause capture")
        }
        try {
            registerMdns(settings)
        } catch (e: Exception) {
            Util.appendLog(this, "mDNS setup threw (${e}); continuing without network discovery")
        }

        synchronized(lock) {
            state = State.RUNNING
            message = "Running"
        }
        Util.appendLog(this, "running: " + Util.describe(device))
        updateNotification("Running - " + Util.describe(device))
    }

    private fun fail(reason: String) {
        Util.appendLog(this, "failed to start: $reason")
        synchronized(lock) {
            state = State.STOPPED
            message = reason
        }
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    /** Stop from any thread; the actual work happens on a background thread. */
    private fun stopEverything(reason: String) {
        stopRequested = true
        Thread {
            doStop(reason)
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
        }.start()
    }

    private fun doStop(reason: String) {
        Util.appendLog(this, "stopping: $reason")
        unregisterMdns()
        try {
            Native.stop()
        } catch (e: Throwable) {
            Util.appendLog(this, "Native.stop() threw (${e}); the native library may not have been loaded")
        }
        try {
            connection?.close()
        } catch (e: Exception) {
            Util.appendLog(this, "closing the USB connection threw (${e})")
        }
        connection = null
        releaseWakeLock()
        synchronized(lock) {
            state = State.STOPPED
            message = reason
        }
    }

    override fun onDestroy() {
        try {
            unregisterReceiver(detachReceiver)
        } catch (e: Exception) {
            // not registered
        }
        if (state != State.STOPPED) {
            Util.appendLog(this, "service destroyed while state was $state - stopping")
            doStop("Stopped")
        }
        super.onDestroy()
    }

    // ------------------------------------------------------------------ mDNS (network discovery)

    /**
     * Advertises the running server(s) so other devices can find this phone by name, e.g. VLC's
     * "Local Network" browser. Only done while `lan` is on: a server bound to loopback-only would
     * still show up to other devices, but every connection to it would then fail.
     */
    private fun registerMdns(settings: Settings) {
        if (!settings.lan) {
            Util.appendLog(this, "mDNS: not advertising (\"Allow other devices on the network\" is off)")
            return
        }
        if (!settings.mdns) {
            Util.appendLog(this, "mDNS: not advertising (\"Advertise via mDNS / Bonjour\" is off)")
            return
        }
        if (Util.localIpv4Addresses().isEmpty()) {
            Util.appendLog(this, "mDNS: not advertising (no local network address - connect to Wi-Fi or a hotspot)")
            return
        }
        val nsd = getSystemService(Context.NSD_SERVICE) as? NsdManager
        if (nsd == null) {
            Util.appendLog(this, "mDNS: NSD_SERVICE is not available on this device - skipping")
            return
        }
        nsdManager = nsd
        mdnsRegistered = false
        mdnsFailed = false
        val name = settings.mdnsName.trim().ifEmpty { "uvcweb" }
        if (!settings.web && !settings.rtsp) {
            Util.appendLog(this, "mDNS: nothing to advertise (neither web nor RTSP is enabled)")
            return
        }
        if (settings.web) {
            registerOneMdnsService(nsd, name, "_http._tcp.", settings.webPort) { info ->
                info.setAttribute("path", "/")   // Bonjour convention: where to find the actual page
            }
        }
        if (settings.rtsp) {
            registerOneMdnsService(nsd, name, "_rtsp._tcp.", settings.rtspPort, null)
        }
    }

    private fun registerOneMdnsService(
        nsd: NsdManager,
        name: String,
        type: String,
        port: Int,
        configure: ((NsdServiceInfo) -> Unit)?,
    ) {
        val info = NsdServiceInfo().apply {
            serviceName = name
            serviceType = type
            this.port = port
        }
        configure?.invoke(info)
        val listener = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(reg: NsdServiceInfo) {
                mdnsRegistered = true
                Util.appendLog(this@CaptureService, "mDNS: advertising '${reg.serviceName}' ($type) on port $port")
            }
            override fun onRegistrationFailed(reg: NsdServiceInfo, errorCode: Int) {
                // Not fatal: the server itself is unaffected, it just won't show up by name -
                // other devices can still use its IP address directly.
                mdnsFailed = true
                Util.appendLog(this@CaptureService, "mDNS: could not advertise $type (error $errorCode)")
            }
            override fun onServiceUnregistered(reg: NsdServiceInfo) {}
            override fun onUnregistrationFailed(reg: NsdServiceInfo, errorCode: Int) {}
        }
        mdnsListeners.add(listener)
        try {
            nsd.registerService(info, NsdManager.PROTOCOL_DNS_SD, listener)
        } catch (e: Exception) {
            mdnsListeners.remove(listener)
            Util.appendLog(this, "mDNS: registerService threw for $type (${e})")
        }
    }

    private fun unregisterMdns() {
        val nsd = nsdManager
        for (listener in mdnsListeners) {
            try {
                nsd?.unregisterService(listener)
            } catch (e: Exception) {
                Util.appendLog(this, "mDNS: unregisterService threw (${e}) - harmless if it was never fully registered")
            }
        }
        mdnsListeners.clear()
        nsdManager = null
        mdnsRegistered = false
        mdnsFailed = false
    }

    // ------------------------------------------------------------------ notification / wake lock

    private fun startInForeground(text: String) {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "Capture", NotificationManager.IMPORTANCE_LOW)
        )
        val notification = buildNotification(text)
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    private fun updateNotification(text: String) {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        nm.notify(NOTIFICATION_ID, buildNotification(text))
    }

    private fun buildNotification(text: String): Notification {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE
        )
        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("uvcweb")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.ic_menu_camera)
            .setContentIntent(open)
            .setOngoing(true)
            .build()
    }

    private fun acquireWakeLock() {
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        val wl = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "uvcweb:capture")
        wl.acquire()
        wakeLock = wl
    }

    private fun releaseWakeLock() {
        try {
            wakeLock?.let { if (it.isHeld) it.release() }
        } catch (e: Exception) {
            // ignore
        }
        wakeLock = null
    }

    companion object {
        const val ACTION_STOP = "com.uvcweb.app.STOP"
        const val EXTRA_DEVICE_NAME = "deviceName"
        private const val CHANNEL_ID = "capture"
        private const val NOTIFICATION_ID = 1

        /** Read by the main screen. */
        @Volatile
        var state: State = State.STOPPED

        @Volatile
        var message: String = "Stopped"

        /** True once mDNS has actually confirmed at least one service registered - not just
         * requested. Read by the main screen so it only shows the .local URL when it will really
         * resolve, instead of assuming registration succeeded. */
        @Volatile
        var mdnsRegistered: Boolean = false

        /** True if a registration attempt has come back with a failure. Read by the main screen
         * to show "unavailable" instead of leaving a "connecting..." message up forever. */
        @Volatile
        var mdnsFailed: Boolean = false
    }
}
