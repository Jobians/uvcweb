package com.uvcweb.app

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.AudioFormat
import android.media.Image
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.media.MediaMuxer
import android.util.Log
import java.io.File
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.roundToInt
import kotlin.math.sin

/**
 * Records the session into an MP4 with the phone's own hardware H.264 encoder,
 * instead of storing the card's JPEG pictures one by one. A recording that way
 * is several times smaller, and plays in a browser and on a computer as well.
 *
 * The Rust side hands us pictures and sound through [Native]'s feed calls, and
 * this turns them into one file:
 *
 *  * each JPEG is decoded and converted to YUV 4:2:0 - the card sends JPEG, not
 *    YUV, so that has to happen somewhere and here it is - and handed to
 *    [MediaCodec] with the timestamp the card gave it;
 *  * the sound goes through the AAC encoder, because a player that is asked to
 *    show H.264 will not take raw PCM in an MP4;
 *  * [MediaMuxer] writes both tracks into the file.
 *
 * Everything it needs from the card comes through [Native], which means a fault on that side
 * looks like a card that has stopped sending: the wait for a first picture times out with
 * nothing queued, and the numbers in the log are what tell the two apart.
 *
 * Timestamps come from the capture rather than from a frame counter, so if a
 * conversion takes longer than a frame and the picture has to be skipped, the
 * file plays at the rate the phone managed and the sound stays in step: a player
 * simply holds each picture until its time. That is what makes skipping safe
 * here. The file is still not the card's own pictures - the Rust recorder
 * ([src/recorder.rs]) keeps those, untouched, and the app falls back to it when
 * this cannot start.
 *
 * All of it runs on one thread of its own: an encoder wants a single, steady
 * consumer, and the user interface must not wait for any of it.
 */
object H264Recorder {
  private const val TAG = "uvcweb"

  /** Which queue to ask for, pictures or sound. */
  private const val VIDEO = 0
  private const val AUDIO = 1

  /** What [Native.feedPull] says. */
  private const val NOTHING = -1
  private const val TOO_SMALL = -2

  /**
   * The widest picture to convert. The conversion is in software and the
   * encoder scales anything larger down anyway, so a 4K stream becomes a
   * 960-pixel-wide one instead of costing a whole core per frame.
   */
  private const val MAX_WIDTH = 960

  private const val AUDIO_BITRATE = 128_000
  /**
   * The rate the sound is encoded at. A card that hands over 96 kHz is not
   * asking for 96 kHz AAC: a phone's encoder usually says yes and then gives
   * back 48 kHz, which leaves a file whose sound is a whistle on top of the
   * speech. So 48 kHz is asked for, and a higher card rate is brought down to it
   * first.
   */
  private const val SOUND_RATE = 48_000
  private const val I_FRAME_SECONDS = 2
  /** How long to wait for the card's first picture at the start. */
  private const val FIRST_PICTURE_MS = 3_000
  /**
   * How long to give the card's sound interface to say what it does. It is
   * usually already up by the time a recording starts, and a sound track cannot
   * be added to a file once it has started, so this is the one moment waiting
   * is worth it.
   */
  private const val SOUND_GRACE_MS = 400
    /**
     * A card that has not sent a single picture yet is a card that is still
     * starting, not a card that is broken: the session reports itself running
     * before the first frame comes out of the USB pipe, and on a phone that
     * takes a few seconds. So a wait that runs out with nothing sent at all is
     * given longer, while one that runs out with the card already sending is a
     * fault and is reported as one.
     */
    private const val CARD_START_MS = 6_000L
    private const val IDLE_SLEEP_MS = 3L

  /** How long the encoders are given to flush what they hold at the end. */
  private const val STOP_WAIT_MS = 20_000L
  private const val CODEC_TIMEOUT_US = 10_000L
  /** A nominal frame's worth of time, used to place the end of each track. */
  private const val FRAME_US = 33_333L

  /** What a finished recording left behind. */
  class Finished(
      val file: File?,
      val frames: Int,
      val seconds: Long,
      val skipped: Int,
      val error: String?
  )

  /** Live numbers for the status line. */
  class Progress(val frames: Int, val megabytes: Double, val seconds: Long, val skipped: Int)

  @Volatile private var session: Session? = null

  /**
   * A recording that finished by itself - the camera was stopped - and whose
   * result nobody has been told about yet. It is handed to the next stop, so
   * that the file is never written without anyone being told it exists.
   */
  @Volatile private var pending: Finished? = null

  val isActive: Boolean get() = session != null

