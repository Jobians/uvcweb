//! Unit tests for `src/protocols/rtsp.rs`, kept in this separate file so the module itself stays clean.
//! Included there as `#[path = "../../tests/unit/rtsp_tests.rs"] mod tests;` -- it is still logically part of
//! that module (private items stay reachable through `use super::*;`), just not stored inline.

use super::*;
use crate::golden_tests::{hex, JPEG_HEX};
use std::net::Shutdown;

#[test]
fn sdp_matches_the_ffmpeg_tested_reference() {
    let got = build_sdp(Some(AudioFormat { rate: 96000, channels: 1 }));
    let want = "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\ns=uvcweb\r\nc=IN IP4 0.0.0.0\r\nt=0 0\r\na=control:*\r\na=range:npt=0-\r\n\
m=video 0 RTP/AVP 26\r\na=rtpmap:26 JPEG/90000\r\na=control:trackID=0\r\n\
m=audio 0 RTP/AVP 97\r\na=rtpmap:97 L16/96000/1\r\na=control:trackID=1\r\n";
    assert_eq!(got, want);
    assert!(!build_sdp(None).contains("m=audio"));
}

#[test]
fn transport_negotiation() {
    match parse_transport("RTP/AVP/TCP;unicast;interleaved=2-3") {
        Some(Chosen::Tcp(2, 3)) => {}
        _ => panic!("tcp"),
    }
    match parse_transport("RTP/AVP;unicast;client_port=5000-5001") {
        Some(Chosen::Udp(5000, 5001)) => {}
        _ => panic!("udp"),
    }
    // several offers: multicast is skipped, the first usable one wins
    match parse_transport("RTP/AVP;multicast;port=1-2, RTP/AVP/UDP;unicast;client_port=6000-6001, RTP/AVP/TCP;interleaved=0-1") {
        Some(Chosen::Udp(6000, 6001)) => {}
        _ => panic!("offer list"),
    }
    assert!(parse_transport("RTP/AVP;multicast").is_none());
    assert!(parse_transport("").is_none());
    assert_eq!(track_id("rtsp://h:8554/live/trackID=1"), Some(1));
    assert_eq!(track_id("rtsp://h:8554/live"), None);
}

#[test]
fn clock_anchor_math() {
    let a = Anchor::now();
    let later = a.inst + Duration::from_millis(100);
    assert_eq!(a.signed_micros(later), 100_000);
    assert_eq!(a.ticks_90k(later), 9_000);
    assert_eq!(a.unix_us(later), a.wall_us + 100_000);
}

fn loopback_pair() -> (TcpStream, TcpStream) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let c = TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let (s, _) = l.accept().unwrap();
    (c, s)
}

#[test]
fn request_parser_skips_interleaved_frames() {
    let (mut c, s) = loopback_pair();
    c.write_all(b"$\x00\x00\x04abcd").unwrap(); // an RTCP receiver report from the client
    c.write_all(b"OPTIONS rtsp://x/live RTSP/1.0\r\nCSeq: 5\r\nUser-Agent: t\r\n\r\n").unwrap();
    let mut rd = BufReader::new(s);
    let r = read_request(&mut rd).unwrap().unwrap();
    assert_eq!((r.method.as_str(), r.url.as_str(), r.header("CSeq")), ("OPTIONS", "rtsp://x/live", Some("5")));
    c.shutdown(Shutdown::Both).unwrap();
    assert!(read_request(&mut rd).unwrap().is_none());
}

/// Send a request and read its response, skipping interleaved RTP/RTCP frames that may precede it.
fn exchange(c: &mut TcpStream, rd: &mut BufReader<TcpStream>, req: &str) -> (String, Vec<u8>) {
    c.write_all(req.as_bytes()).unwrap();
    loop {
        let first = rd.fill_buf().unwrap()[0];
        if first != b'$' {
            break;
        }
        let mut hd = [0u8; 4];
        rd.read_exact(&mut hd).unwrap();
        let n = ((hd[2] as usize) << 8) | (hd[3] as usize);
        let mut skip = vec![0u8; n];
        rd.read_exact(&mut skip).unwrap();
    }
    let mut head = String::new();
    let mut clen = 0usize;
    loop {
        let mut l = String::new();
        rd.read_line(&mut l).unwrap();
        if l.trim().is_empty() {
            break;
        }
        let lower = l.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            clen = v.trim().parse().unwrap();
        }
        head.push_str(&l);
    }
    let mut body = vec![0u8; clen];
    rd.read_exact(&mut body).unwrap();
    (head, body)
}

