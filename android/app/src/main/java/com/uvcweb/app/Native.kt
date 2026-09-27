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

    // ------------------------------------------------------------ the encoder feed
    //
    // The H.264 recorder pulls the card's pictures and sound out of these. It is
    // a short queue, not a record of everything: a picture that arrives too late
    // is worth nothing to an encoder, so the freshest one wins. Pulling, rather
    // than being called back into, keeps the capture thread free of the encoder.

    /** Starts copying the stream for one reader. 0 on success, otherwise [describeError]. */
    @JvmStatic
    external fun feedArm(): Int

    /** Stops copying. Pull the tail first: whatever is still queued goes with this. */
    @JvmStatic
    external fun feedDisarm()

    /** True once the reader is on the live edge; sound from before that is not recorded. */
    @JvmStatic
    external fun feedAttached(): Boolean

    /** True once the capture session ended, so a reader can finish its file. */
    @JvmStatic
    external fun feedEnded(): Boolean

    /** Timestamp in microseconds of the next item, or -1 when there is nothing. */
    @JvmStatic
    external fun feedPeekPts(kind: Int): Long

    /**
     * Fills [into] with the next item and returns how many bytes it wrote, -1 when
     * the queue was empty, or -2 when [into] was too small - in which case the item
     * stays and the call can be repeated with a bigger buffer.
     */
    @JvmStatic
    external fun feedPull(kind: Int, into: ByteArray): Int

    /** The sound format as rate shl 32 or channels, or -1 when there is none yet. */
    @JvmStatic
    external fun feedAudioFormat(): Long

    /** The picture rate the session is running at, as a hint for an encoder. */
    @JvmStatic
    external fun feedVideoFps(): Double

    /** [frames, dropped, chunks, soundBytes, seconds, queued, queuedBytes]. */
    @JvmStatic
    external fun feedStats(): LongArray

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
