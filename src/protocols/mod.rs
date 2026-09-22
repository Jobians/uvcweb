//! Protocol plug-in point.
//!
//! A protocol is anything that serves the hub's video / audio to clients.
//! To add one (WebRTC, HLS, SRT, ...):
//!   1. create `src/protocols/<name>.rs` with a type implementing [`Protocol`]
//!      and a `pub fn create() -> Box<dyn Protocol>`;
//!   2. add `pub mod <name>;` below;
//!   3. add one line to [`REGISTRY`].
//!
//! The command line (`-P name`, `-p name=port`, `--help`) picks it up automatically.
//!
//! `start` must bind its sockets, spawn its own threads and return quickly.
//! Threads should exit when `ctx.hub.is_stopped()` becomes true. Accept loops started with
//! `serve_tcp` are tracked by the hub, so a restart can wait until their ports are free.
//! Subscribe to the data with `ctx.hub.next_frame(..)` (video) and
//! `ctx.hub.next_chunk(..)` (audio, S16LE PCM); see `web.rs` for the simplest example.

use crate::config::Config;
use crate::hub::Hub;
use std::io;
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub mod rtsp;
pub mod web;

/// Everything a protocol needs to start serving.
pub struct Ctx {
    pub hub: Arc<Hub>,
    pub cfg: Arc<Config>,
    pub bind: IpAddr,
    pub port: u16,
}

pub trait Protocol: Send + Sync {
    fn start(&self, ctx: Ctx) -> io::Result<()>;
}

pub struct ProtocolInfo {
    pub name: &'static str,
    pub about: &'static str,
    pub default_port: u16,
    pub create: fn() -> Box<dyn Protocol>,
}

pub static REGISTRY: &[ProtocolInfo] = &[
    ProtocolInfo {
        name: "web",
        about: "HTTP viewer page, MJPEG /stream, WAV /audio, /snapshot, /status",
        default_port: 8080,
        create: web::create,
    },
    ProtocolInfo {
        name: "rtsp",
        about: "RTSP server: MJPEG (RTP/JPEG) + PCM audio, over TCP or UDP",
        default_port: 8554,
        create: rtsp::create,
    },
];

pub fn find(name: &str) -> Option<&'static ProtocolInfo> {
    REGISTRY.iter().find(|p| p.name == name)
}

pub fn bind_addr(lan: bool) -> IpAddr {
    if lan {
        IpAddr::from([0, 0, 0, 0])
    } else {
        IpAddr::from([127, 0, 0, 1])
    }
}

/// Shared accept loop for TCP based protocols: one thread per connection,
/// stops accepting when the program shuts down.
pub fn serve_tcp<F>(hub: &Arc<Hub>, listener: TcpListener, handler: F)
where
    F: Fn(TcpStream) + Send + Sync + 'static,
{
    let handler = Arc::new(handler);
    let session = Arc::clone(hub);
    let accept_thread = thread::spawn(move || {
        let _ = listener.set_nonblocking(true);
        while !session.is_stopped() {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let h = Arc::clone(&handler);
                    thread::spawn(move || (*h)(stream));
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50))
                }
                Err(_) => thread::sleep(Duration::from_millis(200)),
            }
        }
    });
    hub.track(accept_thread);
}
