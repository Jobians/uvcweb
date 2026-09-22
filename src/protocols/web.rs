//! HTTP protocol: viewer page, MJPEG stream, WAV audio stream, snapshot, status.
//! Behaves like the C version (same URLs, same log lines).

use super::{serve_tcp, Ctx, Protocol};
use crate::hub::Hub;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

const PAGE: &str = include_str!("web_page.html");

struct Web;

pub fn create() -> Box<dyn Protocol> {
    Box::new(Web)
}

impl Protocol for Web {
    fn start(&self, ctx: Ctx) -> io::Result<()> {
        let listener = TcpListener::bind((ctx.bind, ctx.port))?;
        say!(
            "open http://127.0.0.1:{} in your browser{}",
            ctx.port,
            if ctx.cfg.lan { " (also reachable from the LAN, no password)" } else { "" }
        );
        let hub = ctx.hub.clone();
        serve_tcp(&ctx.hub, listener, move |s| client(s, &hub));
        Ok(())
    }
}

fn reply(s: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        status,
        ctype,
        body.len()
    );
    if s.write_all(head.as_bytes()).is_ok() && !body.is_empty() {
        let _ = s.write_all(body);
    }
}

/// Read the request head and return the requested path (query string included).
fn read_request_path(s: &mut TcpStream) -> Option<String> {
    let mut buf = [0u8; 2048];
    let mut got = 0usize;
    while got < buf.len() {
        let n = s.read(&mut buf[got..]).ok()?;
        if n == 0 {
            return None;
        }
        got += n;
        if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf[..got]).into_owned();
    let line = text.lines().next()?;
    let mut it = line.split_whitespace();
    if it.next()? != "GET" {
        return None;
    }
    Some(it.next()?.to_string())
}

fn client(mut s: TcpStream, hub: &Arc<Hub>) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
    let _ = s.set_nodelay(true);
    let full = match read_request_path(&mut s) {
        Some(p) => p,
        None => return,
    };
    let path = full.split('?').next().unwrap_or("").to_string();
    match path.as_str() {
        "/" => reply(&mut s, "200 OK", "text/html", PAGE.as_bytes()),
        "/stream" => serve_stream(&mut s, hub),
        "/audio" => {
            if hub.audio_format().is_none() {
                reply(&mut s, "503 Service Unavailable", "text/plain", b"audio not available\n");
            } else {
                serve_audio(&mut s, hub);
            }
        }
        "/snapshot" => match hub.latest_frame() {
            Some(f) => {
                reply(&mut s, "200 OK", "image/jpeg", &f.data);
                say!("snapshot served ({} bytes)", f.data.len());
            }
            None => {
                reply(&mut s, "503 Service Unavailable", "text/plain", b"no frame yet\n");
                say!("snapshot served (0 bytes)");
            }
        },
        "/status" => serve_status(&mut s, hub),
        "/favicon.ico" => reply(&mut s, "204 No Content", "text/plain", b""),
        _ => reply(&mut s, "404 Not Found", "text/plain", b"not found\n"),
    }
}

fn serve_status(s: &mut TcpStream, hub: &Arc<Hub>) {
    let st = hub.video_stats();
    let age = st.age.unwrap_or(-1.0);
    let js = format!(
        "{{\"frames\":{},\"fps\":{:.1},\"w\":{},\"h\":{},\"age\":{:.1},\"same\":{},\"clients\":{},\"audio\":{}}}",
        st.total,
        st.fps,
        st.w,
        st.h,
        age,
        st.same,
        st.viewers,
        if hub.audio_format().is_some() { 1 } else { 0 }
    );
    reply(s, "200 OK", "application/json", js.as_bytes());
}

fn serve_stream(s: &mut TcpStream, hub: &Arc<Hub>) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
    if s.write_all(head.as_bytes()).is_err() {
        return;
    }
    let (id, watching) = hub.viewer_join();
    say!("viewer #{} connected ({} watching)", id, watching);
    let mut last = 0u64;
    let mut sent = 0u64;
    let mut why = "server stopping".to_string();
    while !hub.is_stopped() {
        let f = match hub.next_frame(&mut last, Duration::from_secs(1)) {
            Some(f) => f,
            None => continue,
        };
        let part = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", f.data.len());
        let res = s.write_all(part.as_bytes()).and_then(|_| s.write_all(&f.data)).and_then(|_| s.write_all(b"\r\n"));
        if let Err(e) = res {
            why = match e.kind() {
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => "send timeout (browser stalled)".to_string(),
                _ => e.to_string(),
            };
            break;
        }
        sent += 1;
    }
    hub.viewer_leave();
    say!("viewer #{} disconnected after {} frames ({})", id, sent, why);
}

/// 44-byte WAV header with "infinite" length fields, for live PCM.
fn wav_header(rate: u32, channels: u16) -> Vec<u8> {
    let byte_rate = rate * channels as u32 * 2;
    let block_align = channels * 2;
    let mut h: Vec<u8> = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0x7F]);
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&block_align.to_le_bytes());
    h.extend_from_slice(&16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0x7F]);
    h
}

fn serve_audio(s: &mut TcpStream, hub: &Arc<Hub>) {
    let fmt = match hub.audio_format() {
        Some(f) => f,
        None => return,
    };
    let head = "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
    if s.write_all(head.as_bytes()).is_err() || s.write_all(&wav_header(fmt.rate, fmt.channels)).is_err() {
        return;
    }
    let mut next = hub.audio_live_edge(); // start at the live edge, not in the past
    say!("audio listener connected");
    while !hub.is_stopped() {
        if let Some(c) = hub.next_chunk(&mut next, Duration::from_millis(100)) {
            if s.write_all(&c.data).is_err() {
                break;
            }
        }
    }
    say!("audio listener disconnected");
}

#[cfg(test)]
#[path = "../../tests/unit/web_tests.rs"]
mod tests;
