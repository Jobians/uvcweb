#![allow(non_snake_case)] // JNI names are dictated by the Java class: Java_<package>_<class>_<method>

//! JNI entry points for the Android app (see `android/`). Only compiled for Android.
//!
//! They take **primitive arguments only** - with one exception, the encoder
//! feed, where the app hands us a `byte[]` to fill. That needs two functions out
//! of the JNI table, declared by hand below so the crate still has no
//! dependencies. Matching Kotlin:
//!
//! ```kotlin
//! package com.uvcweb.app
//! object Native {
//!     init { System.loadLibrary("uvcweb_core") }
//!     @JvmStatic external fun start(fd: Int, width: Int, height: Int, fps: Int, audio: Boolean,
//!                                   audioRate: Int, audioChannels: Int, lan: Boolean,
//!                                   webPort: Int, rtspPort: Int, avOffsetMs: Int): Int
//!     @JvmStatic external fun stop()
//!     @JvmStatic external fun isRunning(): Boolean
//!     @JvmStatic external fun startRecord(): Int
//!     @JvmStatic external fun stopRecord(): Int
//!     @JvmStatic external fun isRecording(): Boolean
//!     @JvmStatic external fun recordFrames(): Int
//!     @JvmStatic external fun recordSeconds(): Int
//!     @JvmStatic external fun recordMegabytes(): Int
//!     @JvmStatic external fun feedArm(): Int
//!     @JvmStatic external fun feedDisarm()
//!     @JvmStatic external fun feedAttached(): Boolean
//!     @JvmStatic external fun feedEnded(): Boolean
//!     @JvmStatic external fun feedPeekPts(kind: Int): Long
//!     @JvmStatic external fun feedPull(kind: Int, into: ByteArray): Int
//!     @JvmStatic external fun feedAudioFormat(): Long      // rate << 32 | channels, -1 if none
//!     @JvmStatic external fun feedVideoFps(): Double
//!     @JvmStatic external fun feedStats(): LongArray
//! ```

//!
//! Logging: the app sets the environment variable UVCWEB_LOG_FILE (android.system.Os.setenv)
//! before `start`; lines also go to logcat under the tag "uvcweb".

use crate::config::Config;
use crate::engine::Engine;
use crate::feed;
use std::os::raw::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

// The JNI table lives behind the env pointer, so its entries are only reachable
// through the runtime. Android's libart exports these two by name (which is how
// the `jni` crate reaches them as well), so they can simply be called: it saves
// carrying a JNIEnv through the Rust code that needs no other part of it.
extern "system" {
    /// Length of a Java array, in elements.
    fn GetArrayLength(env: *mut c_void, array: *mut c_void) -> i32;
    /// Copies `len` bytes into a Java byte array, from `start`.
    fn SetByteArrayRegion(
        env: *mut c_void,
        array: *mut c_void,
        start: i32,
        len: i32,
        buf: *const u8,
    );
    fn SetLongArrayRegion(
        env: *mut c_void,
        array: *mut c_void,
        start: i32,
        len: i32,
        buf: *const i64,
    );
}

thread_local! {
    /// The app's own buffer is where a picture is really written; this is only
    /// the space to copy it out of. One per reading thread, and never freed.
    static SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);
// Mirrors `ENGINE.is_some()` without taking the lock: `start` holds the lock while the camera opens.
static RUNNING: AtomicBool = AtomicBool::new(false);

const ERR_ALREADY_RUNNING: i32 = -100;
const ERR_PANIC: i32 = -101;
const ERR_NOT_RUNNING: i32 = -102; // no session, so nothing to record from
const ERR_BAD_DIR: i32 = -103; // the record directory could not be used
const ERR_NO_FILES: i32 = -104; // the recording produced no file

