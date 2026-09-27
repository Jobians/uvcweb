//! A small, bounded copy of the live stream for a real-time encoder to read.
//!
//! Writing a file (see [`crate::recorder`]) is about durability: everything that
//! arrives ends up on disk and losing a picture is a bug. A video encoder is
//! about deadlines: a picture that arrives too late is worthless, so the queue
//! is short, the byte budget is fixed, and when the reader falls behind the
//! freshest data wins. That is why this is a separate subscriber rather than a
//! tap on the recorder's writer.
//!
//! The Android app pulls from here: it decodes each JPEG, hands the picture to
//! `MediaCodec` and muxes the result into an MP4. Nothing here knows about
//! H.264, Android or Java; it only hands out pictures and sound with the
//! timestamps the hub gave them, and it never blocks the stream.
//!
//! A reader that falls behind does not get a queue of everything it missed: the
//! hub itself only keeps the newest picture (that is what a live stream is), so
//! the reader gets the freshest one instead. For an encoder that is the wanted
//! behaviour - a picture from half a second ago is worth nothing, and H.264
//! would not have time to use it. Sound is different: it comes out of a queue
//! the hub keeps for two seconds, and every chunk is delivered in order, so
//! gaps in the sound are gaps the reader caused, not ones we hid.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::hub::{self, AudioChunk, AudioFormat, Hub, VideoFrame};

/// Which queue [`peek_pts`] and [`pull`] mean.
pub const VIDEO: i32 = 0;
pub const AUDIO: i32 = 1;

/// [`pull`] found nothing waiting.
pub const EMPTY: i64 = -1;
/// [`pull`] was given a buffer smaller than the item, which stays queued.
pub const TOO_SMALL: i64 = -2;

/// How much video to keep for the reader. A handful of pictures is plenty: it
/// gives the reader slack for one slow convert without letting the file drift
/// away from the stream.
const VIDEO_BUDGET: usize = 6 << 20;
/// Audio chunks are tiny, so this is a second or two of them.
const AUDIO_BUDGET: usize = 1 << 20;
/// Never keep fewer than this many items, so one oversized picture still fits.
const MIN_ITEMS: usize = 4;
/// How long a pull waits for the stream when there is nothing else to do.
const POLL: Duration = Duration::from_millis(200);

/// One picture or one chunk of sound, with the time it belongs at.
enum Entry {
    Video { frame: Arc<VideoFrame>, pts_us: u64 },
    Audio { chunk: Arc<AudioChunk>, pts_us: u64 },
}

impl Entry {
    fn pts_us(&self) -> u64 {
        match self {
            Entry::Video { pts_us, .. } | Entry::Audio { pts_us, .. } => *pts_us,
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Entry::Video { frame, .. } => frame.data.len(),
            Entry::Audio { chunk, .. } => chunk.data.len(),
        }
    }

    fn copy_into(&self, out: &mut [u8]) {
        let data = match self {
            Entry::Video { frame, .. } => &frame.data,
            Entry::Audio { chunk, .. } => &chunk.data,
        };
        out[..data.len()].copy_from_slice(data);
    }
}

struct Queue {
    items: VecDeque<Entry>,
    bytes: usize,
}

impl Queue {
    fn new() -> Queue {
        Queue {
            items: VecDeque::new(),
            bytes: 0,
        }
    }

    fn peek_pts(&self) -> i64 {
        match self.items.front() {
            Some(e) => e.pts_us() as i64,
            None => EMPTY,
        }
    }

    /// Puts an entry in, making room by dropping the oldest: real-time data
    /// wants the newest picture, not every one of them. Returns true if
    /// something had to go.
    fn push(&mut self, entry: Entry, budget: usize) -> bool {
        let mut dropped = false;
        while self.bytes + entry.bytes() > budget && self.items.len() > MIN_ITEMS {
            if let Some(old) = self.items.pop_front() {
                self.bytes -= old.bytes();
                dropped = true;
            }
        }
        self.bytes += entry.bytes();
        self.items.push_back(entry);
        dropped
    }

    fn pop(&mut self, out: &mut [u8]) -> i64 {
        let front = match self.items.front() {
            Some(e) => e,
            None => return EMPTY,
        };
        if front.bytes() > out.len() {
            return TOO_SMALL;
        }
        let entry = self.items.pop_front().unwrap();
        self.bytes -= entry.bytes();
        entry.copy_into(out);
        entry.bytes() as i64
    }
}

