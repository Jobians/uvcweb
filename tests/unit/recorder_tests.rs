//! Unit tests for `src/recorder.rs`, kept in this separate file so the module itself
//! stays clean. Included there as `#[path = "../tests/unit/recorder_tests.rs"] mod tests;`
//! -- it is still logically part of that module (private items stay reachable through
//! `use super::*;`), just not stored inline.

use super::*;

fn u16at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn stereo(rate: u32) -> AudioFormat {
    AudioFormat { rate, channels: 2 }
}

/// A unique, empty directory for one test (no external crates, so no tempfile).
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "uvcweb-test-{}-{}-{}",
        name,
        std::process::id(),
        crate::log::file_stamp()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// There is one global recorder, so the tests that use it take turns.
fn one_recorder() -> std::sync::MutexGuard<'static, ()> {
    super::test_slot()
}

/// Waits (up to about two seconds) for something the recorder's own threads do.
/// A fixed sleep would be a race: they poll twice a second, and a loaded machine can
/// take much longer than that.
fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..100 {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {}", what);
}

/// Frames the running recording has taken so far.
fn recorded_frames() -> u64 {
    match status() {
        Status::Recording { frames, .. } => frames,
        Status::Idle => 0,
    }
}

fn jpegish(n: usize, fill: u8) -> Vec<u8> {
    let mut v = vec![fill; n];
    v[0] = 0xFF;
    v[1] = 0xD8;
    v[n - 2] = 0xFF;
    v[n - 1] = 0xD9; // the end of a JPEG, for anyone that looks
    v
}

// ===========================================================================
// The header pieces
// ===========================================================================

#[test]
fn avih_has_the_layout_players_read() {
    let h = avih(33_333, 2, 640, 480);
    assert_eq!(h.len(), 56);
    assert_eq!(u32at(&h, 0), 33_333); // dwMicroSecPerFrame
    assert_eq!(u32at(&h, 4), 0); // dwMaxBytesPerSec: patched when the file closes
    assert_eq!(u32at(&h, 12), AVIF_HASINDEX); // dwFlags
    assert_eq!(u32at(&h, 16), 0); // dwTotalFrames: patched
    assert_eq!(u32at(&h, 24), 2); // dwStreams
    assert_eq!(u32at(&h, 32), 640); // dwWidth
    assert_eq!(u32at(&h, 36), 480); // dwHeight
    assert!(h[40..56].iter().all(|b| *b == 0)); // dwReserved[4]
}

#[test]
fn video_strh_counts_frames_at_the_frame_rate() {
    let h = strh(b"vids", b"MJPG", 1, 25, [0, 0, 640, 480]);
    assert_eq!(h.len(), 56);
    assert_eq!(&h[0..4], b"vids");
    assert_eq!(&h[4..8], b"MJPG");
    assert_eq!(u32at(&h, 20), 1); // dwScale
    assert_eq!(u32at(&h, 24), 25); // dwRate -> 25 fps
    assert_eq!(u32at(&h, 28), 0); // dwStart
                                  // rcFrame: left, top, right (640), bottom (480)
    assert_eq!(u16at(&h, 48), 0);
    assert_eq!(u16at(&h, 50), 0);
    assert_eq!(u16at(&h, 52), 640);
    assert_eq!(u16at(&h, 54), 480);
}

#[test]
fn audio_strh_uses_block_align_over_byte_rate() {
    let h = strh(b"auds", &[0, 0, 0, 0], 4, 192_000, [0, 0, 0, 0]);
    assert_eq!(h.len(), 56);
    assert_eq!(&h[0..4], b"auds");
    assert_eq!(u32at(&h, 20), 4); // dwScale: bytes per sample frame
    assert_eq!(u32at(&h, 24), 192_000); // dwRate -> 48000 samples per second
    assert!(h[48..56].iter().all(|b| *b == 0)); // rcFrame is empty for audio
}

