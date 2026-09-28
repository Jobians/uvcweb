package com.uvcweb.app

import android.annotation.TargetApi
import android.content.BroadcastReceiver
import android.content.ContentResolver
import android.content.ContentValues
import android.content.Context
import android.content.IntentFilter
import android.hardware.usb.UsbConstants
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.media.MediaScannerConnection
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
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

    /** Folder name for the half-finished work, inside the app's own storage (see [recordDir]). */
    const val RECORD_DIR_NAME = "record"

    /** This app's own folder inside the shared Movies collection. */
    const val MOVIES_SUBDIR = "com.uvcweb.app"

    /**
     * Where a finished recording is published, as MediaStore wants it: a path from
     * the storage root, which is the same folder [moviesDir] points at.
     */
    const val MOVIE_PATH = "Movies/$MOVIES_SUBDIR"

    /** What a recording is being written as, which is how it is published. */
    private const val MOVIE_MIME = "video/mp4"

    /**
     * Where the work of a recording goes. The app's external files folder is
     * preferred over the internal one: it needs no permission and a file manager
     * can show the .avi files, so a recording can actually be found afterwards.
     * Falls back to the internal folder on a device with no external storage
     * mounted. A file here is the app's own business and nothing else can see
     * it, which is what a recording in progress has to be.
     *
     * The Rust side learns this folder through the UVCWEB_RECORD_DIR environment
     * variable, set by [CaptureService] before the engine starts.
     */
    fun recordDir(context: Context): File {
        val root = context.getExternalFilesDir(null) ?: context.filesDir
        return File(root, RECORD_DIR_NAME)
    }

    /** The real folder [MOVIE_PATH] names, for saying where a recording will be. */
    fun moviesDir(context: Context): File =
        File(
            Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_MOVIES), MOVIES_SUBDIR)

    /**
     * Puts a finished recording where other apps and the gallery can find it, and
     * answers the file it is now at.
     *
     * The recording is written to the app's own folder while it is being made, so
     * a file that never got finished is never seen by anything else; only a file
     * the muxer was happy with is published, and only once every byte of it is
     * in place. Nothing is left half-published: if the copy fails, the entry
     * goes away again rather than showing a file that plays half way.
     */
    fun publishMovie(context: Context, part: File): File {
        val name = part.name.removeSuffix(".part")
        return if (Build.VERSION.SDK_INT >= 29) {
            publishToCollection(context, part, name)
        } else {
            publishToFolder(context, part, name)
        }
    }

    /**
     * Android's own way: a row in the Movies collection, marked pending until
     * every byte of the recording is in it. Pending is what keeps it out of the
     * gallery and out of other apps' reach while it is being written.
     */
    @TargetApi(Build.VERSION_CODES.Q)
    private fun publishToCollection(context: Context, part: File, name: String): File {
        val resolver = context.contentResolver
        val values = ContentValues()
        values.put(MediaStore.Video.Media.DISPLAY_NAME, name)
        values.put(MediaStore.Video.Media.MIME_TYPE, MOVIE_MIME)
        values.put(MediaStore.Video.Media.RELATIVE_PATH, MOVIE_PATH)
        values.put(MediaStore.Video.Media.IS_PENDING, 1)
        val uri =
            resolver.insert(MediaStore.Video.Media.EXTERNAL_CONTENT_URI, values)
                ?: throw IOException("the Movies collection would not take $name")
        try {
            resolver.openOutputStream(uri)?.use { out ->
                part.inputStream().use { from -> from.copyTo(out, 64 * 1024) }
            } ?: throw IOException("the Movies collection would not open $name")
            // Everything is in it, so it can be seen now.
            values.clear()
            values.put(MediaStore.Video.Media.IS_PENDING, 0)
            resolver.update(uri, values, null, null)
        } catch (e: Exception) {
            // Half a file is worse than none: the row goes with it, so nothing is
            // left in the collection that plays half way or that cannot be seen
            // at all.
            runCatching { resolver.delete(uri, null, null) }
            throw e
        }
        return File(moviesDir(context), publishedName(resolver, uri, name))
    }

    /** The name the row ended up with, which is [name] unless that was already taken. */
    @TargetApi(Build.VERSION_CODES.Q)
    private fun publishedName(resolver: ContentResolver, uri: Uri, name: String): String {
        return try {
            resolver.query(uri, arrayOf(MediaStore.Video.Media.DISPLAY_NAME), null, null, null)?.use {
                if (it.moveToFirst()) it.getString(0) else name
            } ?: name
        } catch (e: Exception) {
            name
        }
    }

    /**
     * Before Android had a collection of its own there was only the folder, and
     * the file has to be moved there and then say that it is there, so the
     * gallery picks it up. A phone that will not allow the move still has the
     * file in the app's own folder, and it is said where that is.
     */
    private fun publishToFolder(context: Context, part: File, name: String): File {
        val dir = moviesDir(context)
        if (!dir.isDirectory && !dir.mkdirs()) {
            throw IOException("could not make ${dir.absolutePath}")
        }
        val target = File(dir, name)
        // One card, so this is a move rather than a copy of the whole file.
        if (!part.renameTo(target)) {
            part.inputStream().use { from -> target.outputStream().use { to -> from.copyTo(to, 64 * 1024) } }
        }
        MediaScannerConnection.scanFile(
            context, arrayOf(target.absolutePath), arrayOf(MOVIE_MIME), null)
        return target
    }

    /** A file name for a recording: the same shape the Rust program uses. */
    fun recordStamp(): String {
        return SimpleDateFormat("yyyyMMdd-HHmmss", Locale.US).format(Date())
    }

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
