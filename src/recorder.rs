//! Recording: write what the hub publishes into a file, without re-encoding.
//!
//! One more subscriber next to `web` / `rtsp`, except that the bytes go into an
//! AVI file instead of a socket:
//!
//! ```text
//!   hub --(JPEG pictures + S16LE PCM)--> two feeder threads --> one writer --> rec-*.avi
//! ```
//!
//! The pictures are already complete JPEGs and the audio is already PCM, so an AVI
//! is a container problem only: no encoder, no external crate, and the result plays
//! in VLC, ffmpeg and every phone gallery. `ffmpeg -i rec.avi -c copy out.mp4`
//! turns a recording into MP4 later.
//!
//! Why two feeder threads: the hub has two independent streams with their own
//! condvars. They hand work to the single writer over a bounded channel, so the file
//! (and the `idx1` index it needs) stays in one writer's hands. A slow disk loses
//! video frames instead of piling them up; audio is kept, but is never allowed to
//! block for more than a second.
//!
//! Timing: video timestamps come from `VideoFrame::at`, audio timestamps from the
//! running sample counter in `AudioChunk::pos` (sample exact, so no drift over
//! hours). RIFF sizes are 32 bit wide, so long recordings become several files.
//!
//! The writer finalises its file on the way out *however* the session ends - a
//! `stop()` call, the hub being stopped, or the feeders being dropped - so a file
//! is always playable, and `Engine::stop` closes an open recording before it
//! releases the camera.

use crate::hub::{self, AudioChunk, AudioFormat, Hub, VideoFrame};
use std::fs::OpenOptions;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Where recordings go when nothing else is configured.
pub const DEFAULT_DIR: &str = "record";

/// Split a long recording into several files. RIFF sizes are 32 bit, so 4 GiB is a
/// hard wall; the limit stays well below it.
const MAX_SECONDS: u64 = 300;
const MAX_BYTES: u64 = 1_500_000_000;

/// Items waiting for the writer. Deep enough for a hiccup, shallow enough that a
/// stalled disk is felt immediately.
const QUEUE: usize = 256;

/// How long a feeder waits to hand an item over before giving up on it.
const VIDEO_HANDOFF: Duration = Duration::from_millis(20);
const AUDIO_HANDOFF: Duration = Duration::from_millis(1000);
/// Pause between attempts while the queue is full.
const WAIT_SLICE: Duration = Duration::from_millis(2);
/// How long a feeder waits for new data before looking at the stop flag again.
const POLL: Duration = Duration::from_millis(200);
/// Audio chunks buffered while waiting for the first picture (its size goes into
/// the file header). 200 chunks is a couple of seconds at most.
const EARLY_AUDIO: usize = 200;

// `AVIF_HASINDEX` in the main AVI header, `AVIIF_KEYFRAME` in the index: every
// MJPEG picture is a key frame.
const AVIF_HASINDEX: u32 = 0x10;
const AVIIF_KEYFRAME: u32 = 0x10;
/// Frame rate written until the real one has been measured (it is patched on close).
const FALLBACK_FPS: f64 = 30.0;

// ===========================================================================
// What the caller gets back
// ===========================================================================

#[derive(Clone, Debug, Default)]
pub struct Summary {
    /// Every file that was written, in order (more than one when it was split).
    pub files: Vec<String>,
    pub frames: u64,
    pub secs: f64,
    /// Pictures dropped because the disk could not keep up.
    pub dropped: u64,
    /// Audio gaps for the same reason (rare: it costs a hole in the sound).
    pub audio_holes: u64,
}

impl Summary {
    /// Total size on disk, read from the files.
    pub fn bytes(&self) -> u64 {
        self.files
            .iter()
            .filter_map(|f| std::fs::metadata(f).ok())
            .map(|m| m.len())
            .sum()
    }

    /// One line for the log, or a short note when nothing was captured.
    pub fn describe(&self) -> String {
        if self.files.is_empty() {
            return "nothing was captured".to_string();
        }
        let mb = self.bytes() as f64 / (1024.0 * 1024.0);
        format!(
            "{} frame(s), {:.1} s, {:.1} MB in {} file(s): {}",
            self.frames,
            self.secs,
            mb,
            self.files.len(),
            self.files.join(", ")
        )
    }
}

#[derive(Clone, Debug)]
pub enum Status {
    Idle,
    Recording {
        file: String,
        frames: u64,
        bytes: u64,
        secs: u64,
    },
}

impl Status {
    pub fn recording(&self) -> bool {
        matches!(self, Status::Recording { .. })
    }
}