#[test]
fn strf_video_is_a_bitmapinfoheader() {
    let h = strf_video(1280, 720);
    assert_eq!(h.len(), 40);
    assert_eq!(u32at(&h, 0), 40); // biSize
    assert_eq!(u32at(&h, 4), 1280); // biWidth
    assert_eq!(u32at(&h, 8), 720); // biHeight
    assert_eq!(u16at(&h, 12), 1); // biPlanes
    assert_eq!(u16at(&h, 14), 24); // biBitCount
    assert_eq!(&h[16..20], b"MJPG"); // biCompression
    assert_eq!(u32at(&h, 20), 1280 * 720 * 3); // biSizeImage
}

#[test]
fn strf_audio_is_a_waveformatex() {
    let h = strf_audio(stereo(48_000));
    assert_eq!(h.len(), 18);
    assert_eq!(u16at(&h, 0), 1); // PCM
    assert_eq!(u16at(&h, 2), 2); // channels
    assert_eq!(u32at(&h, 4), 48_000); // sample rate
    assert_eq!(u32at(&h, 8), 48_000 * 4); // byte rate
    assert_eq!(u16at(&h, 12), 4); // block align
    assert_eq!(u16at(&h, 14), 16); // bits per sample
    assert_eq!(u16at(&h, 16), 0); // no extra fields
}

#[test]
fn chunks_and_lists_are_word_aligned() {
    assert_eq!(
        chunk(b"avih", &[1, 2, 3]),
        b"avih\x03\x00\x00\x00\x01\x02\x03\x00".to_vec()
    );
    assert_eq!(
        chunk(b"avih", &[1, 2, 3, 4]),
        b"avih\x04\x00\x00\x00\x01\x02\x03\x04".to_vec()
    );
    // LIST size counts the four-cc plus the body, and is padded to an even length.
    let l = list(b"strl", &[7, 8, 9]);
    assert_eq!(&l[0..4], b"LIST");
    assert_eq!(u32at(&l, 4), 7); // 4 + 3, not including the pad byte
    assert_eq!(&l[8..12], b"strl");
    assert_eq!(l.len(), 12 + 3 + 1);
}

// ===========================================================================
// The whole header
// ===========================================================================

#[test]
fn header_is_a_riff_avi_with_both_streams() {
    let h = build_header(640, 480, Some(stereo(48_000)), 30.0);
    assert_eq!(&h.bytes[0..4], b"RIFF");
    assert_eq!(&h.bytes[8..12], b"AVI ");
    assert_eq!(&h.bytes[12..16], b"LIST");
    assert_eq!(&h.bytes[20..24], b"hdrl");
    assert_eq!(&h.bytes[24..28], b"avih");
    // one video stream and one audio stream
    assert_eq!(u32at(&h.bytes, 24 + 8 + 24), 2);
    // the audio stream is there
    assert!(h.bytes.windows(4).any(|w| w == b"auds"));
    // and the video one
    assert!(h.bytes.windows(8).any(|w| w == b"vidsMJPG"));
}

#[test]
fn header_without_audio_has_one_stream() {
    let h = build_header(320, 240, None, 15.0);
    assert_eq!(u32at(&h.bytes, 24 + 8 + 24), 1); // dwStreams
    assert!(!h.bytes.windows(4).any(|w| w == b"auds"));
    // 12 (RIFF head) + 12 (LIST) + 64 (avih) + 124 (video strl)
    assert_eq!(h.bytes.len(), 212);
    assert_eq!(u32at(&h.bytes, h.us_per_frame), 66_667); // 1e6 / 15 fps
}

#[test]
fn header_without_audio_is_shorter_by_one_stream() {
    let with = build_header(320, 240, Some(stereo(48_000)), 15.0);
    let without = build_header(320, 240, None, 15.0);
    // LIST(12) + strh(64) + strf(26)
    assert_eq!(with.bytes.len(), without.bytes.len() + 12 + 64 + 26);
}