  /**
   * Starts recording. This blocks until the encoders are up, which is also when
   * a failure shows up: null means it is running, anything else is a message to
   * show the user.
   */
  fun start(context: Context, dir: File): String? {
    if (session != null) return "a recording is already running"
    // Whatever the last one did is in the log by now.
    pending = null
    val code = Native.feedArm()
    if (code != 0) return Native.describeError(code)
    val fresh = Session(context.applicationContext, dir)
    return try {
      fresh.open()
      session = fresh
      null
    } catch (e: Exception) {
      fresh.dispose()
      Native.feedDisarm()
      session = null
      Log.e(TAG, "could not start the H.264 recorder", e)
      e.message ?: "could not start the H.264 recorder"
    }
  }

  /** Stops recording and waits for the file to be finished properly. */
  fun stop(): Finished {
    val running = session
    if (running == null) {
      val done = pending
      pending = null
      return done ?: Finished(null, 0, 0, 0, null)
    }
    session = null
    return running.seal()
  }

  fun progress(): Progress? {
    val running = session ?: return null
    return Progress(
        running.frames, running.part.length() / 1_048_576.0, running.seconds(), running.skipped)
  }

  // ------------------------------------------------------------------ one recording

  private class Session(private val context: Context, private val dir: File) {
    /**
     * Written under a name that says "not finished" until the muxer is happy, so
     * a recording cut short by a crash or a battery pull is never mistaken for
     * something that plays.
     */
    val part = File(dir, "${Util.recordStamp()}.mp4.part")
    private lateinit var muxer: MediaMuxer

    private var video: MediaCodec? = null
    private var audio: MediaCodec? = null
    private var width = 0
    private var height = 0
    private var videoTrack = -1
    private var audioTrack = -1
    private var muxerStarted = false

    private val videoBuf = Buffer(ByteArray(512 * 1024))
    private val audioBuf = Buffer(ByteArray(64 * 1024))
    private var argb = IntArray(0)

    private var audioHeld = 0
    private var audioAt = 0
    private var audioOutRate = 0
    private var audioFrameBytes = 0
    /**
     * The card's sound on its way to the encoder. It is made even when the two
     * rates are the same, in which case it is a single tap and the sound is
     * passed along as it is.
     */
    private lateinit var down: Decimator
    /** The resampled sound, between the card's rate and the encoder's. */
    private val resampled = ByteArray(64 * 1024)
    /** Where the sound now in flight starts, and where the rate it is timed from. */
    private var audioPtsBase = 0L
    private var audioPtsAt = 0L
    private var firstFrame: ByteArray? = null
    private var firstPts = 0L
    private var lastVideoPts = 0L
    private var lastAudioPts = 0L

    @Volatile internal var frames = 0
    @Volatile internal var skipped = 0

    /**
     * Encoded samples that arrived before the file could be started, which can
     * only happen if a phone's encoder would not say what its output looks like.
     * The moment of silence the sound encoder is given to say it does not count:
     * that sound is at the very start and would be thrown away either way.
     */
    private var tooEarly = 0
    private var primed = false
    @Volatile private var stopping = false
    @Volatile private var failure: String? = null
    private var videoEosQueued = false
    private var audioEosQueued = false
    private var videoDone = false
    private var audioDone = true // stays true unless a sound track was configured
    private var finished: File? = null
    private val startedAt = System.currentTimeMillis()
    private var worker: Thread? = null

    private fun log(message: String) {
      Log.i(TAG, message)
      Util.appendLog(context, message)
    }

    /** Sets everything up, so that a failure here is a return value and not a log. */
    fun open() {
      if (!dir.isDirectory && !dir.mkdirs()) {
        throw IllegalStateException("${dir.absolutePath} could not be created")
      }
      part.delete() // left over from a recording that was killed
      muxer = MediaMuxer(part.absolutePath, MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4)

      waitForSound()
      // The picture size is whatever mode the card chose, so a picture has to
      // arrive before an encoder can be configured. This one is also the first
      // frame of the file, which is why it is kept instead of read twice.
      val jpeg = waitForFirstPicture()
      val picture =
          BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size)
              ?: throw IllegalStateException("the card's JPEG could not be decoded")
      val size = fit(picture.width, picture.height)
      width = size.first
      height = size.second
      video = makeVideoEncoder(width, height, fpsHint())
      picture.recycle()
      firstFrame = jpeg

      worker = Thread({ pump() }, "uvcweb-h264")
      worker?.start()
      log("recording $width x $height into ${part.name}")
    }