pub fn dir_from_env() -> Option<String> {
    match std::env::var("UVCWEB_RECORD_DIR") {
        Ok(d) if !d.trim().is_empty() => Some(d),
        _ => None,
    }
}

/// The directory used when the caller does not name one.
pub fn default_dir() -> String {
    dir_from_env().unwrap_or_else(|| DEFAULT_DIR.to_string())
}

// ===========================================================================
// Starting and stopping
// ===========================================================================

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Live numbers the writer publishes for `status()`.
#[derive(Default)]
struct Counters {
    frames: AtomicU64,
    bytes: AtomicU64,
    segments: AtomicU32,
    file: Mutex<String>,
}

struct Session {
    dir: PathBuf,
    base: String,
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
    dropped: Arc<AtomicU64>,
    holes: Arc<AtomicU64>,
    started: Instant,
    // Behind locks so that `status()` can look at a session while `stop()` joins
    // its threads.
    feeders: Mutex<Vec<JoinHandle<()>>>,
    writer: Mutex<Option<JoinHandle<Option<Summary>>>>,
}

static SESSION: Mutex<Option<Arc<Session>>> = Mutex::new(None);

/// Start recording the running session into `dir` (empty = the default directory).
/// Recording is one more subscriber, so it works while any protocol is serving.
pub fn start(dir: &str) -> io::Result<()> {
    let hub = hub::try_global().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotConnected, "no capture session is running")
    })?;
    start_with(&hub, dir)
}

pub fn start_with(hub: &Arc<Hub>, dir: &str) -> io::Result<()> {
    let mut slot = lock(&SESSION);
    if slot.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "already recording",
        ));
    }
    let dir = PathBuf::from(if dir.trim().is_empty() {
        default_dir()
    } else {
        dir.trim().to_string()
    });
    std::fs::create_dir_all(&dir)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {}", dir.display(), e)))?;
    // The first picture (which carries the frame size) is what really creates the
    // file, so check the directory is writable before promising anything.
    probe(&dir)?;
    let base = crate::log::file_stamp();
    *slot = Some(Arc::new(spawn(hub.clone(), dir, base)?));
    Ok(())
}

fn spawn(hub: Arc<Hub>, dir: PathBuf, base: String) -> io::Result<Session> {
    let (tx, rx) = sync_channel::<Item>(QUEUE);
    let stop = Arc::new(AtomicBool::new(false));
    let counters = Arc::new(Counters::default());
    let dropped = Arc::new(AtomicU64::new(0));
    let holes = Arc::new(AtomicU64::new(0));
    let first = segment_path(&dir, &base, 1);
    *lock(&counters.file) = first.display().to_string();
    say!("recording started: files will go to {}", dir.display());

    let feeders = vec![
        video_feeder(hub.clone(), tx.clone(), stop.clone(), dropped.clone()),
        audio_feeder(hub.clone(), tx.clone(), stop.clone(), holes.clone()),
    ];
    // The writer holds the last sender: dropping `tx` here means it sees the
    // channel close exactly when both feeders are done.
    drop(tx);
    let writer = {
        let hub = hub.clone();
        let counters = counters.clone();
        let (dir, base) = (dir.clone(), base.clone());
        std::thread::spawn(move || write_file(&hub, rx, &dir, &base, first, &counters))
    };
    Ok(Session {
        dir,
        base,
        stop,
        counters,
        dropped,
        holes,
        started: Instant::now(),
        feeders: Mutex::new(feeders),
        writer: Mutex::new(Some(writer)),
    })
}

/// Stop recording and close the file properly. `None` if nothing was being
/// recorded or if no data ever arrived.
pub fn stop() -> Option<Summary> {
    let session = {
        let mut slot = lock(&SESSION);
        slot.take()
    }?;
    session.stop.store(true, Ordering::SeqCst);
    // The feeders hold the channel senders, so wait for them to notice the flag
    // (one poll interval at most); the writer then sees an empty channel.
    for f in std::mem::take(&mut *lock(&session.feeders)) {
        let _ = f.join();
    }
    let writer = lock(&session.writer).take();
    let mut summary = match writer {
        Some(h) => h.join().ok().flatten().unwrap_or_default(),
        None => Summary::default(),
    };
    if summary.frames > 0 {
        say!("recording stopped: {}", summary.describe());
    } else {
        say!("recording stopped: nothing was captured");
    }
    if summary.dropped > 0 {
        say!(
            "recording: {} picture(s) dropped, the disk could not keep up",
            summary.dropped
        );
    }
    if summary.audio_holes > 0 {
        say!("recording: {} audio gap(s)", summary.audio_holes);
    }
    summary.dropped = session.dropped.load(Ordering::Relaxed);
    summary.audio_holes = session.holes.load(Ordering::Relaxed);
    Some(summary)
}