#[test]
fn patch_offsets_point_at_the_right_fields() {
    let mut h = build_header(640, 480, Some(stereo(48_000)), 30.0);
    // Everything inside the file, so the offsets also index `bytes`.
    patch(&mut h.bytes, h.us_per_frame, 20_000);
    patch(&mut h.bytes, h.max_bps, 1_000_000);
    patch(&mut h.bytes, h.total_frames, 123);
    patch(&mut h.bytes, h.v_rate, 40);
    patch(&mut h.bytes, h.v_length, 456);
    patch(&mut h.bytes, h.v_suggest, 789);
    patch(&mut h.bytes, h.a_length, 1000);
    patch(&mut h.bytes, h.a_suggest, 2048);
    assert_eq!(u32at(&h.bytes, 32), 20_000); // avih.dwMicroSecPerFrame
    assert_eq!(u32at(&h.bytes, 36), 1_000_000); // avih.dwMaxBytesPerSec
    assert_eq!(u32at(&h.bytes, 48), 123); // avih.dwTotalFrames
    assert_eq!(u32at(&h.bytes, 132), 40); // video strh.dwRate
    assert_eq!(u32at(&h.bytes, 140), 456); // video strh.dwLength
    assert_eq!(u32at(&h.bytes, 144), 789); // video strh.dwSuggestedBufferSize
    assert_eq!(u32at(&h.bytes, 264), 1000); // audio strh.dwLength
    assert_eq!(u32at(&h.bytes, 268), 2048); // audio strh.dwSuggestedBufferSize
}

// ===========================================================================
// A finished file
// ===========================================================================

/// Write `frames` pictures and `chunks` audio chunks and return the finished bytes.
fn build_file(path: &Path, frames: usize, chunks: usize, audio: Option<AudioFormat>) -> Vec<u8> {
    let hub = Hub::new();
    if let Some(f) = audio {
        hub.set_audio_format(f);
    }
    let (tx, rx) = sync_channel::<Item>(4096);
    let feed = std::thread::spawn(move || {
        for i in 0..frames {
            let frame = Arc::new(VideoFrame {
                seq: i as u64,
                data: jpegish(2000 + i, i as u8),
                width: 640,
                height: 480,
                at: Instant::now(),
            });
            if tx
                .send(Item::Video {
                    frame,
                    pts_ms: (i as u64) * 33,
                })
                .is_err()
            {
                return;
            }
        }
        for i in 0..chunks {
            let chunk = Arc::new(AudioChunk {
                seq: i as u64,
                pos: (i as u64) * 480,
                data: vec![i as u8; 1920], // 10 ms of 48 kHz stereo
                at: Instant::now(),
            });
            if tx
                .send(Item::Audio {
                    chunk,
                    pts_ms: (i as u64) * 10,
                })
                .is_err()
            {
                return;
            }
        }
    });
    let counters = Counters::default();
    let summary = write_file(
        &hub,
        rx,
        path.parent().unwrap(),
        "test",
        path.to_path_buf(),
        &counters,
    );
    feed.join().unwrap();
    assert!(summary.is_some(), "nothing was written");
    std::fs::read(path).expect("the finished file")
}