    private fun waitForSound() {
      val deadline = System.currentTimeMillis() + SOUND_GRACE_MS
      while (Native.feedAudioFormat() < 0 && System.currentTimeMillis() < deadline) {
        Thread.sleep(IDLE_SLEEP_MS)
      }
      val fmt = Native.feedAudioFormat()
      if (fmt < 0) {
        log("the card has no sound, so this one will be pictures only")
        return
      }
      val rate = (fmt shr 32).toInt()
      val channels = (fmt and 0xFFFF).toInt()
      // A rate that is a whole number of times SOUND_RATE is brought down to it,
      // because that is the one number a phone's AAC encoder is certain to take.
      val out =
          if (rate > SOUND_RATE && rate % SOUND_RATE == 0) SOUND_RATE else rate
      val resampler = Decimator(rate, out, channels)
      audio =
          runCatching { makeAudioEncoder(out, channels) }
              .getOrElse {
                Log.w(TAG, "no sound encoder would start: ${it.message}")
                null
              }
      if (audio == null) {
        log("this phone would not encode the card's sound, so this one has no sound")
        return
      }
      audioDone = false
      audioOutRate = out
      audioFrameBytes = channels * 2
      down = resampler
      if (out == rate) {
        log("the card's sound is $rate Hz and is encoded at that rate")
      } else {
        log("the card's sound is $rate Hz, so it is brought down to $out Hz first")
      }
      primeSound()
    }

    /**
     * A track cannot be added to a file that has already started, and the sound
     * encoder only says what its output looks like once it has been given
     * something to work on. So it is handed a moment of silence here: the track
     * is then known before the first picture is encoded, and the file holds every
     * picture and every sound from the first one instead of losing the opening
     * seconds while the two are still being introduced to each other.
     */
    private fun primeSound() {
      val codec = audio ?: return
      val index = dequeueInput(codec, 500_000)
      if (index < 0) return
      val buffer = codec.getInputBuffer(index) ?: return
      val size = minOf(buffer.capacity(), audioOutRate / 20 * audioFrameBytes) // 50ms
      if (size < audioFrameBytes) return
      buffer.clear()
      for (i in 0 until size) buffer.put(0)
      codec.queueInputBuffer(index, 0, size, 0L, 0)
      val deadline = System.currentTimeMillis() + 500
      while (audioTrack < 0 && System.currentTimeMillis() < deadline) drainAudio()
      primed = true
    }

    private fun waitForFirstPicture(): ByteArray {
      val started = System.currentTimeMillis()
      var deadline = started + FIRST_PICTURE_MS
      while (true) {
        if (Native.feedPeekPts(VIDEO) >= 0) {
          val pts = Native.feedPeekPts(VIDEO)
          val n = videoBuf.read(VIDEO)
          if (n > 0) {
            firstPts = pts
            return videoBuf.bytes.copyOf(n)
          }
        } else if (Native.feedEnded()) {
          throw IllegalStateException("the camera stopped before the first picture")
        }
        val now = System.currentTimeMillis()
        if (now >= deadline) {
          val stats = LongArray(8)
          val written = Native.feedStats(stats)
          val sent = if (written >= 8) stats[7] else -1L
          val read = if (written >= 1) stats[0] else -1L
          if (sent < 0) {
            // The counts could not be read, so there is nothing to conclude from
            // them and only the clock may end this.
            if (now - started < CARD_START_MS) {
              deadline = started + CARD_START_MS
            } else {
              throw IllegalStateException("no picture came from the card in ${now - started}ms")
            }
          } else if (sent == 0L && now - started < CARD_START_MS) {
            // Nothing has come out of the card at all, so it is still starting.
            log("the card has not sent a picture yet (${now - started}ms); waiting for it")
            deadline = started + CARD_START_MS
          } else {
            // Either the card is not sending at all, or it is and the reader is
            // missing it. Both belong in the log rather than in a guess.
            log("no first picture after ${now - started}ms: the card has sent $sent, the reader has $read")
            throw IllegalStateException(
                if (sent == 0L) "the card sent no pictures at all"
                else "the card is sending ($sent pictures) but none reached the recorder")
          }
        }
        Thread.sleep(IDLE_SLEEP_MS)
      }
    }

    // ------------------------------------------------------------- the thread

    private fun pump() {
      val codec = video ?: return
      try {
        firstFrame?.let { jpeg ->
          if (queue(codec, jpeg, jpeg.size, firstPts)) frames++
        }
        firstFrame = null
        var saidGoodbye = false
        while (true) {
          // A card that stopped sending leaves nothing to wait for, so the file is
          // finished here rather than left half written.
          val over = Native.feedEnded()
          if (over && !saidGoodbye) {
            saidGoodbye = true
            log("the camera stopped, so the file is finished here")
          }
          if (stopping || over) {
            queueEndsOfStream()
          } else {
            takePicture(codec)
            takeSound()
          }
          drainVideo()
          drainAudio()
          if ((stopping || over) && videoDone && audioDone) break
        }
        finish()
      } catch (e: Exception) {
        fail("the H.264 recorder stopped", e)
      }
    }

