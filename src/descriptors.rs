//! USB descriptor handling.
//!
//! `read_active_config` is the only unsafe part: it copies libusb's parsed
//! configuration descriptor into plain Rust structs. Everything else here is
//! safe, pure code (and unit tested) that finds
//!   * the UVC MJPEG modes the camera offers, and
//!   * the USB Audio Class (v1) capture alt settings.

use crate::ffi::*;
use std::os::raw::c_int;

pub struct RawEndpoint {
    pub address: u8,
    pub attributes: u8,
    pub max_packet: u16,
    pub extra: Vec<u8>, // class-specific bytes after the endpoint descriptor
}

/// One alternate setting of one interface.
pub struct RawAlt {
    pub interface: u8,
    pub alt: u8,
    pub class: u8,
    pub subclass: u8,
    pub endpoints: Vec<RawEndpoint>,
    pub extra: Vec<u8>, // class-specific descriptors following the interface descriptor
}

unsafe fn bytes(p: *const u8, n: c_int) -> Vec<u8> {
    if p.is_null() || n <= 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(p, n as usize).to_vec()
    }
}

/// Copy the device's active configuration out of libusb.
pub unsafe fn read_active_config(h: *mut UsbHandle) -> Result<Vec<RawAlt>, String> {
    let dev = libusb_get_device(h);
    let mut cfg: *mut ConfigDescriptor = std::ptr::null_mut();
    let r = libusb_get_active_config_descriptor(dev, &mut cfg);
    if r < 0 || cfg.is_null() {
        return Err(format!("cannot read config descriptor: {}", usb_err(r)));
    }
    let c = &*cfg;
    let mut out = Vec::new();
    for i in 0..c.b_num_interfaces as usize {
        let itf = &*c.interface.add(i);
        let n_alt = if itf.num_altsetting > 0 {
            itf.num_altsetting as usize
        } else {
            0
        };
        for j in 0..n_alt {
            let ad = &*itf.altsetting.add(j);
            let mut endpoints = Vec::new();
            for k in 0..ad.b_num_endpoints as usize {
                let e = &*ad.endpoint.add(k);
                endpoints.push(RawEndpoint {
                    address: e.b_endpoint_address,
                    attributes: e.bm_attributes,
                    max_packet: e.w_max_packet_size,
                    extra: bytes(e.extra, e.extra_length),
                });
            }
            out.push(RawAlt {
                interface: ad.b_interface_number,
                alt: ad.b_alternate_setting,
                class: ad.b_interface_class,
                subclass: ad.b_interface_sub_class,
                endpoints,
                extra: bytes(ad.extra, ad.extra_length),
            });
        }
    }
    libusb_free_config_descriptor(cfg);
    Ok(out)
}

/// Walk a run of descriptors: calls `f(descriptor_type, whole_descriptor)`.
fn walk<F: FnMut(u8, &[u8])>(extra: &[u8], mut f: F) {
    let mut o = 0usize;
    while o + 2 <= extra.len() {
        let l = extra[o] as usize;
        if l < 2 || o + l > extra.len() {
            break;
        }
        f(extra[o + 1], &extra[o..o + l]);
        o += l;
    }
}

/// A hex dump of the video-streaming descriptors, for when a card does not list what it
/// was expected to. Bounded so a chatty card cannot flood the log.
pub fn describe_streaming_descriptors(alts: &[RawAlt]) -> String {
    let mut out = String::new();
    for a in alts
        .iter()
        .filter(|a| a.class == 14 && a.subclass == 2 && !a.extra.is_empty())
    {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("video iface {} alt {}:", a.interface, a.alt));
        for b in &a.extra {
            out.push_str(&format!(" {:02x}", b));
            if out.len() > 700 {
                out.push_str(" ...");
                return out;
            }
        }
    }
    out
}