/// Close a recording that is still open (used when the session ends).
pub fn stop_quietly() {
    if lock(&SESSION).is_some() {
        let _ = stop();
    }
}

pub fn is_recording() -> bool {
    lock(&SESSION).is_some()
}

pub fn status() -> Status {
    let session = match lock(&SESSION).clone() {
        Some(s) => s,
        None => return Status::Idle,
    };
    let secs = session.started.elapsed().as_secs();
    let file = lock(&session.counters.file).clone();
    Status::Recording {
        file,
        frames: session.counters.frames.load(Ordering::Relaxed),
        bytes: session.counters.bytes.load(Ordering::Relaxed),
        secs,
    }
}

/// The files written so far, without stopping the recording.
pub fn files() -> Vec<String> {
    let session = match lock(&SESSION).clone() {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for n in 1..=session.counters.segments.load(Ordering::Relaxed).max(1) {
        let p = segment_path(&session.dir, &session.base, n);
        if p.exists() {
            out.push(p.display().to_string());
        }
    }
    out
}

fn probe(dir: &Path) -> io::Result<()> {
    let p = dir.join(".uvcweb-write-test");
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&p)
        .and_then(|mut f| f.write_all(b"x"))
        .and_then(|_| std::fs::remove_file(&p))
}

/// `rec-<stamp>.avi`, then `rec-<stamp>-2.avi` and so on; a counter is added when
/// the name is taken (two recordings inside one second).
fn segment_path(dir: &Path, base: &str, seg: u32) -> PathBuf {
    let stem = if seg <= 1 {
        format!("rec-{}", base)
    } else {
        format!("rec-{}-{}", base, seg)
    };
    let p = dir.join(format!("{}.avi", stem));
    if !p.exists() {
        return p;
    }
    for n in 2..1000u32 {
        let q = dir.join(format!("{}-{}.avi", stem, n));
        if !q.exists() {
            return q;
        }
    }
    p
}

// ===========================================================================
// Feeders: hub -> channel
// ===========================================================================

/// One pictures/audio chunk on its way from a feeder to the file.
#[derive(Clone)]
enum Item {
    Video { frame: Arc<VideoFrame>, pts_ms: u64 },
    Audio { chunk: Arc<AudioChunk>, pts_ms: u64 },
}

enum Hand {
    Sent,
    Dropped,
    Closed,
}

/// Hand an item to the writer, waiting up to `limit` for room in the queue.
fn hand_off(tx: &SyncSender<Item>, mut item: Item, limit: Duration) -> Hand {
    let deadline = Instant::now() + limit;
    loop {
        match tx.try_send(item) {
            Ok(()) => return Hand::Sent,
            Err(TrySendError::Disconnected(back)) => {
                std::mem::drop(back);
                return Hand::Closed;
            }
            Err(TrySendError::Full(back)) => {
                if Instant::now() >= deadline {
                    std::mem::drop(back);
                    return Hand::Dropped;
                }
                std::thread::sleep(WAIT_SLICE);
                item = back;
            }
        }
    }
}

fn video_feeder(
    hub: Arc<Hub>,
    tx: SyncSender<Item>,
    stop: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let start = Instant::now();
        let mut last = 0u64;
        while !stop.load(Ordering::Relaxed) && !hub.is_stopped() {
            let frame = match hub.next_frame(&mut last, POLL) {
                Some(f) => f,
                None => continue,
            };
            let pts_ms = frame.at.saturating_duration_since(start).as_millis() as u64;
            match hand_off(&tx, Item::Video { frame, pts_ms }, VIDEO_HANDOFF) {
                Hand::Sent => {}
                // A slow disk costs us a picture, never the whole stream.
                Hand::Dropped => {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
                Hand::Closed => break,
            }
        }
    })
}

