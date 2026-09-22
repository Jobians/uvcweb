//! RTSP server (RFC 2326): the picture as RTP/JPEG (RFC 2435) and the card's
//! audio as RTP L16 (RFC 3551), over TCP-interleaved or UDP transport, with
//! RTCP sender reports so players can line audio and video up.
//!
//! The session logic is a port of a Python reference that was tested against
//! ffmpeg over TCP and UDP, for 4:2:2 / 4:2:0 / restart-marker JPEGs and for
//! mono 96 kHz and stereo 48 kHz audio.
//!
//! URL: rtsp://HOST:PORT/live   (any path works)

use super::{serve_tcp, Ctx, Protocol};
use crate::config::Config;
use crate::hub::{AudioFormat, Hub};
use crate::{jpeg, rtp};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VIDEO_PAYLOAD: usize = 1400; // RTP payload budget per video packet
const AUDIO_PAYLOAD: usize = 1200; // and per audio packet
const SR_INTERVAL: Duration = Duration::from_secs(2);

struct Rtsp;

pub fn create() -> Box<dyn Protocol> {
    Box::new(Rtsp)
}

impl Protocol for Rtsp {
    fn start(&self, ctx: Ctx) -> io::Result<()> {
        let listener = TcpListener::bind((ctx.bind, ctx.port))?;
        say!(
            "RTSP ready: rtsp://127.0.0.1:{}/live{}",
            ctx.port,
            if ctx.cfg.lan {
                " (also reachable from the LAN, no password)"
            } else {
                ""
            }
        );
        let hub = ctx.hub.clone();
        let cfg = ctx.cfg.clone();
        serve_tcp(&ctx.hub, listener, move |s| {
            handle_conn(s, hub.clone(), cfg.clone())
        });
        Ok(())
    }
}

// ---------------------------------------------------------------- clocks

/// Ties the monotonic clock to wall-clock time at the moment PLAY starts.
#[derive(Clone, Copy)]
struct Anchor {
    wall_us: i64, // microseconds since the Unix epoch
    inst: Instant,
}

impl Anchor {
    fn now() -> Anchor {
        let us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);
        Anchor {
            wall_us: us,
            inst: Instant::now(),
        }
    }
    /// t - anchor, in microseconds (negative if t is earlier).
    fn signed_micros(&self, t: Instant) -> i64 {
        if t >= self.inst {
            t.duration_since(self.inst).as_micros() as i64
        } else {
            -(self.inst.duration_since(t).as_micros() as i64)
        }
    }
    fn unix_us(&self, t: Instant) -> i64 {
        self.wall_us + self.signed_micros(t)
    }
    fn ticks_90k(&self, t: Instant) -> i64 {
        self.signed_micros(t) * 9 / 100
    }
}

// ---------------------------------------------------------------- output sinks

enum Sink {
    Tcp {
        wr: Arc<Mutex<TcpStream>>,
        rtp_ch: u8,
        rtcp_ch: u8,
    },
    Udp {
        rtp: UdpSocket,
        rtcp: UdpSocket,
        peer_rtp: SocketAddr,
        peer_rtcp: SocketAddr,
    },
}

fn send_interleaved(wr: &Arc<Mutex<TcpStream>>, ch: u8, pkt: &[u8]) -> io::Result<()> {
    let mut v: Vec<u8> = Vec::with_capacity(4 + pkt.len());
    v.push(b'$');
    v.push(ch);
    v.push((pkt.len() >> 8) as u8);
    v.push(pkt.len() as u8);
    v.extend_from_slice(pkt);
    let mut s = wr.lock().unwrap_or_else(|e| e.into_inner());
    s.write_all(&v)
}

impl Sink {
    fn send_rtp(&self, pkt: &[u8]) -> io::Result<()> {
        match self {
            Sink::Tcp { wr, rtp_ch, .. } => send_interleaved(wr, *rtp_ch, pkt),
            Sink::Udp { rtp, peer_rtp, .. } => rtp.send_to(pkt, *peer_rtp).map(|_| ()),
        }
    }
    fn send_rtcp(&self, pkt: &[u8]) -> io::Result<()> {
        match self {
            Sink::Tcp { wr, rtcp_ch, .. } => send_interleaved(wr, *rtcp_ch, pkt),
            Sink::Udp {
                rtcp, peer_rtcp, ..
            } => rtcp.send_to(pkt, *peer_rtcp).map(|_| ()),
        }
    }
    fn kind(&self) -> &'static str {
        match self {
            Sink::Tcp { .. } => "tcp",
            Sink::Udp { .. } => "udp",
        }
    }
}

