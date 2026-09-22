//! The hub sits between the capture side (camera + USB audio) and the protocol
//! servers. Capture code *publishes* frames / audio chunks; every protocol
//! *subscribes* independently. A new protocol only needs the subscribe side.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One JPEG picture from the card.
pub struct VideoFrame {
    pub seq: u64,
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub at: Instant, // when it arrived from the camera
}

/// A block of interleaved S16LE PCM straight from the USB audio interface.
pub struct AudioChunk {
    pub seq: u64,
    pub pos: u64, // sample frames delivered before this chunk (running counter)
    pub data: Vec<u8>,
    pub at: Instant, // when the last sample of the chunk arrived
}

#[derive(Clone, Copy, Debug)]
pub struct AudioFormat {
    pub rate: u32,
    pub channels: u16,
}

pub struct VideoStats {
    pub total: u64,
    pub bad: u64,
    pub same: u64,
    pub last_len: usize,
    pub age: Option<f64>,
    pub viewers: i32,
    pub w: u32,
    pub h: u32,
    pub fps: f64,
}

struct VideoState {
    latest: Option<Arc<VideoFrame>>,
    seq: u64,
    total: u64,
    bad: u64,
    same: u64,
    bytes: u64,
    last_at: Option<Instant>,
    fps: f64,
    viewers: i32,
    viewer_seq: i32,
}

struct AudioState {
    chunks: VecDeque<Arc<AudioChunk>>,
    next_seq: u64,
    next_pos: u64,
    bytes: u64,
    queued: usize,
}

pub struct Hub {
    video: Mutex<VideoState>,
    video_cv: Condvar,
    audio: Mutex<AudioState>,
    audio_cv: Condvar,
    audio_fmt: OnceLock<AudioFormat>,
    stop: AtomicBool,                    // per session: a library can be started again after stop
    threads: Mutex<Vec<JoinHandle<()>>>, // listener threads, joined on shutdown so ports are free again
}

static GLOBAL: Mutex<Option<Arc<Hub>>> = Mutex::new(None);

/// The C callbacks (libuvc / libusb) have no convenient way to carry a pointer
/// to the hub, so the running session's hub is also reachable globally.
/// `None` = no session (callbacks that still arrive then simply drop their data).
pub fn set_global(h: Option<Arc<Hub>>) {
    *lock(&GLOBAL) = h;
}

pub fn try_global() -> Option<Arc<Hub>> {
    lock(&GLOBAL).clone()
}

impl Hub {
    pub fn new() -> Arc<Hub> {
        Arc::new(Hub {
            video: Mutex::new(VideoState {
                latest: None,
                seq: 0,
                total: 0,
                bad: 0,
                same: 0,
                bytes: 0,
                last_at: None,
                fps: 0.0,
                viewers: 0,
                viewer_seq: 0,
            }),
            video_cv: Condvar::new(),
            audio: Mutex::new(AudioState {
                chunks: VecDeque::new(),
                next_seq: 0,
                next_pos: 0,
                bytes: 0,
                queued: 0,
            }),
            audio_cv: Condvar::new(),
            audio_fmt: OnceLock::new(),
            stop: AtomicBool::new(false),
            threads: Mutex::new(Vec::new()),
        })
    }

    // ---------------- shutdown ----------------