fn audio_feeder(
    hub: Arc<Hub>,
    tx: SyncSender<Item>,
    stop: Arc<AtomicBool>,
    holes: Arc<AtomicU64>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut next = hub.audio_live_edge();
        let mut pos0: Option<u64> = None;
        let mut fmt = hub.audio_format();
        while !stop.load(Ordering::Relaxed) && !hub.is_stopped() {
            if fmt.is_none() {
                // Audio can only show up once the USB audio interface is up.
                fmt = hub.audio_format();
                if fmt.is_none() {
                    std::thread::sleep(POLL);
                    continue;
                }
            }
            let rate = fmt.map(|f| f.rate).unwrap_or(0).max(1) as u64;
            let chunk = match hub.next_chunk(&mut next, POLL) {
                Some(c) => c,
                None => continue,
            };
            // `pos` counts sample frames since the session started, so the first
            // chunk we see becomes time zero and the rest land exactly right.
            let p0 = *pos0.get_or_insert(chunk.pos);
            let pts_ms = chunk.pos.saturating_sub(p0) * 1000 / rate;
            match hand_off(&tx, Item::Audio { chunk, pts_ms }, AUDIO_HANDOFF) {
                Hand::Sent => {}
                // Never drop audio in the normal case, but never hang forever
                // either: a second without a hand over means a gap.
                Hand::Dropped => {
                    holes.fetch_add(1, Ordering::Relaxed);
                }
                Hand::Closed => break,
            }
        }
    })
}

// ===========================================================================
// The writer: channel -> AVI
// ===========================================================================

/// Everything the writer needs for one recording, so that rolling over to the next
/// file is a method instead of a dozen locals.
struct Writer<'a> {
    hub: &'a Arc<Hub>,
    dir: &'a Path,
    base: &'a str,
    counters: &'a Counters,
    avi: Option<Avi>,
    early: Vec<Item>, // audio that arrived before the first picture
    dims: (u32, u32), // the picture size, known from the first frame
    audio: Option<AudioFormat>,
    seg: u32,               // which file we are writing (0 = none yet)
    next_path: PathBuf,     // where the next one will go
    seg_start: Option<u64>, // timestamp of the first item in the current file
    summary: Summary,
}

fn write_file(
    hub: &Arc<Hub>,
    rx: Receiver<Item>,
    dir: &Path,
    base: &str,
    first: PathBuf,
    counters: &Counters,
) -> Option<Summary> {
    Writer {
        hub,
        dir,
        base,
        counters,
        avi: None,
        early: Vec::new(),
        dims: (0, 0),
        audio: hub.audio_format(),
        seg: 0,
        next_path: first,
        seg_start: None,
        summary: Summary::default(),
    }
    .run(rx)
}

impl<'a> Writer<'a> {
    /// Runs until both feeders are gone: on `stop()`, on the hub being stopped, and
    /// on a dropped channel.
    fn run(mut self, rx: Receiver<Item>) -> Option<Summary> {
        while let Ok(item) = rx.recv() {
            if self.avi.is_none() && !self.open_if_possible(&item) {
                return None;
            }
            if !self.add(item) {
                break;
            }
        }
        self.finish();
        if self.summary.frames == 0 {
            return None; // nothing to play
        }
        Some(self.summary)
    }

    /// The file header needs the picture size, so the first picture is what opens the
    /// file. Audio that turns up before it waits in `early`.
    fn open_if_possible(&mut self, item: &Item) -> bool {
        let frame = match item {
            Item::Video { frame, .. } => frame.clone(),
            _ => {
                if self.early.len() >= EARLY_AUDIO {
                    self.early.clear();
                    say!("recording: no picture yet, dropping early audio");
                } else {
                    self.early.push(item.clone());
                }
                return true;
            }
        };
        self.dims = (frame.width, frame.height);
        self.audio = self.hub.audio_format();
        if self.open().is_err() {
            return false;
        }
        let early = std::mem::take(&mut self.early);
        for item in early {
            self.put(item);
        }
        true
    }

    /// Write one item, splitting into the next file first if this one is full.
    fn add(&mut self, item: Item) -> bool {
        let pts = pts_ms(&item);
        let start = self.seg_start.unwrap_or(pts);
        if self.seg_start.is_some() && pts.saturating_sub(start) >= MAX_SECONDS * 1000 {
            // Roll over before the item, so no file is left empty and the split lands
            // on a real frame boundary.
            if self.roll_over().is_err() {
                return false;
            }
        }
        self.put(item);
        if let Some(a) = &self.avi {
            if a.data_bytes() >= MAX_BYTES && self.roll_over().is_err() {
                return false;
            }
        }
        true
    }

