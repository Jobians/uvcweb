//! The `uvcweb` program (Termux / Linux). All the work is done by the library;
//! this file only parses the command line, handles Ctrl-C and waits.
//!
//! build:  cargo build --release          (needs libuvc and libusb: pkg install libuvc libusb)
//! run:    termux-usb -r -e "./target/release/uvcweb -w 640 -h 480 -f 30 -P web,rtsp" /dev/bus/usb/001/002

use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use uvcweb_core::engine::Engine;
use uvcweb_core::{config, modes, say};

static CLI_STOP: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn signal(signum: c_int, handler: usize) -> usize;
}

extern "C" fn on_signal(_sig: c_int) {
    CLI_STOP.store(true, Ordering::SeqCst);
}

fn install_signals() {
    let handler = on_signal as extern "C" fn(c_int) as usize;
    unsafe {
        signal(2, handler); // SIGINT
        signal(15, handler); // SIGTERM
    }
    // SIGPIPE is already ignored by the Rust runtime.
}

fn main() {
    std::process::exit(real_main());
}

fn real_main() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let cfg = match config::parse(&args) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("{}", msg);
            return 9;
        }
    };
    install_signals();

    if cfg.list_modes {
        return list_modes(cfg.fd);
    }

    let engine = match Engine::start(cfg) {
        Ok(e) => e,
        Err(code) => return code,
    };
    while !CLI_STOP.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
    say!("stopping...");
    engine.stop();
    say!("bye");
    0
}

/// The `--list-modes` action: ask the card what it can do and print it. The card is
/// opened, read and closed again, so nothing is streamed and it can be started on
/// straight afterwards. This is the same detection the Android app uses.
fn list_modes(fd: i32) -> i32 {
    match modes::probe_modes(fd) {
        Ok(found) => {
            println!("the card lists {} MJPEG mode(s):", found.len());
            for m in &found {
                println!(
                    "  {}x{} @ {} fps{}",
                    m.width,
                    m.height,
                    m.fps,
                    if m.is_default {
                        "  (the card's own default)"
                    } else {
                        ""
                    }
                );
            }
            0
        }
        Err(code) => {
            eprintln!("cannot read the card's modes: {}", probe_error(code));
            code
        }
    }
}

/// The three numbers [`modes::probe_modes`] can fail with (see its documentation).
fn probe_error(code: i32) -> &'static str {
    match code {
        1 => "libuvc would not start",
        2 => "the card would not open (it may already be streaming)",
        3 => "the card's descriptors list no MJPEG mode",
        _ => "unknown error",
    }
}