#[test]
fn finished_file_is_a_consistent_riff() {
    let dir = scratch("riff");
    let path = dir.join("rec.avi");
    let b = build_file(&path, 3, 2, Some(stereo(48_000)));

    assert_eq!(&b[0..4], b"RIFF");
    assert_eq!(&b[8..12], b"AVI ");
    // The RIFF size is the file minus the first eight bytes.
    assert_eq!(u32at(&b, 4) as usize, b.len() - 8);

    // The index is the last thing in the file.
    let idx_marker = b
        .windows(4)
        .rposition(|w| w == b"idx1")
        .expect("an index at the end");
    assert_eq!(u32at(&b, idx_marker + 4) as usize, b.len() - idx_marker - 8);
    let entries = (b.len() - idx_marker - 8) / 16;
    assert_eq!(entries, 5); // 3 pictures + 2 audio chunks

    // Every entry names a chunk, points at it, and has its length right.
    let movi = b
        .windows(4)
        .rposition(|w| w == b"movi")
        .expect("a movi list");
    for n in 0..entries {
        let e = idx_marker + 8 + n * 16;
        let id = &b[e..e + 4];
        let flags = u32at(&b, e + 4);
        let offset = u32at(&b, e + 8) as usize;
        let len = u32at(&b, e + 12) as usize;
        assert!(id == b"00dc" || id == b"01wb", "chunk id {:?}", id);
        assert_eq!(flags, AVIIF_KEYFRAME);
        // Offsets count from the 'movi' four-cc, so the first chunk is at 4.
        let at = movi + offset;
        assert_eq!(
            &b[at..at + 4],
            id,
            "offset of entry {} does not point at its chunk",
            n
        );
        assert_eq!(u32at(&b, at + 4) as usize, len);
    }
    assert_eq!(
        u32at(&b, idx_marker + 8 + 8),
        4,
        "the first chunk sits at 4"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_header_and_the_picture_data_are_butted_together() {
    let dir = scratch("butted");
    let path = dir.join("rec.avi");
    let b = build_file(&path, 3, 2, Some(stereo(48_000)));

    // A player walks the file by following the sizes in it, so every chunk has to
    // start exactly where the previous one ends. This walks the head that way and
    // fails on a single stray byte between two chunks.
    let mut at = 12usize; // after 'RIFF', its size, and 'AVI '
    let mut movi_list = 0usize;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let size = u32at(&b, at + 4) as usize;
        if id == b"LIST" {
            if &b[at + 8..at + 12] == b"movi" {
                movi_list = at;
                break;
            }
            // A nested list: the caller checks the contents, we only need its end.
            at += 8 + size;
        } else {
            at += 8 + size;
        }
        assert_eq!(at % 2, 0, "chunks stay word aligned");
    }
    assert_ne!(movi_list, 0, "found the movi list");

    // The pictures start on the very next byte, and the movi list accounts for all
    // of them up to the index.
    let idx_marker = b
        .windows(4)
        .rposition(|w| w == b"idx1")
        .expect("an index at the end");
    assert_eq!(
        &b[movi_list + 12..movi_list + 16],
        b"00dc",
        "the first chunk follows 'movi' directly"
    );
    assert_eq!(
        u32at(&b, movi_list + 4) as usize,
        idx_marker - (movi_list + 8),
        "the movi list size covers every chunk and nothing else"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn finished_file_has_the_pictures_untouched() {
    let dir = scratch("pictures");
    let path = dir.join("rec.avi");
    let b = build_file(&path, 4, 0, Some(stereo(48_000)));
    for i in 0..4usize {
        let want = jpegish(2000 + i, i as u8);
        assert!(
            b.windows(want.len()).any(|w| w == want.as_slice()),
            "picture {} is not in the file",
            i
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn finished_file_patches_the_counts_and_the_frame_rate() {
    let dir = scratch("counts");
    let path = dir.join("rec.avi");
    // 10 pictures at 40 ms apart = 360 ms of recording, so ~27.8 fps is measured
    // from the timestamps rather than from the hint.
    let hub = Hub::new();
    hub.set_audio_format(stereo(48_000));
    let (tx, rx) = sync_channel::<Item>(64);
    let feed = std::thread::spawn(move || {
        for i in 0..10u64 {
            let frame = Arc::new(VideoFrame {
                seq: i,
                data: jpegish(2000, i as u8),
                width: 640,
                height: 480,
                at: Instant::now(),
            });
            if tx
                .send(Item::Video {
                    frame,
                    pts_ms: i * 40,
                })
                .is_err()
            {
                return;
            }
        }
    });
    write_file(&hub, rx, &dir, "test", path.clone(), &Counters::default());
    feed.join().unwrap();
    let b = std::fs::read(&path).unwrap();
    assert_eq!(u32at(&b, 48), 10); // avih.dwTotalFrames
    assert_eq!(u32at(&b, 140), 10); // video strh.dwLength
                                    // 10 frames over 360 ms is 27.7 fps; the file has to say so.
    let fps = u32at(&b, 132);
    assert!((27..=28).contains(&fps), "frame rate in the file: {}", fps);
    let us = u32at(&b, 32);
    assert!(
        (35_000..=36_000).contains(&us),
        "microseconds per frame: {}",
        us
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn audio_counts_samples_in_the_header() {
    let dir = scratch("samples");
    let path = dir.join("rec.avi");
    let b = build_file(&path, 1, 2, Some(stereo(48_000)));
    // Two chunks of 1920 bytes each at 4 bytes per sample frame = 960 samples.
    assert_eq!(u32at(&b, 264), 960);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn odd_sized_chunks_are_padded_and_the_index_accounts_for_it() {
    let dir = scratch("padding");
    let path = dir.join("rec.avi");
    let hub = Hub::new();
    hub.set_audio_format(stereo(48_000));
    let (tx, rx) = sync_channel::<Item>(16);
    let odd = vec![7u8; 1933]; // odd length: needs a pad byte
    let expect = odd.clone();
    let feed = std::thread::spawn(move || {
        for i in 0..2u64 {
            let frame = Arc::new(VideoFrame {
                seq: i,
                data: if i == 0 {
                    odd.clone()
                } else {
                    jpegish(2000, 1)
                },
                width: 640,
                height: 480,
                at: Instant::now(),
            });
            if tx
                .send(Item::Video {
                    frame,
                    pts_ms: i * 33,
                })
                .is_err()
            {
                return;
            }
        }
    });
    write_file(&hub, rx, &dir, "test", path.clone(), &Counters::default());
    feed.join().unwrap();
    let b = std::fs::read(&path).unwrap();
    let movi = b.windows(4).rposition(|w| w == b"movi").unwrap();
    let first = movi + 4; // the first chunk, right after the 'movi' four-cc
    assert_eq!(u32at(&b, first + 4) as usize, 1933);
    assert_eq!(&b[first + 8..first + 8 + 1933], &expect[..]);
    assert_eq!(b[first + 8 + 1933], 0, "the pad byte");
    // The second chunk starts an even distance further on, and the index agrees.
    let second = first + 8 + 1933 + 1;
    assert_eq!(&b[second..second + 4], b"00dc");
    let idx = b.windows(4).rposition(|w| w == b"idx1").unwrap();
    // The second entry's offset: idx(4) + size(4) + 16 bytes per earlier entry.
    assert_eq!(u32at(&b, idx + 8 + 16 + 8) as usize, second - movi);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_long_recording_is_split_into_several_files() {
    let dir = scratch("split");
    let hub = Hub::new();
    hub.set_audio_format(stereo(48_000));
    let (tx, rx) = sync_channel::<Item>(64);
    // Two pictures one second apart, then one 400 s later: the second one is past
    // the split limit, so the writer rolls over to a new file.
    let feed = std::thread::spawn(move || {
        for i in 0..3u64 {
            let frame = Arc::new(VideoFrame {
                seq: i,
                data: jpegish(2000, i as u8),
                width: 640,
                height: 480,
                at: Instant::now(),
            });
            let pts = if i < 2 {
                i * 1000
            } else {
                MAX_SECONDS * 1000 + 500
            };
            if tx.send(Item::Video { frame, pts_ms: pts }).is_err() {
                return;
            }
        }
    });
    let summary = write_file(
        &hub,
        rx,
        &dir,
        "test",
        dir.join("rec-test-1.avi"),
        &Counters::default(),
    )
    .expect("something was written");
    feed.join().unwrap();
    assert_eq!(
        summary.files.len(),
        2,
        "expected a rollover: {:?}",
        summary.files
    );
    assert_eq!(
        summary.frames, 3,
        "all three pictures are kept, across both files"
    );
    for f in &summary.files {
        let b = std::fs::read(f).unwrap();
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(u32at(&b, 4) as usize, b.len() - 8);
    }
    assert!(dir.join("rec-test-2.avi").exists());
    // The split happens on a frame boundary: the first file holds the first two
    // pictures, the second one the last.
    let first = std::fs::read(&summary.files[0]).unwrap();
    let second = std::fs::read(&summary.files[1]).unwrap();
    assert_eq!(u32at(&first, 48), 2, "frames in the first file");
    assert_eq!(u32at(&second, 48), 1, "frames in the second file");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_recording_with_no_pictures_leaves_no_file_behind() {
    let dir = scratch("empty");
    let hub = Hub::new();
    let (tx, rx) = sync_channel::<Item>(8);
    let feed = std::thread::spawn(move || {
        // audio only, and the writer never gets a picture to size the header with
        for _ in 0..3 {
            let chunk = Arc::new(AudioChunk {
                seq: 0,
                pos: 0,
                data: vec![0; 320],
                at: Instant::now(),
            });
            if tx.send(Item::Audio { chunk, pts_ms: 0 }).is_err() {
                return;
            }
        }
    });
    let path = dir.join("rec-test-1.avi");
    let out = write_file(&hub, rx, &dir, "test", path.clone(), &Counters::default());
    feed.join().unwrap();
    assert!(out.is_none(), "nothing should have been reported");
    assert!(!path.exists(), "no empty file should be left behind");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn audio_is_dropped_when_the_file_has_no_audio_stream() {
    let dir = scratch("noaudio");
    let hub = Hub::new(); // no audio format: as with -a off
    let (tx, rx) = sync_channel::<Item>(16);
    let feed = std::thread::spawn(move || {
        let frame = Arc::new(VideoFrame {
            seq: 0,
            data: jpegish(2000, 3),
            width: 320,
            height: 240,
            at: Instant::now(),
        });
        let _ = tx.send(Item::Video { frame, pts_ms: 0 });
        let chunk = Arc::new(AudioChunk {
            seq: 0,
            pos: 0,
            data: vec![1; 640],
            at: Instant::now(),
        });
        let _ = tx.send(Item::Audio { chunk, pts_ms: 0 });
    });
    let path = dir.join("rec-test-1.avi");
    let s = write_file(&hub, rx, &dir, "test", path.clone(), &Counters::default()).unwrap();
    feed.join().unwrap();
    assert_eq!(s.frames, 1);
    let b = std::fs::read(&path).unwrap();
    assert!(!b.windows(4).any(|w| w == b"01wb"), "no audio chunks");
    let idx = b.windows(4).rposition(|w| w == b"idx1").unwrap();
    assert_eq!((b.len() - idx - 8) / 16, 1, "one index entry");
    std::fs::remove_dir_all(&dir).ok();
}

// ===========================================================================
// Names
// ===========================================================================

#[test]
fn file_names_carry_the_time_and_count_up() {
    let dir = scratch("names");
    assert_eq!(
        segment_path(&dir, "20260927-142530", 1),
        dir.join("rec-20260927-142530.avi")
    );
    assert_eq!(
        segment_path(&dir, "20260927-142530", 3),
        dir.join("rec-20260927-142530-3.avi")
    );
    // A second recording in the same second must not overwrite the first one.
    std::fs::write(dir.join("rec-20260927-142530.avi"), b"x").unwrap();
    assert_eq!(
        segment_path(&dir, "20260927-142530", 1),
        dir.join("rec-20260927-142530-2.avi")
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_default_directory_is_the_record_folder() {
    // Nothing is set in the test environment, so the default applies.
    if std::env::var("UVCWEB_RECORD_DIR").is_err() {
        assert_eq!(default_dir(), DEFAULT_DIR);
    }
}

#[test]
fn an_unwritable_directory_is_reported_instead_of_silently_losing_frames() {
    let _guard = one_recorder();
    // A file where the directory should be: creating it must fail.
    let dir = scratch("notadir");
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let hub = Hub::new();
    let e = start_with(&hub, blocker.to_str().unwrap()).unwrap_err();
    assert!(
        e.to_string().contains("blocker"),
        "the error should name the directory, got: {}",
        e
    );
    // Nothing was started, so the slot is still free.
    assert!(!is_recording());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_global_session_slot_is_single_use() {
    let _guard = one_recorder();
    let dir = scratch("session");
    let hub = Hub::new();
    hub.submit_frame(&jpegish(2000, 5), 640, 480);
    start_with(&hub, dir.to_str().unwrap()).expect("first start");
    // A second start while recording is refused, not silently accepted.
    assert!(start_with(&hub, dir.to_str().unwrap()).is_err());
    assert!(status().recording());
    // Feeding a picture or two and stopping leaves a real file behind.
    for _ in 0..5 {
        hub.submit_frame(&jpegish(2000, 6), 640, 480);
        std::thread::sleep(Duration::from_millis(30));
    }
    let summary = stop().expect("a summary");
    assert!(!is_recording());
    assert!(
        summary.frames > 0,
        "expected frames, got {}",
        summary.frames
    );
    assert_eq!(summary.files.len(), 1);
    let b = std::fs::read(&summary.files[0]).unwrap();
    assert_eq!(&b[8..12], b"AVI ");
    assert_eq!(u32at(&b, 4) as usize, b.len() - 8);
    // The slot is free again for the next recording.
    assert!(!status().recording());
    assert!(stop().is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_stopping_session_still_closes_the_file() {
    let _guard = one_recorder();
    // Engine::stop() does this, and the writers must not be left with a half file.
    let dir = scratch("hubstop");
    let hub = Hub::new();
    hub.set_audio_format(stereo(48_000));
    hub.submit_frame(&jpegish(2000, 9), 640, 480);
    start_with(&hub, dir.to_str().unwrap()).unwrap();
    for _ in 0..5 {
        hub.submit_frame(&jpegish(2000, 10), 640, 480);
        std::thread::sleep(Duration::from_millis(30));
    }
    wait_for("the writer to take a frame", || recorded_frames() > 0);
    hub.request_stop();
    // Nothing to wait for here: stop() sets the recorder's own stop flag and joins the
    // feeders, which is how they learn that the hub is gone.
    let summary = stop().expect("a summary");
    assert!(summary.frames > 0);
    let b = std::fs::read(&summary.files[0]).unwrap();
    assert_eq!(u32at(&b, 4) as usize, b.len() - 8);
    assert!(b.windows(4).any(|w| w == b"idx1"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_summary_describes_itself_for_the_log() {
    let mut s = Summary {
        files: vec!["/tmp/rec-1.avi".to_string()],
        frames: 300,
        secs: 10.0,
        ..Default::default()
    };
    let text = s.describe();
    assert!(text.contains("300 frame(s)"), "{}", text);
    assert!(text.contains("10.0 s"), "{}", text);
    assert!(text.contains("rec-1.avi"), "{}", text);
    s.files.clear();
    assert_eq!(s.describe(), "nothing was captured");
}

#[test]
fn the_record_directory_follows_the_environment() {
    let _turn = one_recorder();
    std::env::remove_var("UVCWEB_RECORD_DIR");
    assert_eq!(default_dir(), DEFAULT_DIR);
    // A blank value is no value at all.
    std::env::set_var("UVCWEB_RECORD_DIR", "   ");
    assert_eq!(default_dir(), DEFAULT_DIR);

    // The variable decides where the files go, and an empty option (a bare -R) means
    // "use that" rather than "use the default folder".
    let dir = scratch("resolve");
    std::env::set_var("UVCWEB_RECORD_DIR", dir.to_str().unwrap());
    let hub = Hub::new();
    start_with(&hub, "").expect("start");
    hub.submit_frame(&jpegish(2000, 7), 64, 48); // the hub wants a real-sized JPEG
    wait_for("the file to be written", || {
        std::fs::read_dir(&dir)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    });
    stop();
    let written = std::fs::read_dir(&dir)
        .expect("the folder from the variable")
        .filter_map(|e| e.ok())
        .count();
    assert_eq!(written, 1, "the file went to the folder from the variable");
    let _ = std::fs::remove_dir_all(&dir);
    std::env::remove_var("UVCWEB_RECORD_DIR");
}