    fn put(&mut self, item: Item) {
        let a = match &mut self.avi {
            Some(a) => a,
            None => return,
        };
        if self.seg_start.is_none() {
            self.seg_start = Some(pts_ms(&item));
        }
        match item {
            Item::Video { frame, pts_ms } => {
                a.push(b"00dc", &frame.data, pts_ms);
                a.frames += 1;
                a.vbuf = a.vbuf.max(frame.data.len() as u32);
                self.summary.frames += 1;
                self.counters
                    .frames
                    .store(self.summary.frames, Ordering::Relaxed);
            }
            Item::Audio { chunk, pts_ms } => {
                if a.audio.is_some() {
                    a.push(b"01wb", &chunk.data, pts_ms);
                    a.samples += (chunk.data.len() as u64) / a.block_align() as u64;
                    a.abuf = a.abuf.max(chunk.data.len() as u32);
                } // else: this file has no audio stream (started with -a off)
            }
        }
        let bytes = a.len();
        self.counters.bytes.store(bytes, Ordering::Relaxed);
    }

    fn open(&mut self) -> io::Result<()> {
        let (w, h) = self.dims;
        let fps = self.hub.video_stats().fps;
        let avi = Avi::create(&self.next_path, w, h, self.audio, fps)?;
        say!(
            "recording to {} ({}x{}{})",
            self.next_path.display(),
            w,
            h,
            match self.audio {
                Some(f) => format!(", audio {} Hz x{}", f.rate, f.channels),
                None => ", no audio".to_string(),
            }
        );
        self.avi = Some(avi);
        self.seg = if self.seg == 0 { 1 } else { self.seg };
        self.seg_start = None;
        self.counters.segments.store(self.seg, Ordering::Relaxed);
        *lock(&self.counters.file) = self.next_path.display().to_string();
        Ok(())
    }

    /// Close the current file and continue in the next one.
    fn roll_over(&mut self) -> io::Result<()> {
        if let Some(path) = self.close_file() {
            say!("recording: {} is full, continuing in a new file", path);
        }
        self.seg += 1;
        self.next_path = segment_path(self.dir, self.base, self.seg);
        self.open()
    }

    /// Finalise the open file, if it holds anything.
    fn close_file(&mut self) -> Option<String> {
        let avi = self.avi.take()?;
        if avi.is_empty() {
            // Nothing went in (the split happened right after the last item): an
            // AVI with no index would not play, so do not leave it behind.
            let _ = std::fs::remove_file(&avi.path);
            return None;
        }
        match avi.finish() {
            Ok(path) => {
                self.summary.files.push(path.clone());
                Some(path)
            }
            Err(e) => {
                say!("recording: could not finish the file: {}", e);
                None
            }
        }
    }

    fn finish(&mut self) {
        self.close_file();
        if self.summary.frames == 0 {
            // Nothing to play: do not leave an empty file behind.
            for f in &self.summary.files {
                let _ = std::fs::remove_file(f);
            }
            self.summary.files.clear();
        }
    }
}

fn pts_ms(item: &Item) -> u64 {
    match item {
        Item::Video { pts_ms, .. } => *pts_ms,
        Item::Audio { pts_ms, .. } => *pts_ms,
    }
}

// ===========================================================================
// The AVI file
//
//   RIFF 'AVI '  <size, patched on close>
//     LIST 'hdrl'  avih(56)  strl[vids: strh(56) strf(40, MJPG)]  strl[auds: strh(56) strf(18, PCM)]
//     LIST 'movi'  '00dc' <len> JPEG | '01wb' <len> PCM      (word aligned; odd sizes get a pad byte)
//     'idx1' <len> {ckid, flags, offset, length} * n         (offsets count from the 'movi' four-cc)
//
// The frame rate, the lengths and the sizes are only known when recording ends, so
// the header is written once with placeholders and rewritten in place at the end.
// ===========================================================================

/// The file positions of the fields that are patched when the file is closed.
/// Offsets are absolute, which is also their position inside `Header::bytes`.
#[derive(Default)]
struct Header {
    bytes: Vec<u8>,
    us_per_frame: usize,
    max_bps: usize,
    total_frames: usize,
    v_rate: usize,
    v_length: usize,
    v_suggest: usize,
    a_length: usize,
    a_suggest: usize,
}

struct Avi {
    file: std::fs::File,
    path: PathBuf,
    hdr: Header,
    movi_base: u64, // file offset of the 'movi' four-cc
    movi_size: u64, // file offset of the 'movi' LIST size field
    pos: u64,       // next byte to be written
    idx: Vec<u8>,
    frames: u32,
    samples: u64,
    vbuf: u32,
    abuf: u32,
    dur_ms: u64,
    audio: Option<AudioFormat>,
}

