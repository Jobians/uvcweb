//! Command line parsing. Protocol names/ports come from `protocols::REGISTRY`,
//! so adding a protocol never requires touching this file.

use crate::protocols;

pub struct Config {
    pub fd: i32,
    pub width: u32, // 0 = use the card's default MJPEG mode
    pub height: u32,
    pub fps: u32,
    pub audio: bool,
    pub audio_rate: u32, // preferred; the card's own rate wins if it differs
    pub audio_channels: u16,
    pub lan: bool,                     // listen on all interfaces instead of loopback
    pub protocols: Vec<(String, u16)>, // chosen protocols with their ports
    pub av_offset_ms: i32,             // RTSP: shift video timestamps later (+) / earlier (-)
}

impl Config {
    /// Defaults for library use (the Android app fills in what it needs afterwards):
    /// web viewer on port 8080, audio on, the card's own video mode.
    pub fn for_fd(fd: i32) -> Config {
        Config {
            fd,
            width: 0,
            height: 0,
            fps: 0,
            audio: true,
            audio_rate: 48000,
            audio_channels: 2,
            lan: false,
            protocols: vec![("web".to_string(), 8080)],
            av_offset_ms: 0,
        }
    }
}

pub fn usage() -> String {
    let mut s = String::new();
    s.push_str("usage: termux-usb -r -e \"./uvcweb [options]\" DEVICE\n\n");
    s.push_str("video / audio:\n");
    s.push_str("  -w W -h H -f FPS   MJPEG mode (default: the card's own default mode)\n");
    s.push_str(
        "  -a usb|off         audio from the card's USB audio interface (default) or none\n",
    );
    s.push_str(
        "  -ar RATE -ac CH    preferred audio rate / channels (the card's own values win)\n\n",
    );
    s.push_str("serving:\n");
    s.push_str("  -P LIST            protocols to serve, comma separated (default: web)\n");
    s.push_str("  -p [NAME=]PORT     port; use NAME=PORT when serving several protocols\n");
    s.push_str("  -l                 listen on the LAN too (no password!)\n");
    s.push_str("  -o MS              RTSP audio/video offset in ms (+ = video later)\n\n");
    s.push_str("available protocols:\n");
    for p in protocols::REGISTRY {
        s.push_str(&format!(
            "  {:<6} {} (default port {})\n",
            p.name, p.about, p.default_port
        ));
    }
    s.push_str("\nexamples:\n");
    s.push_str("  -w 640 -h 480 -f 30                 web viewer only\n");
    s.push_str("  -w 640 -h 480 -f 30 -P rtsp         RTSP only\n");
    s.push_str("  -w 640 -h 480 -f 30 -P web,rtsp -l  both, reachable from the LAN\n");
    s
}

fn take<'a>(args: &'a [String], i: &mut usize, last: usize, opt: &str) -> Result<&'a str, String> {
    if *i + 1 >= last {
        return Err(format!("{} needs a value", opt));
    }
    *i += 1;
    Ok(args[*i].as_str())
}

fn num<T: std::str::FromStr>(s: &str, opt: &str) -> Result<T, String> {
    s.parse::<T>()
        .map_err(|_| format!("{}: bad number '{}'", opt, s))
}

pub fn parse(args: &[String]) -> Result<Config, String> {
    if args.len() < 2 || args.iter().any(|a| a == "--help") {
        return Err(usage());
    }
    let last = args.len() - 1; // termux-usb appends the fd as the last argument
    let mut cfg = Config::for_fd(num::<i32>(&args[last], "fd")?);
    cfg.protocols.clear(); // filled from -P / -p below
    let mut names: Vec<String> = Vec::new();
    let mut port_args: Vec<(Option<String>, u16)> = Vec::new();

    let mut i = 1;
    while i < last {
        let a = args[i].as_str();
        match a {
            "-w" => cfg.width = num(take(args, &mut i, last, a)?, a)?,
            "-h" => cfg.height = num(take(args, &mut i, last, a)?, a)?,
            "-f" => cfg.fps = num(take(args, &mut i, last, a)?, a)?,
            "-ar" => cfg.audio_rate = num(take(args, &mut i, last, a)?, a)?,
            "-ac" => cfg.audio_channels = num(take(args, &mut i, last, a)?, a)?,
            "-o" => cfg.av_offset_ms = num(take(args, &mut i, last, a)?, a)?,
            "-l" => cfg.lan = true,
            "-a" => {
                let v = take(args, &mut i, last, a)?;
                match v {
                    "usb" => cfg.audio = true,
                    "off" => cfg.audio = false,
                    _ => {
                        return Err(format!(
                            "-a {}: only 'usb' (default) or 'off' are supported",
                            v
                        ))
                    }
                }
            }
            "-P" => {
                let v = take(args, &mut i, last, a)?;
                for n in v.split(',') {
                    let n = n.trim().to_lowercase();
                    if n.is_empty() {
                        continue;
                    }
                    if protocols::find(&n).is_none() {
                        return Err(format!(
                            "unknown protocol '{}' (available: {})",
                            n,
                            protocol_names()
                        ));
                    }
                    if !names.contains(&n) {
                        names.push(n);
                    }
                }
            }
            "-p" => {
                let v = take(args, &mut i, last, a)?;
                match v.split_once('=') {
                    Some((n, p)) => port_args.push((Some(n.trim().to_lowercase()), num(p, a)?)),
                    None => port_args.push((None, num(v, a)?)),
                }
            }
            _ => return Err(format!("unknown option: {}\n\n{}", a, usage())),
        }
        i += 1;
    }

    if names.is_empty() {
        names.push("web".to_string());
    }
    for (pn, _) in &port_args {
        if let Some(n) = pn {
            if protocols::find(n).is_none() {
                return Err(format!(
                    "-p {}=...: unknown protocol (available: {})",
                    n,
                    protocol_names()
                ));
            }
        }
    }
    for n in &names {
        let info = match protocols::find(n) {
            Some(x) => x,
            None => continue,
        };
        let mut port = info.default_port;
        for (pn, p) in &port_args {
            match pn {
                Some(x) if x == n => port = *p,
                Some(_) => {}
                None => {
                    if names.len() == 1 {
                        port = *p;
                    } else {
                        return Err(
                            "-p PORT is ambiguous with several protocols; use -p NAME=PORT"
                                .to_string(),
                        );
                    }
                }
            }
        }
        if cfg.protocols.iter().any(|(_, existing)| *existing == port) {
            return Err(format!(
                "port {} is used by two protocols; set another with -p {}=PORT",
                port, n
            ));
        }
        cfg.protocols.push((n.clone(), port));
    }
    Ok(cfg)
}

fn protocol_names() -> String {
    protocols::REGISTRY
        .iter()
        .map(|p| p.name)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
#[path = "../tests/unit/config_tests.rs"]
mod tests;
