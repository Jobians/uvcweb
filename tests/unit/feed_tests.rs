// Tests for the encoder feed: the short queue the Android app pulls its pictures
// and sound from. What matters is that a slow reader costs freshness but never
// the stream, and that what comes out is exactly what the hub was given.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::feed;
use crate::hub::{AudioFormat, Hub};

/// A blob the hub accepts as a picture: it only looks at the JPEG markers and
/// the length, which is all the feed cares about.
fn jpegish(n: usize, fill: u8) -> Vec<u8> {
    let mut v = vec![fill; n];
    v[0] = 0xFF;
    v[1] = 0xD8;
    v[n - 2] = 0xFF;
    v[n - 1] = 0xD9;
    v
}

/// The feed has one queue per reader, so these tests take turns. A test that
/// panics half way must not leave a queue behind for the next one.
fn one_reader() -> std::sync::MutexGuard<'static, ()> {
    let guard = feed::test_slot();
    feed::disarm();
    guard
}

fn a_hub() -> Arc<Hub> {
    let hub = Hub::new();
    hub.set_audio_format(AudioFormat {
        rate: 16_000,
        channels: 2,
    });
    hub
}

/// Waits for something the feed does on its own thread.
fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..150 {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    feed::disarm();
    panic!("timed out waiting for {}", what);
}

/// A reader big enough for anything these tests publish.
fn big_enough() -> Vec<u8> {
    vec![0u8; 4 << 20]
}

/// Reads everything waiting, growing the buffer when the feed asks for more.
fn drain(kind: i32, buf: &mut Vec<u8>) -> Vec<(u64, Vec<u8>)> {
    let mut out = Vec::new();
    while let pts @ 0.. = feed::peek_pts(kind) {
        let n = loop {
            let n = feed::pull(kind, buf);
            if n == feed::TOO_SMALL {
                buf.resize(buf.len() * 2, 0);
                continue;
            }
            break n;
        };
        out.push((pts as u64, buf[..n as usize].to_vec()));
    }
    out
}