fn u16le(b: &[u8]) -> u32 {
    (b[0] as u32) | ((b[1] as u32) << 8)
}
fn u24le(b: &[u8]) -> u32 {
    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16)
}
fn u32le(b: &[u8]) -> u32 {
    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24)
}

// ------------------------------------------------------------------ video

pub struct MjpegMode {
    pub width: u32,
    pub height: u32,
    pub interval: u32, // default frame interval in 100 ns units
    pub is_default: bool,
}

/// MJPEG frame sizes of the first MJPEG format of the UVC VideoStreaming interface.
pub fn mjpeg_modes(alts: &[RawAlt]) -> Vec<MjpegMode> {
    let mut modes = Vec::new();
    for a in alts.iter().filter(|a| a.class == 14 && a.subclass == 2) {
        let mut in_mjpeg = false;
        let mut done = false;
        let mut default_idx = 0u8;
        walk(&a.extra, |t, d| {
            if t != 0x24 || d.len() < 4 || done {
                return;
            }
            match d[2] {
                0x06 => {
                    // VS_FORMAT_MJPEG
                    if !modes.is_empty() {
                        done = true; // only the first MJPEG format, like the C version
                    } else if d.len() >= 7 {
                        in_mjpeg = true;
                        default_idx = d[5]; // bDefaultFrameIndex, before the 16-byte guidFormat
                    }
                }
                0x04 | 0x10 => in_mjpeg = false, // uncompressed / frame based formats
                0x07 if in_mjpeg && d.len() >= 25 => {
                    // VS_FRAME_MJPEG
                    modes.push(MjpegMode {
                        width: u16le(&d[5..7]),
                        height: u16le(&d[7..9]),
                        interval: u32le(&d[21..25]),
                        is_default: d[3] == default_idx,
                    });
                }
                _ => {}
            }
        });
        if !modes.is_empty() {
            break;
        }
    }
    modes
}

/// The card's own default mode: (width, height, fps).
pub fn default_mode(modes: &[MjpegMode]) -> Option<(u32, u32, u32)> {
    let m = modes
        .iter()
        .find(|m| m.is_default)
        .or_else(|| modes.first())?;

    let fps = 10_000_000_u32.checked_div(m.interval).unwrap_or(30);

    Some((m.width, m.height, fps))
}

/// One picture size and rate the card says it can do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoMode {
    pub width: u32,
    pub height: u32,
    /// The rate to ask the card for, in whole frames a second.
    pub fps: u32,
    /// This is the size and rate the card itself would start on.
    pub is_default: bool,
}