    /// Every loop of this session polls this; it turns true when the session is being stopped.
    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Ask every thread of the session to finish (and wake the ones waiting for data).
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // Touch each lock before notifying: a subscriber that has just checked the flag is still
        // holding it until it starts waiting, so it cannot miss this wake-up.
        drop(lock(&self.video));
        self.video_cv.notify_all();
        drop(lock(&self.audio));
        self.audio_cv.notify_all();
    }

    /// Remember a long-running thread (a protocol's accept loop) so shutdown can wait for it.
    pub fn track(&self, handle: JoinHandle<()>) {
        lock(&self.threads).push(handle);
    }

    /// Wait for all tracked threads. Call after `request_stop`.
    pub fn join_tracked(&self) {
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *lock(&self.threads));
        for h in handles {
            let _ = h.join();
        }
    }

    // ---------------- video: publish ----------------

    /// Called for every frame the camera delivers. Rejects empty / non-JPEG frames.
    pub fn submit_frame(&self, d: &[u8], w: u32, h: u32) {
        if d.len() < 1024 || d[0] != 0xFF || d[1] != 0xD8 {
            let bad = {
                let mut g = lock(&self.video);
                g.bad += 1;
                g.bad
            };
            if bad <= 5 {
                say!("dropped bad/empty frame ({} bytes)", d.len());
            }
            return;
        }
        let total = {
            let mut g = lock(&self.video);
            let same = match &g.latest {
                Some(f) => f.data.as_slice() == d,
                None => false,
            };
            g.seq += 1;
            g.total += 1;
            g.bytes += d.len() as u64;
            g.same = if same { g.same + 1 } else { 0 };
            let now = Instant::now();
            g.last_at = Some(now);
            let frame = Arc::new(VideoFrame { seq: g.seq, data: d.to_vec(), width: w, height: h, at: now });
            g.latest = Some(frame);
            g.total
        };
        self.video_cv.notify_all();
        if total == 1 {
            say!("first frame received: {}x{}, {} bytes", w, h, d.len());
        }
    }

    // ---------------- video: subscribe ----------------

    pub fn latest_frame(&self) -> Option<Arc<VideoFrame>> {
        lock(&self.video).latest.clone()
    }

    /// Wait for a frame newer than `*last_seq` (starts at 0 = "give me the current picture").
    /// Slow subscribers simply skip frames; they never queue them.
    pub fn next_frame(&self, last_seq: &mut u64, timeout: Duration) -> Option<Arc<VideoFrame>> {
        let deadline = Instant::now() + timeout;
        let mut g = lock(&self.video);
        loop {
            if self.is_stopped() {
                return None;
            }
            if g.seq > *last_seq {
                if let Some(f) = g.latest.clone() {
                    *last_seq = f.seq;
                    return Some(f);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (ng, _) = self.video_cv.wait_timeout(g, deadline - now).unwrap_or_else(|e| e.into_inner());
            g = ng;
        }
    }

    // ---------------- audio: publish ----------------

    pub fn set_audio_format(&self, f: AudioFormat) {
        let _ = self.audio_fmt.set(f);
    }

    /// None = this run has no audio.
    pub fn audio_format(&self) -> Option<AudioFormat> {
        self.audio_fmt.get().copied()
    }

    pub fn push_audio(&self, data: &[u8]) {
        let fmt = match self.audio_format() {
            Some(f) => f,
            None => return,
        };
        let fb = fmt.channels as usize * 2;
        if fb == 0 || data.is_empty() {
            return;
        }
        let cap = fmt.rate as usize * fb * 2; // keep about 2 s
        {
            let mut g = lock(&self.audio);
            let seq = g.next_seq;
            let pos = g.next_pos;
            g.next_seq += 1;
            g.next_pos += (data.len() / fb) as u64;
            g.bytes += data.len() as u64;
            g.queued += data.len();
            g.chunks.push_back(Arc::new(AudioChunk { seq, pos, data: data.to_vec(), at: Instant::now() }));
            while g.queued > cap && g.chunks.len() > 1 {
                if let Some(old) = g.chunks.pop_front() {
                    g.queued -= old.data.len();
                }
            }
        }
        self.audio_cv.notify_all();
    }

    pub fn audio_bytes(&self) -> u64 {
        lock(&self.audio).bytes
    }

    // ---------------- audio: subscribe ----------------

    /// Sequence number of the next chunk to be published: subscribing here means "live".
    pub fn audio_live_edge(&self) -> u64 {
        lock(&self.audio).next_seq
    }

    /// Wait for the chunk with sequence `*next_seq`. A subscriber that fell more than
    /// ~2 s behind is moved forward to the oldest chunk still available.
    pub fn next_chunk(&self, next_seq: &mut u64, timeout: Duration) -> Option<Arc<AudioChunk>> {
        let deadline = Instant::now() + timeout;
        let mut g = lock(&self.audio);
        loop {
            if self.is_stopped() {
                return None;
            }
            let front_seq = g.chunks.front().map(|c| c.seq);
            if let Some(fs) = front_seq {
                if *next_seq < fs {
                    *next_seq = fs;
                }
                if *next_seq < g.next_seq {
                    let idx = (*next_seq - fs) as usize;
                    let found = g.chunks.get(idx).cloned();
                    if let Some(c) = found {
                        *next_seq += 1;
                        return Some(c);
                    }
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (ng, _) = self.audio_cv.wait_timeout(g, deadline - now).unwrap_or_else(|e| e.into_inner());
            g = ng;
        }
    }

    // ---------------- stats / viewers ----------------

    pub fn video_stats(&self) -> VideoStats {
        let g = lock(&self.video);
        VideoStats {
            total: g.total,
            bad: g.bad,
            same: g.same,
            last_len: g.latest.as_ref().map(|f| f.data.len()).unwrap_or(0),
            age: g.last_at.map(|t| t.elapsed().as_secs_f64()),
            viewers: g.viewers,
            w: g.latest.as_ref().map(|f| f.width).unwrap_or(0),
            h: g.latest.as_ref().map(|f| f.height).unwrap_or(0),
            fps: g.fps,
        }
    }

    pub fn last_frame_at(&self) -> Option<Instant> {
        lock(&self.video).last_at
    }

    pub fn set_fps(&self, fps: f64) {
        lock(&self.video).fps = fps;
    }

    /// Returns (viewer id, number of viewers now).
    pub fn viewer_join(&self) -> (i32, i32) {
        let mut g = lock(&self.video);
        g.viewer_seq += 1;
        g.viewers += 1;
        (g.viewer_seq, g.viewers)
    }

    pub fn viewer_leave(&self) {
        let mut g = lock(&self.video);
        g.viewers -= 1;
    }
}

#[cfg(test)]
#[path = "../tests/unit/hub_tests.rs"]
mod tests;