#[test]
fn a_reader_gets_the_picture_the_hub_published() {
    let _guard = one_reader();
    let hub = a_hub();
    feed::arm_with(&hub).expect("arm");
    hub.submit_frame(&jpegish(2000, 7), 64, 48);

    wait_for("a picture", || feed::peek_pts(feed::VIDEO) >= 0);
    let mut buf = vec![0u8; 64];
    let n = loop {
        let n = feed::pull(feed::VIDEO, &mut buf);
        if n == feed::TOO_SMALL {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        break n;
    };
    assert!(n > 0, "a picture came out");
    assert_eq!(
        &buf[..n as usize],
        &jpegish(2000, 7)[..],
        "the bytes are the card's"
    );
    assert_eq!(feed::peek_pts(feed::VIDEO), feed::EMPTY, "nothing is left");
    feed::disarm();
}

#[test]
fn a_buffer_that_is_too_small_keeps_the_picture() {
    let _guard = one_reader();
    let hub = a_hub();
    feed::arm_with(&hub).expect("arm");
    hub.submit_frame(&jpegish(3000, 8), 64, 48);
    wait_for("a picture", || feed::peek_pts(feed::VIDEO) >= 0);

    let mut tiny = vec![0u8; 16];
    assert_eq!(feed::pull(feed::VIDEO, &mut tiny), feed::TOO_SMALL);
    // Still there, still first: the reader just asks again with more room.
    assert!(feed::peek_pts(feed::VIDEO) >= 0);
    let mut big = vec![0u8; 64 * 1024];
    let n = feed::pull(feed::VIDEO, &mut big);
    assert!(n > 16, "the same picture, once there is room");
    assert_eq!(&big[..n as usize], &jpegish(3000, 8)[..]);
    feed::disarm();
}

#[test]
fn pictures_come_out_in_order_with_rising_timestamps() {
    let _guard = one_reader();
    let hub = a_hub();
    feed::arm_with(&hub).expect("arm");
    wait_for("the reader to attach", feed::is_attached);
    for i in 0..3u8 {
        hub.submit_frame(&jpegish(2000, 20 + i), 320, 240);
        std::thread::sleep(Duration::from_millis(60));
    }
    wait_for("three pictures", || feed::stats().frames >= 3);

    let mut buf = big_enough();
    let got = drain(feed::VIDEO, &mut buf);
    assert!(got.len() >= 3, "got {} pictures", got.len());
    for (i, (pts, data)) in got.iter().take(3).enumerate() {
        assert_eq!(data, &jpegish(2000, 20 + i as u8), "in the order they came");
        if i > 0 {
            assert!(
                *pts > got[i - 1].0,
                "time moves forward: {} then {}",
                got[i - 1].0,
                pts
            );
        }
    }
    feed::disarm();
}

#[test]
fn a_reader_that_falls_behind_gets_the_newest_picture() {
    let _guard = one_reader();
    let hub = a_hub();
    feed::arm_with(&hub).expect("arm");
    wait_for("the reader to attach", feed::is_attached);
    // Three pictures, none of them read until the last one is on its way. The
    // hub only keeps the newest, so a late reader must not be handed a backlog:
    // for an encoder a stale picture is worth nothing.
    hub.submit_frame(&jpegish(2000, 31), 320, 240);
    std::thread::sleep(Duration::from_millis(60));
    hub.submit_frame(&jpegish(2000, 32), 320, 240);
    std::thread::sleep(Duration::from_millis(60));
    hub.submit_frame(&jpegish(2000, 33), 320, 240);
    wait_for("the pictures", || feed::stats().frames >= 3);

    let mut buf = big_enough();
    let got = drain(feed::VIDEO, &mut buf);
    let newest = got.last().expect("something to read").1.clone();
    assert_eq!(
        newest,
        jpegish(2000, 33),
        "the newest picture is what arrives"
    );
    feed::disarm();
}

#[test]
fn a_full_queue_makes_room_for_the_newest() {
    let _guard = one_reader();
    let hub = a_hub();
    feed::arm_with(&hub).expect("arm");
    wait_for("the reader to attach", feed::is_attached);
    // Big pictures, so the budget is reached in a handful of frames, and slow
    // enough that the copier sees each one.
    for i in 0..7u8 {
        hub.submit_frame(&jpegish(1_500_000, 40 + i), 1280, 720);
        std::thread::sleep(Duration::from_millis(30));
    }
    wait_for("the queue to overflow", || feed::stats().dropped > 0);

    let st = feed::stats();
    assert!(st.dropped > 0, "something had to go: {:?}", st);
    // Bounded is the promise: at most the budget plus the picture in hand.
    assert!(
        st.queued_bytes <= (6 << 20) + 1_500_000u64 * 2,
        "the queue is bounded, {} bytes deep",
        st.queued_bytes
    );
    // What survived is the recent past: the last picture in is the last out.
    let mut buf = big_enough();
    let got = drain(feed::VIDEO, &mut buf);
    let last = got.last().expect("something to read");
    assert_eq!(
        last.1,
        jpegish(1_500_000, 46),
        "the newest picture survived"
    );
    feed::disarm();
}

#[test]
fn sound_carries_its_own_timing() {
    let _guard = one_reader();
    let hub = a_hub();
    feed::arm_with(&hub).expect("arm");
    wait_for("the reader to attach", feed::is_attached);
    // 16 kHz stereo 16 bit is 4 bytes a sample frame, so 3200 bytes is 50 ms.
    for i in 0..5u16 {
        hub.push_audio(&vec![(i % 7) as u8; 3200]);
    }
    wait_for("sound", || feed::peek_pts(feed::AUDIO) >= 0);

    let mut buf = big_enough();
    let got = drain(feed::AUDIO, &mut buf);
    assert!(got.len() >= 4, "several chunks, got {}", got.len());
    let first = got[0].0 as i64;
    for (i, (pts, data)) in got.iter().enumerate() {
        assert_eq!(data.len(), 3200, "whole chunks");
        let want = first + i as i64 * 50_000;
        assert!(
            (*pts as i64 - want).abs() <= 50_000,
            "chunk {} at {} us, wanted about {} us",
            i,
            pts,
            want
        );
    }
    feed::disarm();
}

#[test]
fn sound_from_before_the_reader_attached_is_not_recorded() {
    let _guard = one_reader();
    let hub = a_hub();
    // A recording starts now, so the two seconds of sound the hub still has
    // from before this moment are not part of it.
    hub.push_audio(&vec![1u8; 3200]);
    feed::arm_with(&hub).expect("arm");
    wait_for("the reader to attach", feed::is_attached);
    let mut buf = big_enough();
    assert!(
        drain(feed::AUDIO, &mut buf).is_empty(),
        "nothing from before"
    );
    hub.push_audio(&vec![2u8; 3200]);
    wait_for("the new sound", || feed::peek_pts(feed::AUDIO) >= 0);
    let got = drain(feed::AUDIO, &mut buf);
    assert_eq!(got.len(), 1, "only the sound from now on");
    assert_eq!(got[0].1[0], 2);
    feed::disarm();
}

#[test]
fn a_reader_sees_the_formats_an_encoder_needs() {
    let _guard = one_reader();
    let hub = a_hub();
    feed::arm_with(&hub).expect("arm");
    let fmt = feed::audio_format().expect("the sound format");
    assert_eq!(fmt.rate, 16_000);
    assert_eq!(fmt.channels, 2);
    assert!(
        feed::video_fps() >= 0.0,
        "a picture rate, or 0 before the first"
    );
    // What the card has sent, so a reader that gets nothing can say whether the
    // card is silent or the reader is.
    assert_eq!(feed::stats().source, 0, "the card has sent nothing yet");
    hub.submit_frame(&jpegish(2000, 60), 64, 48);
    wait_for("the card's own count", || feed::stats().source > 0);
    feed::disarm();
    // Once the reader is gone there is nothing to ask.
    assert!(feed::audio_format().is_none());
    assert!(!feed::is_armed());
}

#[test]
fn only_one_reader_at_a_time() {
    let _guard = one_reader();
    let hub = a_hub();
    feed::arm_with(&hub).expect("arm");
    let again = feed::arm_with(&hub).expect_err("a second reader");
    assert_eq!(again.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(feed::is_armed());
    feed::disarm();
    assert!(!feed::is_armed());
}

#[test]
fn the_stream_ends_when_the_capture_session_does() {
    let _guard = one_reader();
    let hub = a_hub();
    feed::arm_with(&hub).expect("arm");
    assert!(!feed::has_ended(), "the session is still running");
    hub.submit_frame(&jpegish(2000, 50), 64, 48);
    wait_for("a picture", || feed::peek_pts(feed::VIDEO) >= 0);
    // The session goes away (the card was unplugged). A reader has to be able to
    // tell that no more pictures are coming, so it can finish and seal its file.
    hub.request_stop();
    wait_for("the feed to end", feed::has_ended);
    feed::disarm();
}

#[test]
fn a_reader_never_holds_up_the_stream() {
    let _guard = one_reader();
    let hub = a_hub();
    feed::arm_with(&hub).expect("arm");
    wait_for("the reader to attach", feed::is_attached);
    // Nothing is ever read, so the copier is always pushing into a queue nobody
    // looks at. The hub must take pictures just as fast as before.
    let started = Instant::now();
    for i in 0..200u8 {
        hub.submit_frame(&jpegish(2000, i), 320, 240);
    }
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(2),
        "publishing stayed quick: {:?}",
        took
    );
    assert_eq!(hub.video_stats().total, 200, "every picture was taken");
    wait_for("the feed to see them", || feed::stats().frames > 0);
    feed::disarm();
}

/// A session that was already streaming when the reader attached, which is what
/// the app does: the camera has been running for a while before Record is tapped.
#[test]
fn a_reader_attaching_to_a_running_session_gets_the_current_picture() {
    let _guard = one_reader();
    let hub = a_hub();
    for i in 0..30 {
        hub.submit_frame(&jpegish(2000, 100 + i), 1920, 1080);
    }
    feed::arm_with(&hub).expect("arm");
    wait_for("a picture from the running session", || {
        feed::peek_pts(feed::VIDEO) >= 0
    });
    let mut buf = vec![0u8; 512 * 1024];
    let n = feed::pull(feed::VIDEO, &mut buf);
    assert!(n > 0, "a picture came out");
    feed::disarm();
}