/// Returns 0 on success, otherwise the same error codes as the command line program's exit status:
/// 1 uvc_init, 2 open/wrap (bad fd), 3 no such video mode, 4 streaming failed, 5 port in use;
/// -100 already running, -101 internal error.
/// `width`/`height` 0 = the card's default mode. A port of 0 disables that protocol.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_start(
    _env: *mut c_void,
    _class: *mut c_void,
    fd: i32,
    width: i32,
    height: i32,
    fps: i32,
    audio: u8,
    audio_rate: i32,
    audio_channels: i32,
    lan: u8,
    web_port: i32,
    rtsp_port: i32,
    av_offset_ms: i32,
) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut guard = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return ERR_ALREADY_RUNNING;
        }
        let mut cfg = Config::for_fd(fd);
        cfg.width = width.max(0) as u32;
        cfg.height = height.max(0) as u32;
        cfg.fps = fps.max(0) as u32;
        cfg.audio = audio != 0;
        if audio_rate > 0 {
            cfg.audio_rate = audio_rate as u32;
        }
        if audio_channels > 0 {
            cfg.audio_channels = audio_channels as u16;
        }
        cfg.lan = lan != 0;
        cfg.av_offset_ms = av_offset_ms;
        cfg.protocols.clear();
        if web_port > 0 {
            cfg.protocols.push(("web".to_string(), web_port as u16));
        }
        if rtsp_port > 0 {
            cfg.protocols.push(("rtsp".to_string(), rtsp_port as u16));
        }
        match Engine::start(cfg) {
            Ok(engine) => {
                *guard = Some(engine);
                RUNNING.store(true, Ordering::SeqCst);
                0
            }
            Err(code) => code,
        }
    }));
    result.unwrap_or(ERR_PANIC)
}

/// Stops the session and waits until the camera is released and the ports are free. Safe to call any time.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_stop(_env: *mut c_void, _class: *mut c_void) {
    let taken = {
        let mut guard = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        guard.take()
    };
    RUNNING.store(false, Ordering::SeqCst);
    if let Some(engine) = taken {
        let _ = catch_unwind(AssertUnwindSafe(move || engine.stop()));
    }
}

#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_isRunning(
    _env: *mut c_void,
    _class: *mut c_void,
) -> u8 {
    if RUNNING.load(Ordering::SeqCst) {
        1
    } else {
        0
    }
}

/// Starts a recording of everything the session streams into the directory named by
/// UVCWEB_RECORD_DIR (the app sets it before `start`).
/// Returns 0 on success, otherwise one of the error codes above.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_startRecord(
    _env: *mut c_void,
    _class: *mut c_void,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| match crate::recorder::start("") {
        Ok(()) => 0,
        Err(e) => {
            say!("record asked for by the app: {}", e);
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                ERR_ALREADY_RUNNING
            } else if e.kind() == std::io::ErrorKind::NotConnected {
                ERR_NOT_RUNNING
            } else {
                ERR_BAD_DIR
            }
        }
    }))
    .unwrap_or(ERR_PANIC)
}

/// Stops the recording, which writes the index and closes the file so a player accepts
/// it. Returns how many pictures the recording holds, or a negative error code when
/// there was nothing to close. The app can then look in its record directory for the
/// new file.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_stopRecord(
    _env: *mut c_void,
    _class: *mut c_void,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| match crate::recorder::stop() {
        Some(sum) if !sum.files.is_empty() => {
            say!("{}", sum.describe());
            sum.frames.min(i32::MAX as u64) as i32
        }
        Some(_) => ERR_NO_FILES,
        None => ERR_NOT_RUNNING,
    }))
    .unwrap_or(ERR_PANIC)
}

/// Whether a recording is running right now.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_isRecording(
    _env: *mut c_void,
    _class: *mut c_void,
) -> u8 {
    u8::from(crate::recorder::status().recording())
}

/// Pictures written into the running recording so far (0 when idle).
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_recordFrames(
    _env: *mut c_void,
    _class: *mut c_void,
) -> i32 {
    match crate::recorder::status() {
        crate::recorder::Status::Recording { frames, .. } => frames.min(i32::MAX as u64) as i32,
        _ => 0,
    }
}

/// Seconds the running recording has been going (0 when idle).
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_recordSeconds(
    _env: *mut c_void,
    _class: *mut c_void,
) -> i32 {
    match crate::recorder::status() {
        crate::recorder::Status::Recording { secs, .. } => secs.min(i32::MAX as u64) as i32,
        _ => 0,
    }
}

/// Megabytes written into the running recording so far (0 when idle).
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_recordMegabytes(
    _env: *mut c_void,
    _class: *mut c_void,
) -> i32 {
    match crate::recorder::status() {
        crate::recorder::Status::Recording { bytes, .. } => {
            (bytes / (1024 * 1024)).min(i32::MAX as u64) as i32
        }
        _ => 0,
    }
}

// ---------------------------------------------------------------- the encoder feed
//
// The app's H.264 recorder pulls pictures and sound out of `feed` (see that
// module for why it is separate from the file recorder). Everything here is one
// number out, or bytes into a buffer the app owns and reuses, so a frame costs
// no allocation on this side of the boundary either.