impl Avi {
    fn create(
        path: &Path,
        w: u32,
        h: u32,
        audio: Option<AudioFormat>,
        fps: f64,
    ) -> io::Result<Avi> {
        let hdr = build_header(w, h, audio, fps);
        let hdr_bytes = hdr.bytes.clone();
        // The header already starts with the 12 byte RIFF head, so the 'movi' LIST
        // follows at hdr_bytes.len() and its size field four bytes further on.
        let movi_size = (hdr_bytes.len() + 4) as u64;
        let mut a = Avi {
            file: OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)?,
            path: path.to_path_buf(),
            movi_base: 0,
            movi_size,
            pos: 0,
            idx: Vec::with_capacity(16 * 120),
            frames: 0,
            samples: 0,
            vbuf: 0,
            abuf: 0,
            dur_ms: 0,
            audio,
            hdr,
        };
        // The header is the whole file head, the 12 byte RIFF/'AVI ' part included;
        // its RIFF size stays zero until finish() knows the length.
        a.raw(&hdr_bytes)?;
        a.raw(b"LIST\0\0\0\0movi")?;
        a.movi_base = a.pos;
        Ok(a)
    }

    fn raw(&mut self, b: &[u8]) -> io::Result<()> {
        self.file.write_all(b)?;
        self.pos += b.len() as u64;
        Ok(())
    }

    /// Append one chunk to `movi` and remember it for the index.
    fn push(&mut self, id: &[u8; 4], data: &[u8], pts_ms: u64) {
        // Index offsets count from the 'movi' four-cc, so the first chunk is at 4
        // (`movi_base` is the byte after it). This is what ffmpeg writes into an
        // index and what its demuxer expects.
        let offset = (self.pos - self.movi_base + 4) as u32;
        let pad = data.len() % 2;
        let ok = self
            .raw(id)
            .and_then(|_| self.raw(&(data.len() as u32).to_le_bytes()))
            .and_then(|_| self.raw(data))
            .and_then(|_| if pad == 1 { self.raw(&[0]) } else { Ok(()) });
        if ok.is_err() {
            say!("recording: write failed: {}", ok.unwrap_err());
            return;
        }
        self.idx.extend_from_slice(id);
        // Every MJPEG picture is a key frame.
        self.idx.extend_from_slice(&AVIIF_KEYFRAME.to_le_bytes());
        self.idx.extend_from_slice(&offset.to_le_bytes());
        self.idx
            .extend_from_slice(&(data.len() as u32).to_le_bytes());
        self.dur_ms = self.dur_ms.max(pts_ms);
    }

    /// True until something has actually been written into `movi`.
    fn is_empty(&self) -> bool {
        self.frames == 0 && self.samples == 0
    }

    /// Bytes inside `movi` (the whole file is a little more).
    fn data_bytes(&self) -> u64 {
        self.pos - self.movi_base
    }

    fn len(&self) -> u64 {
        self.pos
    }

    fn block_align(&self) -> u16 {
        self.audio.map(|f| f.channels * 2).unwrap_or(1)
    }

    /// Write the index, patch the sizes and the measured frame rate, and return the
    /// name of the finished file.
    fn finish(mut self) -> io::Result<String> {
        let movi_end = self.pos;
        let index = std::mem::take(&mut self.idx);
        let index_len = index.len();
        self.raw(b"idx1")?;
        self.raw(&(index_len as u32).to_le_bytes())?;
        self.raw(&index)?;
        if index_len % 2 == 1 {
            self.raw(&[0])?;
        }
        let file_len = self.pos;

        // Measured, not assumed: this is the frame rate AVI needs for a duration.
        let secs = (self.dur_ms as f64) / 1000.0;
        let fps = if secs > 0.0 && self.frames > 0 {
            (self.frames as f64) / secs
        } else {
            FALLBACK_FPS
        };
        let h = &mut self.hdr.bytes;
        patch(h, 4, (file_len as u32).saturating_sub(8)); // RIFF size
        patch(
            h,
            self.hdr.us_per_frame,
            (1e6 / fps).round().max(1.0) as u32,
        );
        patch(
            h,
            self.hdr.max_bps,
            if secs > 0.0 {
                (file_len as f64 / secs) as u32
            } else {
                0
            },
        );
        patch(h, self.hdr.total_frames, self.frames);
        patch(h, self.hdr.v_rate, fps.round().max(1.0) as u32);
        patch(h, self.hdr.v_length, self.frames);
        patch(h, self.hdr.v_suggest, self.vbuf);
        patch(h, self.hdr.a_length, self.samples as u32);
        patch(h, self.hdr.a_suggest, self.abuf);
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(h)?;

        // The 'movi' LIST size lives after the header, so it is patched in place.
        let movi_len = (movi_end - self.movi_base + 4) as u32; // + the 'movi' four-cc
        self.file.seek(SeekFrom::Start(self.movi_size))?;
        self.file.write_all(&movi_len.to_le_bytes())?;
        self.file.flush()?;
        Ok(self.path.display().to_string())
    }
}