    /** Reads one picture, converts it and hands it to the encoder. */
    private fun takePicture(codec: MediaCodec) {
      if (Native.feedPeekPts(VIDEO) < 0) return
      val pts = Native.feedPeekPts(VIDEO)
      val n = videoBuf.read(VIDEO)
      if (n <= 0) return
      val index = dequeueInput(codec, 2_000_000)
      if (index < 0) {
        // The encoder is busy and this picture is stale now. It goes; the next
        // one carries the next timestamp, so the file still plays in step.
        skipped++
        return
      }
      if (queue(codec, videoBuf.bytes, n, pts, index)) frames++ else skipped++
    }

    private fun takeSound() {
      val codec = audio ?: return
      val filter = down
      if (audioHeld == 0) {
        if (Native.feedPeekPts(AUDIO) < 0) return
        val pts = Native.feedPeekPts(AUDIO)
        val n = audioBuf.read(AUDIO)
        if (n < audioFrameBytes) return
        // A frame that is only half there cannot be resampled, and the card
        // always sends whole ones, so this only ever costs nothing.
        audioHeld = n - n % audioFrameBytes
        audioAt = 0
        // The resampled sound is timed from this chunk's own clock and counted
        // in frames, so a rate that does not divide evenly cannot drift, and a
        // gap in the card's sound is a gap in the file.
        audioPtsBase = pts
        audioPtsAt = filter.frames
      }
      while (audioHeld > 0) {
        val index = dequeueInput(codec, 1_000_000)
        // The encoder is busy, so the rest of this chunk waits for the next pass
        // rather than being thrown away: the sound stays whole.
        if (index < 0) return
        val buffer = codec.getInputBuffer(index) ?: return
        val before = filter.frames
        val (used, written) =
            filter.convert(
                audioBuf.bytes,
                audioAt,
                audioHeld,
                resampled,
                minOf(buffer.capacity(), resampled.size)
            )
        // Every buffer carries the time of its own first sample, so a chunk that
        // is handed over in pieces still lands where it belongs. What was read
        // is counted whether or not it turned into sound this time, since a part
        // of a group waits inside the filter for the sound that follows it.
        val at = audioPtsBase + (before - audioPtsAt) * 1_000_000L / audioOutRate
        audioAt += used
        audioHeld -= used
        if (written == 0) {
          // Nothing came out this time, so the buffer is handed straight back
          // rather than held on to: the codec is waiting for it either way.
          codec.queueInputBuffer(index, 0, 0, 0L, 0)
          return
        }
        buffer.clear()
        buffer.put(resampled, 0, written)
        codec.queueInputBuffer(index, 0, written, at, 0)
        lastAudioPts = at + written.toLong() / audioFrameBytes * 1_000_000L / audioOutRate
      }
    }

    /** Decodes a JPEG into the encoder's picture, as YUV. */
    private fun queue(
        codec: MediaCodec,
        jpeg: ByteArray,
        len: Int,
        pts: Long,
        index: Int = -1
    ): Boolean {
      val slot = if (index >= 0) index else dequeueInput(codec, 2_000_000)
      if (slot < 0) return false
      var picture: Bitmap? = null
      var queued = false
      return try {
        val decoded = BitmapFactory.decodeByteArray(jpeg, 0, len) ?: return false
        val scaled =
            if (decoded.width == width && decoded.height == height) {
              decoded
            } else {
              val small = Bitmap.createScaledBitmap(decoded, width, height, true)
              if (small !== decoded) decoded.recycle()
              small
            }
        picture = scaled
        if (argb.size != width * height) argb = IntArray(width * height)
        scaled.getPixels(argb, 0, width, 0, 0, width, height)
        val image = codec.getInputImage(slot) ?: return false
        if (image.width != width || image.height != height) {
          throw IllegalStateException("the encoder asked for ${image.width}x${image.height}")
        }
        toYuv(image)
        codec.queueInputBuffer(slot, 0, width * height * 3 / 2, pts, 0)
        queued = true
        lastVideoPts = pts
        true
      } finally {
        picture?.recycle()
        // A buffer the encoder handed out and did not get back is one the encoder
        // can never use again, so it is returned even when this picture failed.
        if (!queued) runCatching { codec.queueInputBuffer(slot, 0, 0, 0, 0) }
      }
    }

    /** One pass over the picture into the planes the encoder wants. */
    private fun toYuv(image: Image) {
      val planes = image.planes
      val w = image.width
      val h = image.height
      when (planes.size) {
        3 -> { // luma, blue difference, red difference
          fillLuma(planes[0], w, h)
          fillChroma(planes[1], w, h, BLUE)
          fillChroma(planes[2], w, h, RED)
        }
        2 -> { // some devices keep the two colours interleaved in one plane
          fillLuma(planes[0], w, h)
          fillColours(planes[1], w, h)
        }
        else -> throw IllegalStateException("the encoder asked for ${planes.size} planes")
      }
    }

