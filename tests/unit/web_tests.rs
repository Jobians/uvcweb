//! Unit tests for `src/protocols/web.rs`, kept in this separate file so the module
//! itself stays clean. Included there as
//! `#[path = "../../tests/unit/web_tests.rs"] mod tests;` -- it is still logically
//! part of that module (private items stay reachable through `use super::*;`),
//! just not stored inline.

use super::*;
use crate::config::Config;
use crate::golden_tests::{hex, JPEG_HEX};
use crate::hub::{self, AudioFormat};
use crate::recorder;
use std::io::Read;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::MutexGuard;

#[test]
fn wav_header_layout_matches_what_the_page_parses() {
    let h = wav_header(96000, 1);
    assert_eq!(h.len(), 44);
    assert_eq!(&h[0..4], b"RIFF");
    // the page reads channels at byte 22 and the rate at bytes 24..28
    assert_eq!(u16::from_le_bytes([h[22], h[23]]), 1);
    assert_eq!(u32::from_le_bytes([h[24], h[25], h[26], h[27]]), 96000);
    assert_eq!(u32::from_le_bytes([h[28], h[29], h[30], h[31]]), 192000); // byte rate
    assert_eq!(&h[36..40], b"data");
}

/// A running web server on a free port, with a hub to feed.
///
/// The record routes reach for the one global hub, so a server takes the recorder
/// slot for as long as it lives: that keeps parallel tests from stealing the global
/// hub (or the one recording) from under each other.
struct Server {
    port: u16,
    hub: Arc<Hub>,
    _turn: MutexGuard<'static, ()>,
}

impl Server {
    fn start(record_dir: &str) -> Server {
        let turn = recorder::test_slot();
        let hub = Hub::new();
        hub.set_audio_format(AudioFormat {
            rate: 16_000,
            channels: 2,
        });
        // The record routes reach for the global hub, the way the engine installs it.
        hub::set_global(Some(hub.clone()));
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let cfg = Arc::new(Config {
            fd: 0,
            width: 0,
            height: 0,
            fps: 0,
            audio: true,
            audio_rate: 0,
            audio_channels: 0,
            lan: false,
            protocols: vec![],
            av_offset_ms: 0,
            record_dir: record_dir.to_string(),
        });
        Web.start(Ctx {
            hub: hub.clone(),
            cfg,
            bind: "127.0.0.1".parse().unwrap(),
            port,
        })
        .expect("web server");
        Server {
            port,
            hub,
            _turn: turn,
        }
    }

    /// One request, the way a browser makes it: the answer as plain text.
    fn get(&self, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s.write_all(format!("GET {} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n", path).as_bytes())
            .unwrap();
        let mut text = String::new();
        s.read_to_string(&mut text).expect("answer");
        text
    }

    /// The body of a JSON answer, without the HTTP head.
    fn json(&self, path: &str) -> String {
        let text = self.get(path);
        let (head, body) = text.split_once("\r\n\r\n").expect("a head and a body");
        assert!(head.starts_with("HTTP/1.1 200 OK"), "head: {}", head);
        body.to_string()
    }

    /// A few real pictures, so a recording has something in it (the hub drops
    /// anything that is not a JPEG).
    fn feed(&self, frames: usize) {
        for _ in 0..frames {
            self.hub.submit_frame(&hex(JPEG_HEX), 64, 48);
            self.hub.push_audio(&vec![0u8; 320]);
        }
    }

