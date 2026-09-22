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
//!   jpeg / rtp           RTP/JPEG + RTP L16 + RTCP building blocks for RTP based protocols
//!   engine               start / stop / supervise one capture session

#[macro_use]
pub mod log;
pub mod config;
pub mod engine;
pub mod hub;
pub mod protocols;

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