/// Every size and rate the card's first MJPEG format says it can do, largest
/// picture first and, within one size, the fastest rate first.
///
/// A frame descriptor names one size and either a list of frame intervals - the
/// discrete rates the card can do at that size - or a range to pick from. A
/// list is turned into one entry per interval. A range says nothing about which
/// rates inside it work well, so its default and its two ends are offered and
/// nothing else.
///
/// The rates are the ones to hand to `uvc_get_stream_ctrl_format_size`: that is
/// `10_000_000 / interval`, the very division libuvc does to match a rate against
/// a discrete interval, so a rate listed here is a rate the card will accept.
/// A card advertising 29.97 as 333667 in 100 ns units is 29 here, and 29 is what
/// selects it - rounding it to 30 would be refused.
///
/// The same size and rate listed by more than one frame descriptor is one entry.
pub fn mjpeg_mode_list(alts: &[RawAlt]) -> Vec<VideoMode> {
    let mut found: Vec<VideoMode> = Vec::new();
    for a in alts.iter().filter(|a| a.class == 14 && a.subclass == 2) {
        let mut in_mjpeg = false;
        let mut default_idx = 0u8;
        let mut modes: Vec<VideoMode> = Vec::new();
        walk(&a.extra, |t, d| {
            if t != 0x24 || d.len() < 4 {
                return;
            }
            match d[2] {
                0x06 => {
                    if d.len() >= 7 {
                        // Every MJPEG format is read. A card is free to spread its modes
                        // over several of them, and stopping at the first would drop
                        // whatever the later ones say.
                        in_mjpeg = true;
                        default_idx = d[5];
                    }
                }
                0x04 | 0x10 => in_mjpeg = false,
                0x07 if in_mjpeg && d.len() >= 25 => {
                    // VS_FRAME_MJPEG: one size, and how many ways to shoot it.
                    let width = u16le(&d[5..7]);
                    let height = u16le(&d[7..9]);
                    let default_interval = u32le(&d[21..25]);
                    let is_default = d[3] == default_idx;

                    // Byte 25 is bFrameIntervalType: 0 means the rates are a continuous
                    // range, any other value is how many discrete rates follow. Either way
                    // the values start at byte 26, so there is no separate count byte and
                    // the list runs to the end of the descriptor. libuvc reads it from the
                    // same offset, which is the offset the working modes agree with.
                    let mut intervals: Vec<u32> = Vec::new();
                    if d[25] >= 1 {
                        // Discrete: the descriptor carries the whole list.
                        for i in 0..d[25] as usize {
                            let at = 26 + 4 * i;
                            if at + 4 > d.len() {
                                break;
                            }
                            intervals.push(u32le(&d[at..at + 4]));
                        }
                    } else if d.len() >= 34 {
                        // A range: every rate between the two ends, in the steps the card
                        // says it takes. Offering only the ends would hide the rest.
                        let lo = u32le(&d[26..30]);
                        let hi = u32le(&d[30..34]);
                        let step = if d.len() >= 38 { u32le(&d[34..38]) } else { 0 };
                        if lo > 0 && hi >= lo {
                            if step > 0 {
                                let mut iv = lo;
                                while iv <= hi && intervals.len() < 64 {
                                    intervals.push(iv);
                                    match iv.checked_add(step) {
                                        Some(next) => iv = next,
                                        None => break,
                                    }
                                }
                            } else {
                                intervals.push(lo);
                                if hi != lo {
                                    intervals.push(hi);
                                }
                            }
                        }
                    }
                    if intervals.is_empty() {
                        // A descriptor that says nothing useful: the card's own default is
                        // still worth offering, since it is what the camera path uses.
                        intervals.push(default_interval);
                    }

                    for interval in intervals {
                        let fps = match 10_000_000_u32.checked_div(interval) {
                            Some(f) if f > 0 => f,
                            _ => continue, // a card that says zero cannot be taken at its word
                        };
                        modes.push(VideoMode {
                            width,
                            height,
                            fps,
                            is_default: is_default && interval == default_interval,
                        });
                    }
                }
                _ => {}
            }
        });
        if !modes.is_empty() {
            found = modes;
            break;
        }
    }

    let mut out: Vec<VideoMode> = Vec::new();
    for m in found {
        match out
            .iter_mut()
            .find(|k| k.width == m.width && k.height == m.height && k.fps == m.fps)
        {
            Some(k) => k.is_default |= m.is_default,
            None => out.push(m),
        }
    }
    out.sort_by(|a, b| {
        b.width
            .cmp(&a.width)
            .then(b.height.cmp(&a.height))
            .then(b.fps.cmp(&a.fps))
    });
    out
}

// ------------------------------------------------------------------ audio

#[derive(Clone, Debug)]
pub struct AudioAlt {
    pub interface: u8,
    pub alt: u8,
    pub endpoint: u8,
    pub max_packet: usize,
    pub channels: u8,
    pub subframe: u8,
    pub bits: u8,
    pub pcm: bool,
    pub rates: Vec<u32>, // discrete rates; empty = continuous rate_min..rate_max
    pub rate_min: u32,
    pub rate_max: u32,
    pub freq_ctl: bool, // endpoint accepts a SET_CUR sampling frequency
}