/// Start copying the stream for a reader. 0 on success, otherwise an error code.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_feedArm(
    _env: *mut c_void,
    _class: *mut c_void,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| match feed::arm() {
        Ok(()) => 0,
        Err(e) => {
            say!("the encoder feed was asked for: {}", e);
            if e.kind() == std::io::ErrorKind::NotConnected {
                ERR_NOT_RUNNING
            } else {
                ERR_ALREADY_RUNNING
            }
        }
    }))
    .unwrap_or(ERR_PANIC)
}

/// Stop copying. Anything still queued is dropped, so the app pulls the tail it
/// wants before calling this.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_feedDisarm(_env: *mut c_void, _class: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(feed::disarm));
}

/// True once both copiers are on the live edge. Sound from before that is not
/// part of the recording.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_feedAttached(
    _env: *mut c_void,
    _class: *mut c_void,
) -> u8 {
    u8::from(feed::is_attached())
}

/// True once the capture session ended, so a reader can seal its file.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_feedEnded(
    _env: *mut c_void,
    _class: *mut c_void,
) -> u8 {
    u8::from(feed::has_ended())
}

/// The timestamp in microseconds of the item at the head of a queue, or -1 when
/// there is nothing. Ask first, pull second, from the one thread that reads.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_feedPeekPts(
    _env: *mut c_void,
    _class: *mut c_void,
    kind: i32,
) -> i64 {
    catch_unwind(AssertUnwindSafe(|| feed::peek_pts(kind))).unwrap_or(feed::EMPTY)
}

/// Fills the app's buffer with the item at the head of a queue. Returns how many
/// bytes it wrote, -1 when the queue was empty, or -2 when the buffer was too
/// small - in which case the item stays and the call can be repeated.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_feedPull(
    env: *mut c_void,
    _class: *mut c_void,
    kind: i32,
    into: *mut c_void,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if into.is_null() {
            return feed::EMPTY;
        }
        // The array is the app's, and only this thread touches it, so its length
        // is a safe bound for the copy.
        let room = unsafe { GetArrayLength(env, into) };
        if room <= 0 {
            return feed::EMPTY;
        }
        // One buffer per reading thread, kept between calls: a picture is
        // copied out of here and never kept, so there is nothing to gain from
        // asking the allocator for the same space thirty times a second.
        SCRATCH.with(|scratch| {
            let mut buf = scratch.borrow_mut();
            buf.resize(room as usize, 0);
            let n = feed::pull(kind, &mut buf);
            if n > 0 {
                unsafe {
                    SetByteArrayRegion(env, into, 0, n as i32, buf.as_ptr());
                }
            }
            n
        })
    }))
    .unwrap_or(feed::EMPTY) as i32
}

/// The sound format as `rate << 32 | channels`, or -1 when the session has not
/// settled on one yet.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_feedAudioFormat(
    _env: *mut c_void,
    _class: *mut c_void,
) -> i64 {
    match feed::audio_format() {
        Some(f) => ((f.rate as i64) << 32) | f.channels as i64,
        None => -1,
    }
}

/// The picture rate the session is running at, as a hint for an encoder.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_feedVideoFps(
    _env: *mut c_void,
    _class: *mut c_void,
) -> f64 {
    catch_unwind(AssertUnwindSafe(feed::video_fps)).unwrap_or(0.0)
}

/// `[frames, dropped, chunks, soundBytes, secs, queued, queuedBytes]`, the
/// numbers the app shows while it is recording.
#[no_mangle]
pub extern "C" fn Java_com_uvcweb_app_Native_feedStats(
    env: *mut c_void,
    _class: *mut c_void,
    out: *mut c_void,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out.is_null() {
            return 0;
        }
        let st = feed::stats();
        // Longs, so a long recording never silently stops counting.
        let numbers = [
            st.frames as i64,
            st.dropped as i64,
            st.chunks as i64,
            st.sound_bytes as i64,
            st.secs as i64,
            st.queued as i64,
            st.queued_bytes as i64,
        ];
        if unsafe { GetArrayLength(env, out) } < numbers.len() as i32 {
            return 0;
        }
        unsafe { SetLongArrayRegion(env, out, 0, numbers.len() as i32, numbers.as_ptr()) };
        numbers.len() as i32
    }))
    .unwrap_or(0)
}