/// Overwrite one 32 bit field of a header we already wrote.
fn patch(buf: &mut [u8], at: usize, value: u32) {
    if at + 4 <= buf.len() {
        buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
}

// ---------------- header pieces (byte exact, unit tested) ----------------

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(8 + body.len() + 1);
    b.extend_from_slice(id);
    b.extend_from_slice(&(body.len() as u32).to_le_bytes());
    b.extend_from_slice(body);
    if body.len() % 2 == 1 {
        b.push(0);
    }
    b
}

fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(12 + body.len() + 1);
    b.extend_from_slice(b"LIST");
    b.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
    b.extend_from_slice(kind);
    b.extend_from_slice(body);
    if (body.len() + 4) % 2 == 1 {
        b.push(0);
    }
    b
}

/// AVIMAINHEADER, 56 bytes.
fn avih(us_per_frame: u32, streams: u32, w: u32, h: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(56);
    b.extend_from_slice(&us_per_frame.to_le_bytes()); // dwMicroSecPerFrame
    b.extend_from_slice(&0u32.to_le_bytes()); // dwMaxBytesPerSec         (patched)
    b.extend_from_slice(&0u32.to_le_bytes()); // dwPaddingGranularity
    b.extend_from_slice(&AVIF_HASINDEX.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes()); // dwTotalFrames            (patched)
    b.extend_from_slice(&0u32.to_le_bytes()); // dwInitialFrames
    b.extend_from_slice(&streams.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes()); // dwSuggestedBufferSize    (patched)
    b.extend_from_slice(&w.to_le_bytes());
    b.extend_from_slice(&h.to_le_bytes());
    for _ in 0..4 {
        b.extend_from_slice(&0u32.to_le_bytes()); // dwReserved[4]
    }
    b
}

/// AVISTREAMHEADER, 56 bytes. dwLength and dwSuggestedBufferSize are patched later.
fn strh(kind: &[u8; 4], handler: &[u8; 4], scale: u32, rate: u32, rc: [i16; 4]) -> Vec<u8> {
    let mut b = Vec::with_capacity(56);
    b.extend_from_slice(kind); // fccType: 'vids' / 'auds'
    b.extend_from_slice(handler); // fccHandler: 'MJPG' / none
    b.extend_from_slice(&0u32.to_le_bytes()); // dwFlags
    b.extend_from_slice(&0u16.to_le_bytes()); // wPriority
    b.extend_from_slice(&0u16.to_le_bytes()); // wLanguage
    b.extend_from_slice(&0u32.to_le_bytes()); // dwInitialFrames
    b.extend_from_slice(&scale.to_le_bytes()); // dwScale
    b.extend_from_slice(&rate.to_le_bytes()); // dwRate: rate / scale = fps
    b.extend_from_slice(&0u32.to_le_bytes()); // dwStart
    b.extend_from_slice(&0u32.to_le_bytes()); // dwLength               (patched)
    b.extend_from_slice(&0u32.to_le_bytes()); // dwSuggestedBufferSize   (patched)
    b.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // dwQuality: default
    b.extend_from_slice(&0u32.to_le_bytes()); // dwSampleSize
    for v in rc {
        b.extend_from_slice(&v.to_le_bytes()); // rcFrame
    }
    b
}

/// BITMAPINFOHEADER, 40 bytes.
fn strf_video(w: u32, h: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(40);
    b.extend_from_slice(&40u32.to_le_bytes()); // biSize
    b.extend_from_slice(&(w as i32).to_le_bytes()); // biWidth
    b.extend_from_slice(&(h as i32).to_le_bytes()); // biHeight
    b.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    b.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    b.extend_from_slice(b"MJPG"); // biCompression
    b.extend_from_slice(&(w * h * 3).to_le_bytes()); // biSizeImage
    b.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    b.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    b.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    b.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
    b
}

