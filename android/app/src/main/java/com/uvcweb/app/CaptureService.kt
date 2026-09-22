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
                return START_NOT_STICKY      // already starting or running
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

    /** Background thread: open the card and start the Rust engine. */
    private fun runCapture(name: String?) {
        val usb = getSystemService(Context.USB_SERVICE) as UsbManager
        val device = if (name == null) null else usb.deviceList[name]
        if (device == null) {
            fail("The capture card was not found")
            return
        }
        if (!usb.hasPermission(device)) {
            fail("No USB permission for the capture card")
            return
        }
        val conn = try {
            usb.openDevice(device)
        } catch (e: Exception) {
            null
        }
        if (conn == null) {
            fail("Could not open the capture card")
            return
        }
        connection = conn

        val settings = Settings.load(this)

        // The Rust side copies its log lines into this file; the main screen shows the tail.
        val logFile = File(filesDir, LOG_NAME)
        logFile.delete()
        try {
            Os.setenv("UVCWEB_LOG_FILE", logFile.absolutePath, true)
        } catch (e: Exception) {
            // logcat still gets the log
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
            fail(Native.describeError(code))
            return
        }
        if (stopRequested) {
            doStop("Stopped")
            return
        }

        acquireWakeLock()
        synchronized(lock) {
            state = State.RUNNING
            message = "Running"
        }
        updateNotification("Running - " + Util.describe(device))
    }

    private fun fail(reason: String) {
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
        try {
            Native.stop()
        } catch (e: Throwable) {
            // library not loaded / already stopped
        }
        try {
            connection?.close()
        } catch (e: Exception) {
            // ignore
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
            doStop("Stopped")
        }
        super.onDestroy()
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
        const val LOG_NAME = "uvcweb.log"
        private const val CHANNEL_ID = "capture"
        private const val NOTIFICATION_ID = 1

        /** Read by the main screen. */
        @Volatile
        var state: State = State.STOPPED

        @Volatile
        var message: String = "Stopped"
    }
}
