//! uvcweb core: UVC MJPEG capture card -> web browser and/or RTSP, with live audio
//! read from the card's own USB audio interface.
//!
//! Two front ends use this library:
//!   * `src/main.rs`     the `uvcweb` program (Termux / Linux, options on the command line)
//!   * `src/android.rs`  JNI entry points for the Android app in `android/`
//!
//! Both do the same thing: hand an already opened USB file descriptor to
//! [`engine::Engine::start`] together with a [`config::Config`].
//!
//! Module map
//!   capture / usbaudio   read the card, publish into the hub
//!   hub                  latest picture + audio queue; protocols subscribe here
//!   protocols            one file per protocol (web, rtsp); protocols/mod.rs is the plug-in point
//!   recorder             one more subscriber: writes the pictures + audio into an AVI file
//!   feed                 a short, drop-the-oldest copy of the stream for a real-time encoder
//!                         (the Android app's H.264 recorder) to read
//!   jnitable             the JNI function table, reached without naming a JNI symbol
//!                         (the app's own library is not allowed to see libart's)
//!   jpeg / rtp           RTP/JPEG + RTP L16 + RTCP building blocks for RTP based protocols
//!   engine               start / stop / supervise one capture session

#[macro_use]
pub mod log;
pub mod config;
pub mod engine;
/// A bounded copy of the stream for a real-time encoder to read (see the module).
pub mod feed;
pub mod hub;
/// The JNI function table, for the app to fill a Java array from here (see the module).
pub mod jnitable;
pub mod protocols;
pub mod recorder;

mod capture;
mod descriptors;
mod ffi;
mod jpeg;
mod rtp;
mod usbaudio;

#[cfg(target_os = "android")]
mod android;

#[cfg(test)]
#[path = "../tests/unit/golden_tests.rs"]
mod golden_tests;