/// WAVEFORMATEX, 18 bytes (PCM, no extra fields).
fn strf_audio(f: AudioFormat) -> Vec<u8> {
    let block_align = f.channels * 2;
    let mut b = Vec::with_capacity(18);
    b.extend_from_slice(&1u16.to_le_bytes()); // wFormatTag: PCM
    b.extend_from_slice(&f.channels.to_le_bytes());
    b.extend_from_slice(&f.rate.to_le_bytes());
    b.extend_from_slice(&(f.rate * block_align as u32).to_le_bytes()); // nAvgBytesPerSec
    b.extend_from_slice(&block_align.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes()); // wBitsPerSample
    b.extend_from_slice(&0u16.to_le_bytes()); // cbSize
    b
}

/// The fields of one `strl` that are patched when the file is closed, as absolute
/// file offsets. `at` is where the `LIST` itself will start.
#[derive(Default)]
struct StreamPatch {
    rate: usize,
    length: usize,
    suggest: usize,
}

/// One `strl` list (strh + strf), plus where its patchable fields will end up.
fn stream_list(
    kind: &[u8; 4],
    handler: &[u8; 4],
    scale: u32,
    rate: u32,
    rc: [i16; 4],
    strf: &[u8],
    at: usize,
) -> (StreamPatch, Vec<u8>) {
    let mut body = Vec::new();
    body.extend_from_slice(&chunk(b"strh", &strh(kind, handler, scale, rate, rc)));
    body.extend_from_slice(&chunk(b"strf", strf));
    // LIST(8) + 'strl'(4) + 'strh'(8) puts the 56 byte strh body at +20, and
    // inside it dwRate is +24, dwLength +32, dwSuggestedBufferSize +36.
    let base = at + 20;
    let patch = StreamPatch {
        rate: base + 24,
        length: base + 32,
        suggest: base + 36,
    };
    (patch, list(b"strl", &body))
}

/// Build the whole file header (the RIFF head plus the `hdrl` list) and remember
/// where its patchable fields will be.
fn build_header(w: u32, h: u32, audio: Option<AudioFormat>, fps: f64) -> Header {
    let fps = if fps > 0.0 { fps } else { FALLBACK_FPS };
    let us_per_frame = (1e6 / fps).round().max(1.0) as u32;
    let rate = fps.round().max(1.0) as u32;
    let streams = if audio.is_some() { 2u32 } else { 1u32 };

    // The hdrl list: its first 12 bytes ('LIST', size, 'hdrl') come before the
    // chunks, and the whole thing follows the 12 byte RIFF head.
    const BASE: usize = 12 + 12;
    let mut hdrl: Vec<u8> = Vec::new();
    hdrl.extend_from_slice(&chunk(b"avih", &avih(us_per_frame, streams, w, h)));
    // 'avih'(4) + size(4) puts the 56 byte body 8 further in.
    let avih_body = 8;
    let mut hdr = Header {
        us_per_frame: BASE + avih_body,
        max_bps: BASE + avih_body + 4,
        total_frames: BASE + avih_body + 16,
        ..Default::default()
    };

    let (v, bytes) = stream_list(
        b"vids",
        b"MJPG",
        1,
        rate,
        [0, 0, w as i16, h as i16],
        &strf_video(w, h),
        BASE + hdrl.len(),
    );
    hdrl.extend_from_slice(&bytes);
    hdr.v_rate = v.rate;
    hdr.v_length = v.length;
    hdr.v_suggest = v.suggest;

    if let Some(f) = audio {
        let block_align = f.channels * 2;
        let byte_rate = f.rate * block_align as u32;
        let (a, bytes) = stream_list(
            b"auds",
            &[0, 0, 0, 0],
            block_align as u32,
            byte_rate,
            [0, 0, 0, 0],
            &strf_audio(f),
            BASE + hdrl.len(),
        );
        hdrl.extend_from_slice(&bytes);
        hdr.a_length = a.length;
        hdr.a_suggest = a.suggest;
    }

    let mut bytes_out = Vec::with_capacity(12 + 12 + hdrl.len());
    bytes_out.extend_from_slice(b"RIFF");
    bytes_out.extend_from_slice(&0u32.to_le_bytes()); // patched: file length - 8
    bytes_out.extend_from_slice(b"AVI ");
    bytes_out.extend_from_slice(&list(b"hdrl", &hdrl));
    hdr.bytes = bytes_out;
    hdr
}

/// There is one recording at a time, so the tests that use the global session take
/// turns on this lock. A panic must not lock out the rest, hence the poison rescue.
#[cfg(test)]
pub(crate) fn test_slot() -> std::sync::MutexGuard<'static, ()> {
    static SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());
    match SLOT.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
#[path = "../tests/unit/recorder_tests.rs"]
mod tests;
