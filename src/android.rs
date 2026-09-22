#![allow(non_snake_case)] // JNI names are dictated by the Java class: Java_<package>_<class>_<method>

//! JNI entry points for the Android app (see `android/`). Only compiled for Android.
//!
//! They take **primitive arguments only**, so no JNI environment calls (and no
//! `jni` crate) are needed. Matching Kotlin:
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
//! }
//! ```
//!
//! Logging: the app sets the environment variable UVCWEB_LOG_FILE (android.system.Os.setenv)
//! before `start`; lines also go to logcat under the tag "uvcweb".

use crate::config::Config;
use crate::engine::Engine;
use std::os::raw::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);
// Mirrors `ENGINE.is_some()` without taking the lock: `start` holds the lock while the camera opens.
static RUNNING: AtomicBool = AtomicBool::new(false);

const ERR_ALREADY_RUNNING: i32 = -100;
const ERR_PANIC: i32 = -101;

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
pub extern "C" fn Java_com_uvcweb_app_Native_isRunning(_env: *mut c_void, _class: *mut c_void) -> u8 {
    if RUNNING.load(Ordering::SeqCst) {
        1
    } else {
        0
    }
}