/// Full session against ourselves: DESCRIBE, SETUP x2 (TCP interleaved), PLAY, receive RTP + RTCP.
#[test]
fn end_to_end_tcp_session() {
    let hub = Hub::new();
    hub.set_audio_format(AudioFormat { rate: 8000, channels: 1 });
    let cfg = Arc::new(Config {
        fd: 0,
        width: 0,
        height: 0,
        fps: 0,
        audio: true,
        audio_rate: 8000,
        audio_channels: 1,
        lan: false,
        protocols: vec![],
        av_offset_ms: 0,
    });
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    Rtsp.start(Ctx { hub: hub.clone(), cfg, bind: "127.0.0.1".parse().unwrap(), port }).unwrap();

    // a feeder standing in for the camera and the USB audio
    let feed_hub = hub.clone();
    let feeder = thread::spawn(move || {
        let jpeg_bytes = hex(JPEG_HEX);
        for i in 0..100u32 {
            feed_hub.submit_frame(&jpeg_bytes, 64, 48);
            let mut pcm = Vec::new();
            for k in 0..80u32 {
                pcm.extend_from_slice(&(((i * 80 + k) % 30000) as i16).to_le_bytes());
            }
            feed_hub.push_audio(&pcm);
            thread::sleep(Duration::from_millis(20));
        }
    });

    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut rd = BufReader::new(c.try_clone().unwrap());
    let base = format!("rtsp://127.0.0.1:{}/live", port);
    let (h, body) = exchange(&mut c, &mut rd, &format!("DESCRIBE {} RTSP/1.0\r\nCSeq: 1\r\n\r\n", base));
    assert!(h.starts_with("RTSP/1.0 200 OK"), "{}", h);
    let sdp = String::from_utf8(body).unwrap();
    assert!(sdp.contains("m=video 0 RTP/AVP 26") && sdp.contains("L16/8000/1"), "{}", sdp);
    let (h, _) = exchange(&mut c, &mut rd, &format!("SETUP {}/trackID=0 RTSP/1.0\r\nCSeq: 2\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n", base));
    assert!(h.contains("interleaved=0-1") && h.contains("Session:"), "{}", h);
    let (h, _) = exchange(&mut c, &mut rd, &format!("SETUP {}/trackID=1 RTSP/1.0\r\nCSeq: 3\r\nTransport: RTP/AVP/TCP;unicast;interleaved=2-3\r\n\r\n", base));
    assert!(h.contains("interleaved=2-3"), "{}", h);
    let (h, _) = exchange(&mut c, &mut rd, &format!("PLAY {} RTSP/1.0\r\nCSeq: 4\r\n\r\n", base));
    assert!(h.starts_with("RTSP/1.0 200 OK"), "{}", h);

    // read interleaved frames until we have seen video, audio and both sender reports
    let (mut video, mut audio, mut sr_v, mut sr_a) = (0, 0, false, false);
    for _ in 0..400 {
        let mut hd = [0u8; 4];
        rd.read_exact(&mut hd).unwrap();
        assert_eq!(hd[0], b'$');
        let n = ((hd[2] as usize) << 8) | hd[3] as usize;
        let mut pkt = vec![0u8; n];
        rd.read_exact(&mut pkt).unwrap();
        match hd[1] {
            0 => {
                assert_eq!(pkt[0], 0x80);
                assert_eq!(pkt[1] & 0x7F, 26);
                video += 1;
            }
            2 => {
                assert_eq!(pkt[1] & 0x7F, 97);
                audio += 1;
            }
            1 => {
                assert_eq!(pkt[1], 200);
                sr_v = true;
            }
            3 => {
                assert_eq!(pkt[1], 200);
                sr_a = true;
            }
            other => panic!("unexpected channel {}", other),
        }
        if video >= 3 && audio >= 3 && sr_v && sr_a {
            break;
        }
    }
    assert!(video >= 3 && audio >= 3 && sr_v && sr_a, "video {} audio {} sr {} {}", video, audio, sr_v, sr_a);
    let (h, _) = exchange(&mut c, &mut rd, &format!("TEARDOWN {} RTSP/1.0\r\nCSeq: 5\r\n\r\n", base));
    assert!(h.starts_with("RTSP/1.0 200 OK"), "{}", h);
    feeder.join().unwrap();
}
