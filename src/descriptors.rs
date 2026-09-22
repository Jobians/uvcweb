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
                        default_idx = d[6];
                    }
                }
                0x04 | 0x10 => in_mjpeg = false, // uncompressed / frame based formats
                0x07 => {
                    // VS_FRAME_MJPEG
                    if in_mjpeg && d.len() >= 25 {
                        modes.push(MjpegMode {
                            width: u16le(&d[5..7]),
                            height: u16le(&d[7..9]),
                            interval: u32le(&d[21..25]),
                            is_default: d[3] == default_idx,
                        });
                    }
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
    let fps = if m.interval > 0 {
        10_000_000 / m.interval
    } else {
        30
    };
    Some((m.width, m.height, fps))
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
    if a > b {
        a - b
    } else {
        b - a
    }
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
