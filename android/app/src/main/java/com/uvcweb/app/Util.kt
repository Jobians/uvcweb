package com.uvcweb.app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.IntentFilter
import android.hardware.usb.UsbConstants
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.os.Build
import java.io.File
import java.io.FileOutputStream
import java.io.RandomAccessFile
import java.net.Inet4Address
import java.net.NetworkInterface
import java.text.SimpleDateFormat
import java.util.Collections
import java.util.Date
import java.util.Locale

/** Small helpers shared by the activities and the service. */
object Util {

    /** Name of the shared log file under Context.filesDir. Both the Rust side (via the
     * UVCWEB_LOG_FILE environment variable) and the Kotlin side (via [appendLog]) write to this
     * same file, so the app's on-screen log is one unified timeline instead of two separate ones. */
    const val LOG_NAME = "uvcweb.log"

    private val logLock = Any()
    private val stampFormat get() = SimpleDateFormat("HH:mm:ss", Locale.US)

    /**
     * Appends one line to the shared app log, in the same "[HH:MM:SS] text" format the Rust side
     * uses, so it reads as one continuous log on the main screen. This is the app's own on-screen
     * logging: nothing important should only go to Logcat, since the person using the app has no
     * way to see that without a computer and adb.
     */
    fun appendLog(context: Context, line: String) {
        synchronized(logLock) {
            try {
                val stamped = "[${stampFormat.format(Date())}] $line\n"
                FileOutputStream(File(context.filesDir, LOG_NAME), true).use { it.write(stamped.toByteArray()) }
            } catch (e: Exception) {
                // if even this fails there is nowhere else to report it
            }
        }
    }

    /** Empties the shared log file. Used by the "Clear" button on the main screen. */
    fun clearLog(context: Context) {
        synchronized(logLock) {
            try {
                FileOutputStream(File(context.filesDir, LOG_NAME), false).use { }
            } catch (e: Exception) {
                // nothing more to do
            }
        }
    }

    /** The first attached device with a USB video (UVC) interface. */
    fun findCaptureDevice(usb: UsbManager): UsbDevice? {
        for (device in usb.deviceList.values) {
            for (i in 0 until device.interfaceCount) {
                if (device.getInterface(i).interfaceClass == UsbConstants.USB_CLASS_VIDEO) {
                    return device
                }
            }
        }
        return null
    }

    fun describe(device: UsbDevice): String {
        val id = String.format(Locale.US, "%04x:%04x", device.vendorId, device.productId)
        val name = device.productName ?: device.deviceName
        return "$id  $name"
    }

    /** Last few kilobytes of a text file, starting at a line boundary. */
    fun tail(file: File, maxBytes: Int = 8000): String {
        if (!file.exists()) return ""
        return try {
            RandomAccessFile(file, "r").use { f ->
                val length = f.length()
                val start = if (length > maxBytes) length - maxBytes else 0L
                f.seek(start)
                val bytes = ByteArray((length - start).toInt())
                f.readFully(bytes)
                val text = String(bytes, Charsets.UTF_8)
                if (start > 0) text.substringAfter('\n', text) else text
            }
        } catch (e: Exception) {
            ""
        }
    }

    /** IPv4 addresses of this phone on its networks (Wi-Fi, hotspot, ...). */
    fun localIpv4Addresses(): List<String> {
        val result = ArrayList<String>()
        try {
            val interfaces = NetworkInterface.getNetworkInterfaces() ?: return result
            for (nif in Collections.list(interfaces)) {
                if (!nif.isUp || nif.isLoopback) continue
                for (address in Collections.list(nif.inetAddresses)) {
                    if (address is Inet4Address && !address.isLoopbackAddress) {
                        val text = address.hostAddress
                        if (text != null) result.add(text)
                    }
                }
            }
        } catch (e: Exception) {
            // no network information available
        }
        return result
    }
}

/** registerReceiver that works on every Android version and keeps the receiver private to this app. */
fun Context.registerPrivateReceiver(receiver: BroadcastReceiver, filter: IntentFilter) {
    if (Build.VERSION.SDK_INT >= 33) {
        registerReceiver(receiver, filter, Context.RECEIVER_NOT_EXPORTED)
    } else {
        registerReceiver(receiver, filter)
    }
}