    private fun fillLuma(plane: Image.Plane, w: Int, h: Int) {
      val buffer = plane.buffer
      val rowStride = plane.rowStride
      val pixelStride = plane.pixelStride
      var row = buffer.position()
      for (y in 0 until h) {
        var at = row
        var color = y * w
        for (x in 0 until w) {
          val rgb = argb[color++]
          val r = (rgb shr 16) and 0xFF
          val g = (rgb shr 8) and 0xFF
          val b = rgb and 0xFF
          // BT.601, the range a player expects.
          buffer.put(at, (((66 * r + 129 * g + 25 * b + 128) shr 8) + 16).coerceIn(0, 255).toByte())
          at += pixelStride
        }
        row += rowStride
      }
    }

    /** The blue-difference plane, or the red-difference one. */
    private fun fillChroma(plane: Image.Plane, w: Int, h: Int, which: Int) {
      val buffer = plane.buffer
      val rowStride = plane.rowStride
      val pixelStride = plane.pixelStride
      var row = buffer.position()
      for (y in 0 until (h + 1) / 2) {
        var at = row
        // One of every two pixels, and the top row of every two: taking the
        // nearest one is what a 4:2:0 picture is, and it costs a quarter of the
        // work of averaging.
        var color = (y * 2) * w
        for (x in 0 until (w + 1) / 2) {
          val rgb = argb[color]
          val r = (rgb shr 16) and 0xFF
          val g = (rgb shr 8) and 0xFF
          val b = rgb and 0xFF
          val value =
              if (which == BLUE) {
                ((-38 * r - 74 * g + 112 * b + 128) shr 8) + 128
              } else {
                ((112 * r - 94 * g - 18 * b + 128) shr 8) + 128
              }
          buffer.put(at, value.coerceIn(1, 255).toByte())
          at += pixelStride
          color += 2
        }
        row += rowStride
      }
    }

    /** The two colours interleaved in one plane, blue difference first. */
    private fun fillColours(plane: Image.Plane, w: Int, h: Int) {
      val buffer = plane.buffer
      val rowStride = plane.rowStride
      val pixelStride = plane.pixelStride
      var row = buffer.position()
      for (y in 0 until (h + 1) / 2) {
        var at = row
        var color = (y * 2) * w
        for (x in 0 until (w + 1) / 2) {
          val rgb = argb[color]
          val r = (rgb shr 16) and 0xFF
          val g = (rgb shr 8) and 0xFF
          val b = rgb and 0xFF
          buffer.put(
              at, (((-38 * r - 74 * g + 112 * b + 128) shr 8) + 128).coerceIn(0, 255).toByte())
          buffer.put(
              at + pixelStride,
              (((112 * r - 94 * g - 18 * b + 128) shr 8) + 128).coerceIn(0, 255).toByte())
          at += pixelStride * 2
          color += 2
        }
        row += rowStride
      }
    }

    // ------------------------------------------------------------- closing up

    private fun queueEndsOfStream() {
      val codec = video ?: return
      if (!videoEosQueued) {
        val index = dequeueInput(codec, 5_000_000)
        if (index >= 0) {
          videoEosQueued = true
          codec.queueInputBuffer(
              index, 0, 0, lastVideoPts + FRAME_US, MediaCodec.BUFFER_FLAG_END_OF_STREAM)
        }
      }
      val sound = audio ?: return
      if (!audioEosQueued) {
        val index = dequeueInput(sound, 2_000_000)
        if (index >= 0) {
          audioEosQueued = true
          sound.queueInputBuffer(
              index, 0, 0, lastAudioPts + FRAME_US, MediaCodec.BUFFER_FLAG_END_OF_STREAM)
        }
      }
    }