// ---------------------------------------------------------------- senders (one thread per track)

fn video_sender(hub: Arc<Hub>, sink: Sink, alive: Arc<AtomicBool>, anchor: Anchor, av_off_us: i64) {
    let ssrc = rtp::rand_u32();
    let mut seq = rtp::rand_u32() as u16;
    let base = rtp::rand_u32();
    let mut last = 0u64;
    let mut pkts_sent = 0u32;
    let mut octets = 0u32;
    let mut next_sr = Instant::now();
    let mut warned_err = false;
    let mut warned_huff = false;
    while alive.load(Ordering::Relaxed) && !hub.is_stopped() {
        let got = hub.next_frame(&mut last, Duration::from_millis(500));
        let now = Instant::now();
        if let Some(f) = got {
            match jpeg::analyze(&f.data) {
                Err(e) => {
                    if !warned_err {
                        say!("rtsp: video frame can't be sent as RTP/JPEG: {}", e);
                        warned_err = true;
                    }
                }
                Ok(info) => {
                    if info.custom_huffman && !warned_huff {
                        say!("rtsp: warning: the camera uses its own Huffman tables; RTP/JPEG assumes the standard ones, so colours may be wrong in RTSP players");
                        warned_huff = true;
                    }
                    let ts = base.wrapping_add(anchor.ticks_90k(f.at) as u32);
                    let pkts =
                        rtp::packetize_jpeg(&info, &f.data, ts, ssrc, &mut seq, VIDEO_PAYLOAD);
                    for p in &pkts {
                        if sink.send_rtp(p).is_err() {
                            return;
                        }
                    }
                    pkts_sent = pkts_sent.wrapping_add(pkts.len() as u32);
                    for p in &pkts {
                        octets = octets.wrapping_add((p.len() - 12) as u32);
                    }
                }
            }
        }
        if pkts_sent > 0 && now >= next_sr {
            let ts = base.wrapping_add(anchor.ticks_90k(now) as u32);
            let sr = rtp::sender_report(
                ssrc,
                anchor.unix_us(now) + av_off_us,
                ts,
                pkts_sent,
                octets,
                "uvcweb",
            );
            if sink.send_rtcp(&sr).is_err() {
                return;
            }
            next_sr = now + SR_INTERVAL;
        }
    }
}

fn audio_sender(
    hub: Arc<Hub>,
    sink: Sink,
    alive: Arc<AtomicBool>,
    anchor: Anchor,
    fmt: AudioFormat,
) {
    let ssrc = rtp::rand_u32();
    let mut seq = rtp::rand_u32() as u16;
    let base = rtp::rand_u32();
    let chans = fmt.channels as usize;
    let frame_bytes = chans * 2;
    let mut next = hub.audio_live_edge(); // start live, not 2 s in the past
    let mut pos0: Option<u64> = None;
    let mut pkts_sent = 0u32;
    let mut octets = 0u32;
    let mut next_sr = Instant::now();
    let mut last_pair: Option<(i64, u32)> = None; // (unix micros, rtp timestamp) at the end of the last chunk
    while alive.load(Ordering::Relaxed) && !hub.is_stopped() {
        if let Some(c) = hub.next_chunk(&mut next, Duration::from_millis(500)) {
            let p0 = *pos0.get_or_insert(c.pos);
            let pkts = rtp::packetize_l16(
                &c.data,
                chans,
                c.pos,
                p0,
                base,
                ssrc,
                &mut seq,
                AUDIO_PAYLOAD,
            );
            for p in &pkts {
                if sink.send_rtp(p).is_err() {
                    return;
                }
                octets = octets.wrapping_add((p.len() - 12) as u32);
            }
            pkts_sent = pkts_sent.wrapping_add(pkts.len() as u32);
            let end_rtp =
                base.wrapping_add((c.pos - p0 + (c.data.len() / frame_bytes) as u64) as u32);
            last_pair = Some((anchor.unix_us(c.at), end_rtp));
        }
        let now = Instant::now();
        if now >= next_sr {
            if let Some((us, ts)) = last_pair {
                let sr = rtp::sender_report(ssrc, us, ts, pkts_sent, octets, "uvcweb");
                if sink.send_rtcp(&sr).is_err() {
                    return;
                }
                next_sr = now + SR_INTERVAL;
            }
        }
    }
}