struct Feed {
    hub: Arc<Hub>,
    video: Mutex<Queue>,
    audio: Mutex<Queue>,
    stop: Arc<AtomicBool>,
    /// Set when the session itself ended, so a reader knows to finish its file.
    ended: AtomicBool,
    /// One bit per copier, set once it is reading the live edge. Sound that
    /// arrived before that is not part of the recording.
    attached: AtomicU32,
    /// Pictures offered to the reader.
    frames: AtomicU64,
    /// Pictures thrown away because the reader could not keep up.
    dropped: AtomicU64,
    /// Sound offered to the reader.
    chunks: AtomicU64,
    sound_bytes: AtomicU64,
    started: Instant,
    thread: Mutex<Option<JoinHandle<()>>>,
}

static FEED: Mutex<Option<Arc<Feed>>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn with_feed<T>(f: impl FnOnce(&Arc<Feed>) -> T, or: T) -> T {
    match lock(&FEED).as_ref() {
        Some(feed) => f(feed),
        None => or,
    }
}

/// Live numbers, for the app's status line.
#[derive(Default, Clone, Copy, Debug)]
pub struct Stats {
    /// Pictures the card itself has delivered this session. The gap between
    /// this and `frames` is the whole diagnosis when a reader gets nothing: a
    /// `source` of 0 means the card has not started sending, while a `source`
    /// that grows under a `frames` of 0 means the reader missed the stream.
    pub source: u64,
    pub frames: u64,
    pub dropped: u64,
    pub chunks: u64,
    pub sound_bytes: u64,
    pub secs: u64,
    /// Pictures waiting to be read right now.
    pub queued: u64,
    /// ...and how many bytes of them that is, which is the bound that matters.
    pub queued_bytes: u64,
}

/// Start copying the running session's pictures and sound for a reader. The
/// reader has to be ready to [`pull`] soon after, because the queue is short.
pub fn arm() -> io::Result<()> {
    let hub = hub::try_global().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotConnected, "no capture session is running")
    })?;
    arm_with(&hub)
}

/// Like [`arm`], but for a given session, which is what the tests use.
pub fn arm_with(hub: &Arc<Hub>) -> io::Result<()> {
    let mut slot = lock(&FEED);
    if slot.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a reader is already attached",
        ));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let feed = Arc::new(Feed {
        hub: hub.clone(),
        video: Mutex::new(Queue::new()),
        audio: Mutex::new(Queue::new()),
        stop: stop.clone(),
        ended: AtomicBool::new(false),
        attached: AtomicU32::new(0),
        frames: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
        chunks: AtomicU64::new(0),
        sound_bytes: AtomicU64::new(0),
        started: Instant::now(),
        thread: Mutex::new(None),
    });
    let handle = {
        let (feed, hub) = (feed.clone(), hub.clone());
        std::thread::spawn(move || copy(hub, feed, stop))
    };
    *lock(&feed.thread) = Some(handle);
    *slot = Some(feed);
    // What the card has sent by now decides what happens next: a reader that
    // attaches before the card's first picture has to wait for one, and the
    // app needs to be able to tell that from a reader that missed the stream.
    say!(
        "the encoder feed attached: the card has sent {} picture(s) so far",
        hub.video_stats().total
    );
    Ok(())
}

/// Stop copying and let go of the queues. Anything still queued is dropped, so
/// a reader that wants the tail has to pull it before calling this.
pub fn disarm() {
    let feed = match lock(&FEED).take() {
        Some(f) => f,
        None => return,
    };
    feed.stop.store(true, Ordering::SeqCst);
    // The handle is taken out before the join so nothing else waits on it.
    let handle = lock(&feed.thread).take();
    drop(feed);
    if let Some(h) = handle {
        let _ = h.join();
    }
}

pub fn is_armed() -> bool {
    lock(&FEED).is_some()
}

/// True once the capture session itself stopped, so a reader knows to finish
/// its file instead of waiting for pictures that will never come.
pub fn has_ended() -> bool {
    with_feed(|f| f.ended.load(Ordering::Relaxed), false)
}

/// True once both copiers are attached to the live edge. Sound published before
/// this point is not part of the recording, so a reader waits for this before it
/// decides whether the session has sound at all.
pub fn is_attached() -> bool {
    with_feed(|f| f.attached.load(Ordering::Acquire) == 3, false)
}

/// The timestamp of the item at the head of a queue, in microseconds, or
/// [`EMPTY`]. Ask first and pull second: between the two the item must not
/// change, which holds as long as only one reader pulls.
pub fn peek_pts(kind: i32) -> i64 {
    with_feed(|f| lock(queue_of(f, kind)).peek_pts(), EMPTY)
}

/// Takes the head of a queue into `out` and returns how many bytes it wrote,
/// [`EMPTY`] if there was nothing, or [`TOO_SMALL`] if `out` was too small - in
/// which case the item stays put and the call can simply be repeated.
pub fn pull(kind: i32, out: &mut [u8]) -> i64 {
    with_feed(|f| lock(queue_of(f, kind)).pop(out), EMPTY)
}

