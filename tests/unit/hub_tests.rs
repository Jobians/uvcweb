//! Unit tests for `src/hub.rs`, kept in this separate file so the module itself stays clean.
//! Included there as `#[path = "../tests/unit/hub_tests.rs"] mod tests;` -- it is still logically part of
//! that module (private items stay reachable through `use super::*;`), just not stored inline.

use super::*;

fn jpegish(n: usize, fill: u8) -> Vec<u8> {
    let mut v = vec![fill; n];
    v[0] = 0xFF;
    v[1] = 0xD8;
    v
}

#[test]
fn frames_are_validated_and_counted() {
    let h = Hub::new();
    h.submit_frame(&[0u8; 10], 1, 1); // too small
    h.submit_frame(&vec![1u8; 2000], 1, 1); // not a JPEG
    assert_eq!(h.video_stats().bad, 2);
    h.submit_frame(&jpegish(2000, 7), 640, 480);
    h.submit_frame(&jpegish(2000, 7), 640, 480); // identical
    let s = h.video_stats();
    assert_eq!((s.total, s.same, s.w, s.h), (2, 1, 640, 480));
    h.submit_frame(&jpegish(2000, 8), 640, 480);
    assert_eq!(h.video_stats().same, 0);
}

#[test]
fn video_subscriber_skips_ahead() {
    let h = Hub::new();
    let mut last = 0u64;
    assert!(h.next_frame(&mut last, Duration::from_millis(10)).is_none());
    h.submit_frame(&jpegish(2000, 1), 1, 1);
    h.submit_frame(&jpegish(2000, 2), 1, 1);
    let f = h.next_frame(&mut last, Duration::from_millis(10)).unwrap();
    assert_eq!(f.seq, 2); // got the newest, not a backlog
    assert!(h.next_frame(&mut last, Duration::from_millis(10)).is_none());
}

#[test]
fn audio_subscriber_is_live_and_gapless() {
    let h = Hub::new();
    h.set_audio_format(AudioFormat { rate: 8000, channels: 1 });
    h.push_audio(&[0u8; 100]); // published before anyone subscribed
    let mut next = h.audio_live_edge();
    assert!(h.next_chunk(&mut next, Duration::from_millis(10)).is_none());
    h.push_audio(&[1u8; 100]);
    h.push_audio(&[2u8; 100]);
    let a = h.next_chunk(&mut next, Duration::from_millis(10)).unwrap();
    let b = h.next_chunk(&mut next, Duration::from_millis(10)).unwrap();
    assert_eq!((a.data[0], a.pos, b.data[0], b.pos), (1, 50, 2, 100));
}

#[test]
fn stop_wakes_subscribers_and_joins_threads() {
    let h = Hub::new();
    let waiter = {
        let h = h.clone();
        std::thread::spawn(move || {
            let mut last = 0u64;
            h.next_frame(&mut last, Duration::from_secs(30)).is_none()
        })
    };
    h.track(std::thread::spawn(|| {}));
    std::thread::sleep(Duration::from_millis(50));
    assert!(!h.is_stopped());
    h.request_stop();
    assert!(h.is_stopped());
    assert!(waiter.join().unwrap()); // returned quickly with None instead of waiting 30 s
    h.join_tracked();
}

#[test]
fn global_can_be_replaced_between_sessions() {
    let a = Hub::new();
    set_global(Some(a.clone()));
    assert!(Arc::ptr_eq(&try_global().unwrap(), &a));
    set_global(None);
    assert!(try_global().is_none());
    let b = Hub::new();
    set_global(Some(b.clone()));
    assert!(Arc::ptr_eq(&try_global().unwrap(), &b));
    set_global(None);
}

#[test]
fn audio_ring_drops_oldest() {
    let h = Hub::new();
    h.set_audio_format(AudioFormat { rate: 100, channels: 1 }); // cap = 400 bytes
    for i in 0..20u8 {
        h.push_audio(&[i; 100]);
    }
    let mut next = 0u64; // a subscriber that is far behind
    let c = h.next_chunk(&mut next, Duration::from_millis(10)).unwrap();
    assert!(c.seq >= 16);
}