// ---------------------------------------------------------------- RTSP messages

struct Request {
    method: String,
    url: String,
    headers: Vec<(String, String)>, // names lower-cased
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        let n = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == n)
            .map(|(_, v)| v.as_str())
    }
}

/// Read one request. Interleaved RTCP frames ('$' + channel + length) that
/// TCP clients send on the same connection are skipped. Ok(None) = closed.
fn read_request(r: &mut BufReader<TcpStream>) -> io::Result<Option<Request>> {
    let mut line = String::new();
    loop {
        let first = {
            let b = r.fill_buf()?;
            if b.is_empty() {
                return Ok(None);
            }
            b[0]
        };
        if first == b'$' {
            let mut hd = [0u8; 4];
            r.read_exact(&mut hd)?;
            let n = ((hd[2] as usize) << 8) | (hd[3] as usize);
            let mut skip = vec![0u8; n];
            r.read_exact(&mut skip)?;
            continue;
        }
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if line.trim().is_empty() {
            continue;
        }
        break;
    }
    let (method, url) = {
        let mut parts = line.trim().splitn(3, ' ');
        let m = parts.next().unwrap_or("").to_string();
        let u = parts.next().unwrap_or("").to_string();
        (m, u)
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let t = line.trim();
        if t.is_empty() {
            break;
        }
        if let Some((k, v)) = t.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let req = Request {
        method,
        url,
        headers,
    };
    let clen = req
        .header("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    if clen > 0 {
        let mut body = vec![0u8; clen.min(65536)];
        r.read_exact(&mut body)?;
    }
    Ok(Some(req))
}

fn respond(
    wr: &Arc<Mutex<TcpStream>>,
    code: u16,
    reason: &str,
    cseq: &str,
    extra: &[(&str, String)],
    body: &[u8],
) -> io::Result<()> {
    let mut h = format!(
        "RTSP/1.0 {} {}\r\nCSeq: {}\r\nServer: uvcweb\r\n",
        code, reason, cseq
    );
    for (k, v) in extra {
        h.push_str(&format!("{}: {}\r\n", k, v));
    }
    h.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut v = h.into_bytes();
    v.extend_from_slice(body);
    let mut s = wr.lock().unwrap_or_else(|e| e.into_inner());
    s.write_all(&v)
}

fn build_sdp(audio: Option<AudioFormat>) -> String {
    let mut s = String::new();
    s.push_str("v=0\r\n");
    s.push_str("o=- 1 1 IN IP4 0.0.0.0\r\n");
    s.push_str("s=uvcweb\r\n");
    s.push_str("c=IN IP4 0.0.0.0\r\n");
    s.push_str("t=0 0\r\n");
    s.push_str("a=control:*\r\n");
    s.push_str("a=range:npt=0-\r\n");
    s.push_str("m=video 0 RTP/AVP 26\r\n");
    s.push_str("a=rtpmap:26 JPEG/90000\r\n");
    s.push_str("a=control:trackID=0\r\n");
    if let Some(f) = audio {
        s.push_str("m=audio 0 RTP/AVP 97\r\n");
        s.push_str(&format!("a=rtpmap:97 L16/{}/{}\r\n", f.rate, f.channels));
        s.push_str("a=control:trackID=1\r\n");
    }
    s
}

enum Chosen {
    Tcp(u8, u8),
    Udp(u16, u16),
}

fn parse_pair<T: std::str::FromStr>(s: &str) -> Option<(T, T)> {
    let mut it = s.splitn(2, '-');
    let a = it.next()?.trim().parse::<T>().ok()?;
    let b = it.next()?.trim().parse::<T>().ok()?;
    Some((a, b))
}

/// Pick the first transport we can serve from the client's (possibly comma separated) offers.
fn parse_transport(h: &str) -> Option<Chosen> {
    for spec in h.split(',') {
        let toks: Vec<&str> = spec.split(';').map(|t| t.trim()).collect();
        if toks.iter().any(|t| *t == "multicast") {
            continue;
        }
        let proto = toks.first().copied().unwrap_or("");
        if proto == "RTP/AVP/TCP" {
            let pair = toks
                .iter()
                .find_map(|t| t.strip_prefix("interleaved="))
                .and_then(parse_pair::<u8>);
            let (a, b) = pair.unwrap_or((0, 1));
            return Some(Chosen::Tcp(a, b));
        }
        if proto == "RTP/AVP" || proto == "RTP/AVP/UDP" {
            let pair = toks
                .iter()
                .find_map(|t| t.strip_prefix("client_port="))
                .and_then(parse_pair::<u16>);
            if let Some((a, b)) = pair {
                return Some(Chosen::Udp(a, b));
            }
        }
    }
    None
}

/// `.../trackID=1` -> 1
fn track_id(url: &str) -> Option<usize> {
    let i = url.rfind("trackID=")?;
    let digits: String = url[i + 8..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<usize>().ok()
}

/// Two consecutive UDP ports (even RTP, odd RTCP).
fn bind_udp_pair() -> io::Result<(UdpSocket, UdpSocket, u16)> {
    for _ in 0..200 {
        let p = ((20000 + rtp::rand_u32() % 20000) as u16) & !1u16;
        if let Ok(a) = UdpSocket::bind(("0.0.0.0", p)) {
            if let Ok(b) = UdpSocket::bind(("0.0.0.0", p + 1)) {
                return Ok((a, b, p));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrInUse,
        "no free UDP port pair",
    ))
}

// ---------------------------------------------------------------- one client connection

fn handle_setup(
    req: &Request,
    cseq: &str,
    wr: &Arc<Mutex<TcpStream>>,
    peer: SocketAddr,
    has_audio: bool,
    session: &str,
    sinks: &mut [Option<Sink>; 2],
) -> io::Result<()> {
    let tid = match track_id(&req.url) {
        Some(t) if t == 0 || (t == 1 && has_audio) => t,
        _ => return respond(wr, 404, "Not Found", cseq, &[], b""),
    };
    let offer = req.header("transport").unwrap_or("").to_string();
    let (sink, transport) = match parse_transport(&offer) {
        None => return respond(wr, 461, "Unsupported Transport", cseq, &[], b""),
        Some(Chosen::Tcp(a, b)) => (
            Sink::Tcp {
                wr: Arc::clone(wr),
                rtp_ch: a,
                rtcp_ch: b,
            },
            format!("RTP/AVP/TCP;unicast;interleaved={}-{}", a, b),
        ),
        Some(Chosen::Udp(a, b)) => match bind_udp_pair() {
            Ok((rtp_s, rtcp_s, sp)) => (
                Sink::Udp {
                    rtp: rtp_s,
                    rtcp: rtcp_s,
                    peer_rtp: SocketAddr::new(peer.ip(), a),
                    peer_rtcp: SocketAddr::new(peer.ip(), b),
                },
                format!(
                    "RTP/AVP;unicast;client_port={}-{};server_port={}-{}",
                    a,
                    b,
                    sp,
                    sp + 1
                ),
            ),
            Err(_) => return respond(wr, 500, "Internal Server Error", cseq, &[], b""),
        },
    };
    sinks[tid] = Some(sink);
    respond(
        wr,
        200,
        "OK",
        cseq,
        &[
            ("Transport", transport),
            ("Session", format!("{};timeout=60", session)),
        ],
        b"",
    )
}

fn start_senders(
    hub: &Arc<Hub>,
    cfg: &Config,
    alive: &Arc<AtomicBool>,
    sinks: &mut [Option<Sink>; 2],
    audio: Option<AudioFormat>,
) {
    let anchor = Anchor::now();
    if let Some(sink) = sinks[0].take() {
        let h = Arc::clone(hub);
        let a = Arc::clone(alive);
        let off = cfg.av_offset_ms as i64 * 1000;
        thread::spawn(move || video_sender(h, sink, a, anchor, off));
    }
    if let Some(sink) = sinks[1].take() {
        if let Some(fmt) = audio {
            let h = Arc::clone(hub);
            let a = Arc::clone(alive);
            thread::spawn(move || audio_sender(h, sink, a, anchor, fmt));
        }
    }
}

fn handle_conn(stream: TcpStream, hub: Arc<Hub>, cfg: Arc<Config>) {
    let peer = match stream.peer_addr() {
        Ok(a) => a,
        Err(_) => return,
    };
    // Generous: UDP clients only talk to us for keep-alives (usually every 30 s).
    let _ = stream.set_read_timeout(Some(Duration::from_secs(65)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_nodelay(true);
    let wr = match stream.try_clone() {
        Ok(s) => Arc::new(Mutex::new(s)),
        Err(_) => return,
    };
    let mut rd = BufReader::new(stream);
    let session = format!("{:08X}", rtp::rand_u32());
    let alive = Arc::new(AtomicBool::new(true));
    let audio = hub.audio_format();
    let mut sinks: [Option<Sink>; 2] = [None, None];
    let mut playing = false;
    let mut teardown = false;
    say!("rtsp: client {} connected", peer);

    while !teardown && !hub.is_stopped() {
        let req = match read_request(&mut rd) {
            Ok(Some(r)) => r,
            _ => break,
        };
        let cseq = req.header("cseq").unwrap_or("0").to_string();
        let sent = match req.method.as_str() {
            "OPTIONS" => respond(
                &wr,
                200,
                "OK",
                &cseq,
                &[(
                    "Public",
                    "OPTIONS, DESCRIBE, SETUP, PLAY, PAUSE, TEARDOWN, GET_PARAMETER, SET_PARAMETER"
                        .to_string(),
                )],
                b"",
            ),
            "DESCRIBE" => {
                let base = if req.url.ends_with('/') {
                    req.url.clone()
                } else {
                    format!("{}/", req.url)
                };
                let sdp = build_sdp(audio);
                respond(
                    &wr,
                    200,
                    "OK",
                    &cseq,
                    &[
                        ("Content-Base", base),
                        ("Content-Type", "application/sdp".to_string()),
                    ],
                    sdp.as_bytes(),
                )
            }
            "SETUP" => handle_setup(
                &req,
                &cseq,
                &wr,
                peer,
                audio.is_some(),
                &session,
                &mut sinks,
            ),
            "PLAY" => {
                if !playing && sinks.iter().all(|s| s.is_none()) {
                    respond(&wr, 455, "Method Not Valid in This State", &cseq, &[], b"")
                } else {
                    // answer first, then start sending, so the response is never preceded by RTP data
                    let r = respond(
                        &wr,
                        200,
                        "OK",
                        &cseq,
                        &[
                            ("Range", "npt=0.000-".to_string()),
                            ("Session", session.clone()),
                        ],
                        b"",
                    );
                    if r.is_ok() && !playing {
                        playing = true;
                        let kinds: Vec<&str> = sinks
                            .iter()
                            .filter_map(|s| s.as_ref().map(|k| k.kind()))
                            .collect();
                        say!(
                            "rtsp: client {} playing ({} track(s) over {})",
                            peer,
                            kinds.len(),
                            kinds.first().copied().unwrap_or("?")
                        );
                        start_senders(&hub, &cfg, &alive, &mut sinks, audio);
                    }
                    r
                }
            }
            // A live picture cannot be paused; we accept the request and keep streaming.
            "PAUSE" | "GET_PARAMETER" | "SET_PARAMETER" => {
                respond(&wr, 200, "OK", &cseq, &[("Session", session.clone())], b"")
            }
            "TEARDOWN" => {
                teardown = true;
                respond(&wr, 200, "OK", &cseq, &[("Session", session.clone())], b"")
            }
            _ => respond(&wr, 501, "Not Implemented", &cseq, &[], b""),
        };
        if sent.is_err() {
            break;
        }
    }
    alive.store(false, Ordering::Relaxed);
    say!("rtsp: client {} disconnected", peer);
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
#[path = "../../tests/unit/rtsp_tests.rs"]
mod tests;