fn queue_of(feed: &Arc<Feed>, kind: i32) -> &Mutex<Queue> {
    if kind == AUDIO {
        &feed.audio
    } else {
        &feed.video
    }
}

/// The sound format the session settled on, for an encoder to configure itself
/// with. None until the USB audio interface is up.
pub fn audio_format() -> Option<AudioFormat> {
    with_feed(|f| f.hub.audio_format(), None)
}

/// The picture rate the session is running at, as a hint for an encoder.
pub fn video_fps() -> f64 {
    with_feed(|f| f.hub.video_stats().fps, 0.0)
}

pub fn stats() -> Stats {
    with_feed(
        |f| {
            // Counted before the queue is locked, so this cannot end up holding
            // two locks at once (the hub's and the feed's).
            let source = f.hub.video_stats().total;
            // One lock, not two: the guards would overlap inside the expression
            // and a mutex cannot be taken twice.
            let (queued, queued_bytes) = {
                let q = lock(&f.video);
                (q.items.len() as u64, q.bytes as u64)
            };
            Stats {
                source,
                frames: f.frames.load(Ordering::Relaxed),
                dropped: f.dropped.load(Ordering::Relaxed),
                chunks: f.chunks.load(Ordering::Relaxed),
                sound_bytes: f.sound_bytes.load(Ordering::Relaxed),
                secs: f.started.elapsed().as_secs(),
                queued,
                queued_bytes,
            }
        },
        Stats::default(),
    )
}

/// The copier's thread: one loop for pictures, one for sound, both until the
/// session stops or the reader goes away.
fn copy(hub: Arc<Hub>, feed: Arc<Feed>, stop: Arc<AtomicBool>) {
    let pictures = {
        let (hub, feed, stop) = (hub.clone(), feed.clone(), stop.clone());
        std::thread::spawn(move || copy_pictures(hub, feed, stop))
    };
    copy_sound(hub, feed.clone(), stop);
    let _ = pictures.join();
    feed.ended.store(true, Ordering::Release);
}

fn copy_pictures(hub: Arc<Hub>, feed: Arc<Feed>, stop: Arc<AtomicBool>) {
    let start = feed.started;
    let mut last = 0u64;
    feed.attached.fetch_or(1, Ordering::Release);
    while !stop.load(Ordering::Relaxed) && !hub.is_stopped() {
        let frame = match hub.next_frame(&mut last, POLL) {
            Some(f) => f,
            None => continue,
        };
        // No copy here: the queue holds the shared frame, and the reader copies
        // the bytes out of it once, into its own buffer.
        let pts_us = frame.at.saturating_duration_since(start).as_micros() as u64;
        if lock(&feed.video).push(Entry::Video { frame, pts_us }, VIDEO_BUDGET) {
            feed.dropped.fetch_add(1, Ordering::Relaxed);
        }
        feed.frames.fetch_add(1, Ordering::Relaxed);
    }
}

fn copy_sound(hub: Arc<Hub>, feed: Arc<Feed>, stop: Arc<AtomicBool>) {
    let mut next = hub.audio_live_edge();
    let mut pos0: Option<u64> = None;
    let mut fmt = hub.audio_format();
    feed.attached.fetch_or(2, Ordering::Release);
    while !stop.load(Ordering::Relaxed) && !hub.is_stopped() {
        let rate = match fmt {
            Some(f) => f.rate.max(1) as u64,
            None => {
                // Sound can only show up once the USB audio interface is up.
                fmt = hub.audio_format();
                if fmt.is_none() {
                    std::thread::sleep(POLL);
                    continue;
                }
                fmt.map(|f| f.rate).unwrap_or(1).max(1) as u64
            }
        };
        let chunk = match hub.next_chunk(&mut next, POLL) {
            Some(c) => c,
            None => continue,
        };
        // `pos` counts sample frames since the session started, so the first
        // chunk we see becomes time zero - the same origin the file recorder
        // uses, which is what keeps sound and picture together.
        let p0 = *pos0.get_or_insert(chunk.pos);
        let pts_us = chunk.pos.saturating_sub(p0).saturating_mul(1_000_000) / rate;
        let bytes = chunk.data.len() as u64;
        if lock(&feed.audio).push(Entry::Audio { chunk, pts_us }, AUDIO_BUDGET) {
            feed.dropped.fetch_add(1, Ordering::Relaxed);
        }
        feed.chunks.fetch_add(1, Ordering::Relaxed);
        feed.sound_bytes.fetch_add(bytes, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[path = "../tests/unit/feed_tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) fn test_slot() -> std::sync::MutexGuard<'static, ()> {
    static SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());
    match SLOT.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}