    /// The recorder's feeders run on their own threads and poll the hub twice a second,
    /// so a test waits for the frames to land instead of sleeping a fixed time that a
    /// loaded machine can beat.
    fn wait_for_frame(&self) {
        for _ in 0..100 {
            let status = self.json("/record/status");
            let frames: u64 = status
                .split("\"frames\":")
                .nth(1)
                .and_then(|rest| rest.split([',', '}']).next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            if frames > 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for a recorded frame");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Leave the machine as we found it, also when a test failed half way.
        let _ = recorder::stop();
        hub::set_global(None);
        self.hub.request_stop();
    }
}

/// A unique, empty directory for one test (no external crates, so no tempfile).
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "uvcweb-web-{}-{}-{}",
        name,
        std::process::id(),
        crate::log::file_stamp()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn a_recording_goes_from_nothing_to_a_file_and_back() {
    let dir = scratch("roundtrip");
    let server = Server::start(dir.to_str().unwrap());

    // Nothing running yet.
    assert_eq!(
        server.json("/record/status"),
        r#"{"ok":true,"recording":false}"#
    );
    assert!(server.json("/status").contains(r#""recording":false"#));
    assert!(server.json("/status").contains(r#""record":null"#));

    server.feed(3);
    let started = server.json("/record/start");
    assert!(started.contains(r#""ok":true"#), "{}", started);
    assert!(started.contains(r#""recording":true"#), "{}", started);
    assert!(started.contains(".avi"), "{}", started);
    // Starting twice is refused, and the refusal is an error, not a second recorder.
    let again = server.json("/record/start");
    assert!(again.contains(r#""ok":false"#), "{}", again);
    assert!(again.contains("already"), "{}", again);

    // While it runs, /status carries the numbers the viewer page shows.
    server.wait_for_frame();
    let running = server.json("/status");
    assert!(running.contains(r#""recording":true"#), "{}", running);
    assert!(running.contains(r#""file":"#), "{}", running);
    assert!(running.contains("\"frames\":"), "{}", running);
    let status = server.json("/record/status");
    assert!(status.contains(r#""recording":true"#), "{}", status);

    let stopped = server.json("/record/stop");
    assert!(stopped.contains(r#""ok":true"#), "{}", stopped);
    assert!(stopped.contains("\"frames\":"), "{}", stopped);
    assert!(stopped.contains(".avi"), "{}", stopped);
    assert!(server
        .json("/record/status")
        .contains(r#""recording":false"#));
    assert!(server.json("/status").contains(r#""record":null"#));

    // And the file it names is really on disk.
    let written = std::fs::read_dir(&dir)
        .expect("the record directory")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "avi").unwrap_or(false))
        .collect::<Vec<_>>();
    assert_eq!(written.len(), 1, "one .avi in {:?}", written);
    let bytes = std::fs::read(&written[0]).expect("the file");
    assert!(bytes.len() > 1000, "only {} bytes", bytes.len());
    assert_eq!(&bytes[0..4], b"RIFF", "a real file, not a stub");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stopping_when_nothing_runs_is_an_error_not_a_crash() {
    let dir = scratch("idle");
    let server = Server::start(dir.to_str().unwrap());
    let stopped = server.json("/record/stop");
    assert!(stopped.contains(r#""ok":false"#), "{}", stopped);
    assert!(stopped.contains("not recording"), "{}", stopped);
    // No file is left behind by a stop that had nothing to close.
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_full_directory_is_reported_instead_of_silently_recording_nothing() {
    let blocker = scratch("blocker");
    let file = blocker.join("in-the-way");
    std::fs::write(&file, b"not a directory").unwrap();
    let server = Server::start(file.to_str().unwrap());
    let started = server.json("/record/start");
    assert!(started.contains(r#""ok":false"#), "{}", started);
    assert!(started.contains("error"), "{}", started);
    let _ = std::fs::remove_dir_all(&blocker);
}

#[test]
fn the_viewer_page_offers_exactly_what_the_server_serves() {
    // The page is the only place the record buttons live, so keep the two in step:
    // a route without a button is dead weight, a button without a route is broken.
    // The page polls /status for the numbers, and only start/stop for the button.
    for route in ["/record/start", "/record/stop", "/status"] {
        assert!(PAGE.contains(route), "the page does not mention {}", route);
    }
    for id in [
        "btn-record",
        "btn-fullscreen",
        "btn-landscape",
        "btn-rotate",
        "btn-hide",
    ] {
        assert!(PAGE.contains(id), "the page has no {}", id);
    }
    // The status fields the page reads have to be the ones the server sends. The
    // page watches `record` (null when idle), which is why it needs no boolean too.
    for field in [
        "status.record",
        "status.same",
        "status.audio",
        "status.frames",
    ] {
        assert!(PAGE.contains(field), "the page never reads {}", field);
    }
}

#[test]
fn an_unknown_url_is_a_404_and_not_a_stream() {
    let dir = scratch("404");
    let server = Server::start(dir.to_str().unwrap());
    let text = server.get("/nope");
    assert!(text.starts_with("HTTP/1.1 404"), "{}", text);
    let _ = std::fs::remove_dir_all(&dir);
}
