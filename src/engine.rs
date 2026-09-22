//! One capture session: open the camera, start the USB audio, start the chosen
//! protocols, then supervise (status log, camera watchdog) on a background thread.
//!
//! `Engine::start` / `Engine::stop` may be called repeatedly in one process
//! (the Android app does that); the CLI simply does it once.

use crate::capture::Capture;
use crate::config::Config;
use crate::hub::{self, Hub};
use crate::protocols;
use crate::usbaudio;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub struct Engine {
    hub: Arc<Hub>,
    supervisor: Option<JoinHandle<()>>,
}

impl Engine {
    /// Open the card and start serving. On failure returns the process exit code
    /// (1 uvc_init, 2 open/wrap, 3 no such video mode, 4 streaming, 5 port in use).
    pub fn start(cfg: Config) -> Result<Engine, i32> {
        crate::log::init_file_from_env();
        let cfg = Arc::new(cfg);
        say!("uvcweb starting: fd={}", cfg.fd);

        let hub = Hub::new();
        hub::set_global(Some(hub.clone()));

        let cap = match Capture::open(&cfg) {
            Ok(c) => c,
            Err(code) => {
                hub::set_global(None);
                return Err(code);
            }
        };

        // Audio straight from the card's USB audio interface.
        if cfg.audio {
            if let Err(e) = usbaudio::start(cap.usb_handle(), cfg.audio_rate, cfg.audio_channels) {
                say!("USB audio: {}", e);
                say!("USB audio unavailable - continuing without audio");
            }
        } else {
            say!("audio disabled (-a off)");
        }

        // Start the chosen protocols.
        for (name, port) in &cfg.protocols {
            let info = match protocols::find(name) {
                Some(i) => i,
                None => continue,
            };
            let proto = (info.create)();
            let ctx = protocols::Ctx {
                hub: hub.clone(),
                cfg: cfg.clone(),
                bind: protocols::bind_addr(cfg.lan),
                port: *port,
            };
            if let Err(e) = proto.start(ctx) {
                say!("cannot start {} on port {}: {}", name, port, e);
                hub.request_stop();
                hub.join_tracked();
                usbaudio::stop();
                cap.close();
                hub::set_global(None);
                return Err(5);
            }
        }

        let session = hub.clone();
        let supervisor = std::thread::spawn(move || supervise(session, cap));
        Ok(Engine {
            hub,
            supervisor: Some(supervisor),
        })
    }

    /// Stop everything and wait until the ports are free and the camera is released.
    pub fn stop(mut self) {
        self.hub.request_stop();
        if let Some(t) = self.supervisor.take() {
            let _ = t.join();
        }
        self.hub.join_tracked();
        hub::set_global(None);
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }
}

/// Once a second: log status, and restart the camera stream if it goes quiet.
/// Owns the camera; releases it when the session is stopped.
fn supervise(hub: Arc<Hub>, mut cap: Capture) {
    let start = Instant::now();
    let mut prev_t = start;
    let mut prev_frames = 0u64;
    let mut tick = 0u32;
    let mut stalled = false;
    let mut flagged_same = false;
    let mut stream_t = start; // when the stream was last (re)started
    let mut restarts = 0u32;

    while !hub.is_stopped() {
        std::thread::sleep(Duration::from_millis(100));
        let t = Instant::now();
        let dt = t.duration_since(prev_t).as_secs_f64();
        if dt < 1.0 {
            continue;
        }
        let st = hub.video_stats();
        let fps = (st.total - prev_frames) as f64 / dt;
        hub.set_fps(fps);
        prev_frames = st.total;
        prev_t = t;
        tick += 1;
        let age = st.age.unwrap_or(0.0);

        if st.total == 0 {
            if tick.is_multiple_of(5) {
                say!(
                    "no frames yet after {:.0}s - is the HDMI source on and connected to the card?",
                    t.duration_since(start).as_secs_f64()
                );
            }
        } else if age > 3.0 {
            if !stalled {
                say!("frames STOPPED (nothing for {:.0}s)", age);
                stalled = true;
            }
        } else {
            if stalled {
                say!("frames resumed");
                stalled = false;
            }
            if st.same >= 90 && !flagged_same {
                say!(
                    "last {} frames are identical: static picture or 'no signal' screen",
                    st.same
                );
                flagged_same = true;
            } else if st.same == 0 && flagged_same {
                say!("picture is changing again");
                flagged_same = false;
            }
            if tick.is_multiple_of(5) {
                let mut astr = String::new();
                if hub.audio_format().is_some() {
                    let ab = hub.audio_bytes();
                    if ab > 0 {
                        astr = format!(", audio {} KB", ab / 1024);
                    } else {
                        astr = ", audio: NO DATA".to_string();
                    }
                }
                say!(
                    "ok: {:.1} fps, last frame {} bytes, {} total, {} dropped, {} viewer(s){}",
                    fps,
                    st.last_len,
                    st.total,
                    st.bad,
                    st.viewers,
                    astr
                );
            }
        }

        if hub.audio_format().is_some() {
            usbaudio::poll();
        }

        // watchdog: restart the stream if the card goes quiet
        let last_act = match hub.last_frame_at() {
            Some(lf) if lf > stream_t => lf,
            _ => stream_t,
        };
        let quiet = t.saturating_duration_since(last_act).as_secs_f64();
        if quiet > 4.0 {
            restarts += 1;
            say!(
                "no video for {:.0}s - restarting stream (restart #{})",
                quiet,
                restarts
            );
            if let Err(e) = cap.restart() {
                say!("restart failed: {}", e);
            }
            stream_t = Instant::now();
        }
    }

    // session over: release the audio interface and the camera
    usbaudio::stop();
    cap.close();
}
