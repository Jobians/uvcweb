//! RTP / RTCP packet building shared by every RTP-based protocol (RTSP today).
//! Byte-for-byte port of the ffmpeg-verified Python reference (proto/rtp.py);
//! `golden_tests.rs` checks that they agree.

use crate::jpeg::JpegInfo;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const PT_JPEG: u8 = 26; // static payload type for RTP/JPEG
pub const PT_L16: u8 = 97; // dynamic payload type used for 16-bit PCM

static RNG: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);

/// Good-enough random numbers for SSRCs, sequence numbers and session ids (no rand crate).
pub fn rand_u32() -> u32 {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let mut x = RNG.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed) ^ t;
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    x as u32
}

pub fn rtp_header(out: &mut Vec<u8>, marker: bool, pt: u8, seq: u16, ts: u32, ssrc: u32) {
    out.push(0x80); // version 2, no padding, no extension, no CSRC
    out.push(if marker { 0x80 | pt } else { pt });
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&ts.to_be_bytes());
    out.extend_from_slice(&ssrc.to_be_bytes());
}

/// Split one JPEG picture into RFC 2435 packets. `max_payload` is the RTP payload budget
/// (JPEG headers included). Quantisation tables travel in the first packet of every frame (Q=255).
pub fn packetize_jpeg(
    info: &JpegInfo,
    data: &[u8],
    ts: u32,
    ssrc: u32,
    seq: &mut u16,
    max_payload: usize,
) -> Vec<Vec<u8>> {
    let scan = &data[info.scan_start..info.scan_end];
    let w8 = (info.width as usize).div_ceil(8) as u8;
    let h8 = (info.height as usize).div_ceil(8) as u8;
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut off = 0usize;
    loop {
        let mut hdr: Vec<u8> = vec![
            0,
            (off >> 16) as u8,
            (off >> 8) as u8,
            off as u8,
            info.rtp_type,
            255,
            w8,
            h8,
        ];
        if info.rtp_type >= 64 {
            hdr.extend_from_slice(&info.dri.to_be_bytes());
            hdr.extend_from_slice(&[0xFF, 0xFF]); // F=1, L=1, count=0x3FFF: not aligned to restart intervals
        }
        if off == 0 {
            hdr.extend_from_slice(&[0, 0]); // MBZ, precision (all tables 8 bit)
            hdr.extend_from_slice(&(info.qtables.len() as u16).to_be_bytes());
            hdr.extend_from_slice(&info.qtables);
        }
        let room = max_payload.saturating_sub(hdr.len()).max(1);
        let end = (off + room).min(scan.len());
        let last = end >= scan.len();
        let mut p: Vec<u8> = Vec::with_capacity(12 + hdr.len() + (end - off));
        rtp_header(&mut p, last, PT_JPEG, *seq, ts, ssrc);
        *seq = seq.wrapping_add(1);
        p.extend_from_slice(&hdr);
        p.extend_from_slice(&scan[off..end]);
        out.push(p);
        off = end;
        if last {
            break;
        }
    }
    out
}

/// Split interleaved little-endian S16 PCM into RTP L16 packets (network byte order, RFC 3551).
/// `pos` / `pos0` are running sample-frame counters; the RTP timestamp is `base_ts + (pos - pos0)`.
#[allow(clippy::too_many_arguments)]
pub fn packetize_l16(
    pcm_le: &[u8],
    chans: usize,
    pos: u64,
    pos0: u64,
    base_ts: u32,
    ssrc: u32,
    seq: &mut u16,
    max_payload: usize,
) -> Vec<Vec<u8>> {
    let fb = 2 * chans.max(1);
    let per = (max_payload / fb).max(1);
    let n_frames = pcm_le.len() / fb;
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut i = 0usize;
    while i < n_frames {
        let k = per.min(n_frames - i);
        let ts = base_ts.wrapping_add((pos - pos0 + i as u64) as u32);
        let mut p: Vec<u8> = Vec::with_capacity(12 + k * fb);
        rtp_header(&mut p, false, PT_L16, *seq, ts, ssrc);
        *seq = seq.wrapping_add(1);
        for s in pcm_le[i * fb..(i + k) * fb].as_chunks::<2>().0 {
            p.push(s[1]); // swap to big endian
            p.push(s[0]);
        }
        out.push(p);
        i += k;
    }
    out
}

/// NTP timestamp (seconds since 1900, 32.32 fixed point) for a Unix time in microseconds.
pub fn ntp_from_unix_micros(us: i64) -> (u32, u32) {
    let sec = us.div_euclid(1_000_000) + 2_208_988_800;
    let rem = us.rem_euclid(1_000_000) as u64;
    let frac = (rem << 32) / 1_000_000;
    (sec as u32, frac as u32)
}

/// RTCP sender report followed by an SDES CNAME chunk (a valid compound packet).
/// It tells receivers which RTP timestamp corresponds to which wall-clock time, which is
/// how players line the audio and video streams up.
pub fn sender_report(
    ssrc: u32,
    unix_us: i64,
    rtp_ts: u32,
    pkt_count: u32,
    octet_count: u32,
    cname: &str,
) -> Vec<u8> {
    let (sec, frac) = ntp_from_unix_micros(unix_us);
    let mut p: Vec<u8> = Vec::with_capacity(48);
    p.extend_from_slice(&[0x80, 200, 0, 6]);
    p.extend_from_slice(&ssrc.to_be_bytes());
    p.extend_from_slice(&sec.to_be_bytes());
    p.extend_from_slice(&frac.to_be_bytes());
    p.extend_from_slice(&rtp_ts.to_be_bytes());
    p.extend_from_slice(&pkt_count.to_be_bytes());
    p.extend_from_slice(&octet_count.to_be_bytes());
    // SDES: one chunk (ssrc + CNAME item + terminator), padded to a 32-bit boundary
    let mut item: Vec<u8> = vec![1, cname.len() as u8];
    item.extend_from_slice(cname.as_bytes());
    item.push(0);
    while !(item.len() + 4).is_multiple_of(4) {
        item.push(0);
    }
    let words = (4 + 4 + item.len()) / 4 - 1;
    p.extend_from_slice(&[0x81, 202]);
    p.extend_from_slice(&(words as u16).to_be_bytes());
    p.extend_from_slice(&ssrc.to_be_bytes());
    p.extend_from_slice(&item);
    p
}