/// Every isochronous-IN AudioStreaming alt setting (UAC 1.0).
pub fn audio_alts(alts: &[RawAlt]) -> Vec<AudioAlt> {
    let mut out = Vec::new();
    for a in alts {
        if a.class != 1 || a.subclass != 2 || a.endpoints.is_empty() {
            continue; // not AudioStreaming, or the idle alt 0
        }
        let ep = &a.endpoints[0];
        if ep.address & 0x80 == 0 || ep.attributes & 3 != 1 {
            continue; // not isochronous IN
        }
        let mult = (((ep.max_packet >> 11) & 3) as usize) + 1;
        let mut alt = AudioAlt {
            interface: a.interface,
            alt: a.alt,
            endpoint: ep.address,
            max_packet: ((ep.max_packet & 0x7ff) as usize) * mult,
            channels: 0,
            subframe: 0,
            bits: 0,
            pcm: true,
            rates: Vec::new(),
            rate_min: 0,
            rate_max: 0,
            freq_ctl: false,
        };
        walk(&ep.extra, |t, d| {
            // CS_ENDPOINT / EP_GENERAL: bmAttributes bit 0 = sampling frequency control
            if t == 0x25 && d.len() >= 4 && d[2] == 1 {
                alt.freq_ctl = d[3] & 1 != 0;
            }
        });
        walk(&a.extra, |t, d| {
            if t != 0x24 || d.len() < 3 {
                return;
            }
            if d[2] == 0x01 && d.len() >= 7 {
                // AS_GENERAL: wFormatTag 1 = PCM
                alt.pcm = u16le(&d[5..7]) == 1;
            } else if d[2] == 0x02 && d.len() >= 8 && d[3] == 1 {
                // FORMAT_TYPE_I
                alt.channels = d[4];
                alt.subframe = d[5];
                alt.bits = d[6];
                let n = d[7] as usize;
                if n == 0 {
                    if d.len() >= 14 {
                        alt.rate_min = u24le(&d[8..11]);
                        alt.rate_max = u24le(&d[11..14]);
                    }
                } else {
                    let mut i = 0usize;
                    while i < n && i < 8 && 8 + 3 * i + 3 <= d.len() {
                        alt.rates.push(u24le(&d[8 + 3 * i..11 + 3 * i]));
                        i += 1;
                    }
                }
            }
        });
        out.push(alt);
    }
    out
}

fn abs_diff(a: u32, b: u32) -> u32 {
    a.abs_diff(b)
}

/// The sample rate we will use for `a` given a preference.
pub fn pick_rate(a: &AudioAlt, want: u32) -> u32 {
    if a.rates.is_empty() {
        if want < a.rate_min {
            return a.rate_min;
        }
        if a.rate_max != 0 && want > a.rate_max {
            return a.rate_max;
        }
        return want;
    }
    let mut best = a.rates[0];
    for &r in &a.rates {
        if r == want {
            return want;
        }
        if abs_diff(r, want) < abs_diff(best, want) {
            best = r;
        }
    }
    best
}

/// Best usable alt setting: 16-bit PCM, mono or stereo; prefers the wanted rate / channels.
pub fn choose_audio(alts: &[AudioAlt], want_rate: u32, want_ch: u16) -> Option<&AudioAlt> {
    let mut best: Option<(&AudioAlt, u32)> = None;
    for a in alts {
        if !a.pcm
            || a.subframe != 2
            || a.bits != 16
            || a.channels < 1
            || a.channels > 2
            || a.max_packet == 0
        {
            continue;
        }
        let mut score = 1u32;
        if pick_rate(a, want_rate) == want_rate {
            score += 4;
        }
        if a.channels as u16 == want_ch {
            score += 2;
        }
        let better = match best {
            Some((_, s)) => score > s,
            None => true,
        };
        if better {
            best = Some((a, score));
        }
    }
    best.map(|(a, _)| a)
}

#[cfg(test)]
#[path = "../tests/unit/descriptors_tests.rs"]
mod tests;
