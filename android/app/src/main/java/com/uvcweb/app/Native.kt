package com.uvcweb.app

/**
 * The Rust library (libuvcweb_core.so). The functions match the JNI entry points in
 * src/android.rs, so names, order and types must stay in sync with that file.
 * They take primitives only.
 */
object Native {
    init {
        System.loadLibrary("uvcweb_core")
    }

    /**
     * Opens the capture card through [fd] (from UsbDeviceConnection.getFileDescriptor) and starts
     * serving. Blocks for a second or two: call it from a background thread.
     *
     * width/height/fps of 0 mean "the card's own default mode".
     * A port of 0 switches that protocol off.
     * Returns 0 on success, otherwise an error code (see [describeError]).
     */
    @JvmStatic
    external fun start(
        fd: Int,
        width: Int,
        height: Int,
        fps: Int,
        audio: Boolean,
        audioRate: Int,
        audioChannels: Int,
        lan: Boolean,
        webPort: Int,
        rtspPort: Int,
        avOffsetMs: Int,
    ): Int

    /** Stops everything and waits until the camera is released. Call from a background thread. */
    @JvmStatic
    external fun stop()

    @JvmStatic
    external fun isRunning(): Boolean

    /**
     * Starts recording everything the session streams into the folder named by the
     * environment variable UVCWEB_RECORD_DIR (see CaptureService).
     * Returns 0 on success, otherwise [describeError].
     * The first picture is what creates the file, so a recording started before the
     * camera streams has nothing to save yet.
     */
    @JvmStatic
    external fun startRecord(): Int

    /**
     * Stops the recording, which writes the index and closes the file so a player
     * accepts it. Returns the number of pictures it holds, or [describeError].
     */
    @JvmStatic
    external fun stopRecord(): Int

    /** Whether a recording is running right now. */
    @JvmStatic
    external fun isRecording(): Boolean

    /** Pictures written into the running recording so far (0 when idle). */
    @JvmStatic
    external fun recordFrames(): Int

    /** Seconds the running recording has been going (0 when idle). */
    @JvmStatic
    external fun recordSeconds(): Int

    /** Megabytes written into the running recording so far (0 when idle). */
    @JvmStatic
    external fun recordMegabytes(): Int

    fun describeError(code: Int): String = when (code) {
        1 -> "libuvc could not start"
        2 -> "could not open the capture card (permission missing, or unplugged?)"
        3 -> "the card has no such video mode - try another size, or 0 x 0 for the card's default"
        4 -> "video streaming failed - try a smaller size or a lower frame rate"
        5 -> "a network port is already in use - pick another port"
        -100 -> "already running"
        -101 -> "internal error (see the log)"
        -102 -> "the camera is not running, so there is nothing to record"
        -103 -> "the record folder could not be used - see the log"
        -104 -> "the recording was empty, so no file was written"
        else -> "error $code"
    }
}