    private fun drainVideo() {
      val codec = video ?: return
      val info = MediaCodec.BufferInfo()
      while (true) {
        val index = codec.dequeueOutputBuffer(info, CODEC_TIMEOUT_US)
        when {
          index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> {
            if (videoTrack < 0) {
              videoTrack = muxer.addTrack(codec.outputFormat)
              startMuxerIfReady()
            }
          }
          index >= 0 -> {
            val buffer = codec.getOutputBuffer(index)
            val config = info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0
            if (buffer != null && info.size > 0 && !config) {
              if (muxerStarted) {
                // The header belongs in the track format, not in a sample.
                buffer.position(info.offset)
                buffer.limit(info.offset + info.size)
                muxer.writeSampleData(videoTrack, buffer, info)
              } else {
                tooEarly++
              }
            }
            codec.releaseOutputBuffer(index, false)
            if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) {
              videoDone = true
              return
            }
          }
          else -> return
        }
      }
    }

    private fun drainAudio() {
      val codec = audio ?: return
      val info = MediaCodec.BufferInfo()
      while (true) {
        val index = codec.dequeueOutputBuffer(info, CODEC_TIMEOUT_US)
        when {
          index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> {
            val format = codec.outputFormat
            // An encoder that answers with a rate other than the one it was asked
            // for has made a file that will not play right, so it is said out loud.
            val gave =
                if (format.containsKey(MediaFormat.KEY_SAMPLE_RATE)) {
                  format.getInteger(MediaFormat.KEY_SAMPLE_RATE)
                } else {
                  -1
                }
            if (gave != audioOutRate) {
              log("the sound encoder gave $gave Hz for the $audioOutRate Hz it was asked for")
            }
            if (audioTrack < 0) {
              audioTrack = muxer.addTrack(format)
              startMuxerIfReady()
            }
          }
          index >= 0 -> {
            val buffer = codec.getOutputBuffer(index)
            val config = info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0
            if (buffer != null && info.size > 0 && !config) {
              if (muxerStarted) {
                buffer.position(info.offset)
                buffer.limit(info.offset + info.size)
                muxer.writeSampleData(audioTrack, buffer, info)
              } else if (primed) {
                tooEarly++
              }
            }
            codec.releaseOutputBuffer(index, false)
            if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) {
              audioDone = true
              return
            }
          }
          else -> return
        }
      }
    }

    private fun startMuxerIfReady() {
      if (muxerStarted || videoTrack < 0) return
      if (audio != null && audioTrack < 0) return // the sound track is still coming
      muxerStarted = true
      muxer.start()
    }

    /** A free input buffer, giving the encoder room to hand out samples first. */
    private fun dequeueInput(codec: MediaCodec, timeoutUs: Long): Int {
      val index = codec.dequeueInputBuffer(timeoutUs)
      if (index >= 0) return index
      drainVideo()
      drainAudio()
      return codec.dequeueInputBuffer(0)
    }

    /**
     * Writes the file out, or throws it away when there is nothing in it, and
     * hands the result to whoever asks next.
     */
    private fun finish() {
      try {
        if (frames == 0) {
          log("nothing was captured, so no file was written")
        } else {
          if (!muxerStarted) {
            throw IllegalStateException("this phone's encoders never produced a file")
          }
          muxer.stop()
          val final = File(part.parentFile, part.name.removeSuffix(".part"))
          if (final.exists() && !final.delete()) {
            throw IllegalStateException("${final.absolutePath} is in the way")
          }
          if (!part.renameTo(final)) {
            throw IllegalStateException("could not move the file into place")
          }
          finished = final
          log("wrote ${final.name}: $frames pictures, ${final.length()} bytes")
        }
      } catch (e: Exception) {
        fail("the file could not be finished", e)
      } finally {
        // What the card sent against what went into the file: the difference is
        // either this phone being slow or the card's own mode being the limit.
        // This has to be asked for while the card is still being listened to,
        // because once it is disarmed there is nothing left to count.
        val seen = LongArray(8)
        val known = Native.feedStats(seen)
        // Nothing will be read from the card again, so it can stop copying.
        runCatching { Native.feedDisarm() }
        if (finished == null) part.delete() // nothing worth keeping
        if (known >= 4) {
          log("the card sent ${seen[0]} pictures and ${seen[2]} sound chunks, $frames were encoded")
        }
        if (tooEarly > 0) log("$tooEarly encoded sample(s) were dropped, the file was not ready for them")
        if (session === this) session = null
        pending = Finished(finished, frames, seconds(), skipped, failure)
        dispose()
      }
    }

    internal fun seconds(): Long = (System.currentTimeMillis() - startedAt) / 1000

    private fun fail(why: String, e: Exception) {
      if (failure == null) {
        failure = why + ": " + (e.message ?: e.javaClass.simpleName)
        Log.e(TAG, why, e)
        Util.appendLog(context, failure!!)
      }
    }

    /** Lets go of the encoders and the muxer, and of a file that is not one. */
    fun dispose() {
      if (::muxer.isInitialized) runCatching { muxer.release() }
      runCatching { video?.stop() }
      runCatching { video?.release() }
      runCatching { audio?.stop() }
      runCatching { audio?.release() }
      video = null
      audio = null
    }

    /** Stops, waits for the file to be sealed and hands back what happened. */
    fun seal(): Finished {
      stopping = true
      val seconds = (System.currentTimeMillis() - startedAt) / 1000
      val thread = worker
      thread?.join(STOP_WAIT_MS)
      if (thread?.isAlive == true) {
        // The encoders are still draining. Only now may the Rust side forget about
        // us, and the worker is left to write the file by itself.
        Native.feedDisarm()
        return Finished(
            null,
            frames,
            seconds,
            skipped,
            failure ?: "the encoders did not finish in time; the file is still being written")
      }
      worker = null
      Native.feedDisarm()
      val file = finished
      if (file == null) part.delete() // nothing worth keeping
      return Finished(file, frames, seconds, skipped, failure)
    }
  }

  /** A byte array that grows itself when the feed has something bigger. */
  private class Buffer(var bytes: ByteArray) {
    /** Reads the next item and returns how many bytes are in it, or 0. */
    fun read(kind: Int): Int {
      while (true) {
        val n = Native.feedPull(kind, bytes)
        if (n == TOO_SMALL) {
          bytes = ByteArray(bytes.size * 2)
          continue
        }
        return if (n > NOTHING) n else 0
      }
    }
  }

  // -------------------------------------------------------------- the sound

  /**
   * Brings a card's sound down to a rate the phone's encoder is certain to take,
   * by taking every [factor] frames for one.
   *
   * A card that sends 96 kHz is not asking for 96 kHz AAC. The encoder will
   * usually say yes, hand back 48 kHz in its output format, and the file plays
   * the sound at the wrong speed with a whistle folded on top of it, because
   * everything above 24 kHz has come back down as something else. So the rate
   * is lowered first, and only by a whole number of times, so no fraction of a
   * sample is ever invented.
   *
   * The filter is a 33-tap windowed sinc that keeps everything below 21.6 kHz
   * and throws the rest away, and it carries its last frames from one call to
   * the next, so a chunk boundary is a cut in the stream and not a step in it.
   */
  private class Decimator(inRate: Int, outRate: Int, private val channels: Int) {
    /** Frames in for each frame out. */
    val factor = inRate / outRate

    /**
     * The filter. A single tap when there is nothing to bring down, so that a
     * card whose rate is already right goes through the same path untouched.
     */
    private val taps = if (factor > 1) windowedSinc(inRate, outRate) else floatArrayOf(1f)

    /**
     * The last frames in, oldest first, then the group arriving now, and then
     * room for a group that was not all there and has to wait for the next call.
     */
    private val line = FloatArray((taps.size - 1 + 2 * factor - 1) * channels)

    /** Frames held in [line] that are not yet in a whole group. */
    private var spare = 0

    /** Output frames handed out so far, which is what a timestamp is counted in. */
    var frames = 0L
      private set

    /**
     * Reads the whole frames in the [len] bytes of [inBuf] from [off] and writes
     * the sound brought down into [out] as the little-endian samples the encoder
     * takes, at most [outMax] bytes of it. Answers the bytes of [inBuf] it read
     * and the bytes it wrote. A group that was not all there is held for the next
     * call rather than thrown away, so that a chunk of any size at all gives the
     * same sound as one of any other size.
     */
    fun convert(inBuf: ByteArray, off: Int, len: Int, out: ByteArray, outMax: Int): Pair<Int, Int> {
      val frameBytes = channels * 2
      val history = taps.size - 1
      val there = history * channels
      val avail = len / frameBytes
      if (avail <= 0) return 0 to 0
      val groups = minOf((spare + avail) / factor, outMax / frameBytes)
      val forGroups = groups * factor - spare
      // A part of one more group at most, so that a chunk which does not divide
      // evenly leaves what it could not use here instead of losing it.
      val tail = minOf(avail - forGroups, factor - 1 - spare).coerceAtLeast(0)
      val wanted = forGroups + tail
      if (wanted <= 0) return 0 to 0
      var read = off
      var held = spare
      var written = 0
      for (g in 0 until groups) {
        // The group goes behind the frames the filter already holds, so that the
        // window is the whole span one output frame is made of.
        while (held < factor) {
          for (c in 0 until channels) {
            line[there + held * channels + c] = sampleAt(inBuf, read + c * 2)
          }
          held++
          read += frameBytes
        }
        for (c in 0 until channels) {
          var sum = 0f
          // Tap zero is the newest frame, which is the last one of the group.
          for (n in taps.indices) {
            sum += taps[n] * line[(history + factor - 1 - n) * channels + c]
          }
          putSample(out, written, sum)
          written += 2
        }
        // The oldest frames of the window have been used up, so the newest take
        // their place and the next group starts from there.
        System.arraycopy(line, factor * channels, line, 0, there)
        held = 0
      }
      for (i in 0 until tail) {
        for (c in 0 until channels) {
          line[there + i * channels + c] = sampleAt(inBuf, read + c * 2)
        }
        read += frameBytes
      }
      spare = tail
      frames += groups
      return read - off to written
    }

    private fun sampleAt(buf: ByteArray, at: Int): Float =
        ((buf[at].toInt() and 0xFF) or (buf[at + 1].toInt() shl 8)).toShort().toFloat()

    /** One sample, rounded and clipped, as the two little-endian bytes it is. */
    private fun putSample(out: ByteArray, at: Int, v: Float) {
      val s = if (v > MAX_SAMPLE) MAX_SAMPLE else if (v < MIN_SAMPLE) MIN_SAMPLE else v.roundToInt()
      out[at] = (s and 0xFF).toByte()
      out[at + 1] = (s shr 8 and 0xFF).toByte()
    }

    companion object {
      private const val TAPS = 33
      private const val MAX_SAMPLE = 32_767
      private const val MIN_SAMPLE = -32_768

      /**
       * A low-pass that keeps the sound and drops the part that would fold back
       * down. The edge sits a little under half the lower rate, which is where
       * nothing that anybody can hear has reached yet, and a Hamming window
       * keeps the stopband far enough down that what does get through cannot be
       * heard on its own.
       */
      private fun windowedSinc(inRate: Int, outRate: Int): FloatArray {
        val cut = (0.45 * outRate / inRate).toFloat()
        val taps = FloatArray(TAPS)
        var total = 0f
        for (i in 0 until TAPS) {
          val x = i - (TAPS - 1) / 2
          val sinc = if (x == 0) 2f * cut else (sin(2 * PI * cut * x) / (PI * x)).toFloat()
          taps[i] = (sinc * (0.54 - 0.46 * cos(2 * PI * i / (TAPS - 1)))).toFloat()
          total += taps[i]
        }
        // Normalised, so the sound comes out as loud as it went in.
        for (i in 0 until TAPS) taps[i] /= total
        return taps
      }
    }
  }

  // ---------------------------------------------------------------- the encoders

  private const val BLUE = 0
  private const val RED = 1

  /** The picture shapes an encoder will take from us, best first. */
  private val YUV_FORMATS =
      intArrayOf(
          MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible,
          MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420SemiPlanar,
          MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Planar)

  /** The size to encode at: the card's own, but never wider than this. */
  private fun fit(w: Int, h: Int): Pair<Int, Int> {
    var width = w
    var height = h
    if (width > MAX_WIDTH) {
      height = (height.toLong() * MAX_WIDTH / width).toInt()
      width = MAX_WIDTH
    }
    // Encoders want even numbers, and rounding down costs a row or a column.
    return Pair((width - width % 2).coerceAtLeast(2), (height - height % 2).coerceAtLeast(2))
  }

  private fun fpsHint(): Int {
    val fps = Native.feedVideoFps()
    return if (fps in 5.0..120.0) fps.toInt() else 30
  }

  private fun makeVideoEncoder(w: Int, h: Int, fps: Int): MediaCodec {
    // A starting bitrate of about a tenth of a bit per pixel per frame, which is
    // in the right region for a picture of this size; the encoder decides the
    // rest from the bitrate window.
    val bitrate = ((w.toLong() * h * fps) / 10).coerceIn(200_000L, 8_000_000L).toInt()
    val list = MediaCodecList(MediaCodecList.REGULAR_CODECS)
    for (info in list.codecInfos) {
      if (!info.isEncoder) continue
      if (info.supportedTypes.none { it.equals("video/avc", ignoreCase = true) }) continue
      val caps = runCatching { info.getCapabilitiesForType("video/avc") }.getOrNull() ?: continue
      val color = YUV_FORMATS.firstOrNull { wanted -> caps.colorFormats.any { it == wanted } }
          ?: continue
      val format = MediaFormat.createVideoFormat("video/avc", w, h).apply {
        setInteger(MediaFormat.KEY_COLOR_FORMAT, color)
        setInteger(MediaFormat.KEY_BIT_RATE, bitrate)
        setInteger(MediaFormat.KEY_FRAME_RATE, fps)
        setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, I_FRAME_SECONDS)
      }
      val codec = runCatching {
            val made = MediaCodec.createByCodecName(info.name)
            made.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
            made.start()
            made
          }
          .getOrElse {
            Log.w(TAG, "${info.name} would not start: ${it.message}")
            null
          }
          ?: continue
      Log.i(TAG, "encoding with ${info.name} at $bitrate bit/s, colour format $color")
      return codec
    }
    throw IllegalStateException("this phone has no H.264 encoder that takes pictures")
  }

  private fun makeAudioEncoder(rate: Int, channels: Int): MediaCodec {
    val codec = MediaCodec.createEncoderByType("audio/mp4a-latm")
    val format = MediaFormat.createAudioFormat("audio/mp4a-latm", rate, channels).apply {
      setInteger(MediaFormat.KEY_AAC_PROFILE, MediaCodecInfo.CodecProfileLevel.AACObjectLC)
      setInteger(MediaFormat.KEY_BIT_RATE, AUDIO_BITRATE)
      setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 32 * 1024)
      setInteger(MediaFormat.KEY_PCM_ENCODING, AudioFormat.ENCODING_PCM_16BIT)
    }
    codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
    codec.start()
    return codec
  }
}
