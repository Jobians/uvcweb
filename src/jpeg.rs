//! Just enough JPEG parsing to send baseline MJPEG frames as RTP/JPEG (RFC 2435).
//! Mirrors `analyze_jpeg` of the ffmpeg-verified Python reference.

pub struct JpegInfo {
    #[allow(dead_code)]
    pub width: u16,
    pub height: u16,
    /// RFC 2435 type: 0 = 4:2:2, 1 = 4:2:0, +64 when the stream uses restart markers
    pub rtp_type: u8,
    /// luminance table then chrominance table, 64 bytes each, zig-zag order as stored in the file
    pub qtables: Vec<u8>,
    pub dri: u16,
    /// entropy-coded data: bytes after the SOS header, before the final EOI
    pub scan_start: usize,
    pub scan_end: usize,
    /// the frame carries Huffman tables that differ from the standard ones RTP/JPEG assumes
    pub custom_huffman: bool,
}

// Standard JPEG Huffman tables (ITU T.81 Annex K.3), extracted from a libjpeg-written file.
pub static DC_LUM_BITS: [u8; 16] = [
    0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
pub static DC_LUM_VALS: [u8; 12] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b,
];
pub static DC_CHROMA_BITS: [u8; 16] = [
    0x00, 0x03, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
];
pub static DC_CHROMA_VALS: [u8; 12] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b,
];
pub static AC_LUM_BITS: [u8; 16] = [
    0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05, 0x05, 0x04, 0x04, 0x00, 0x00, 0x01, 0x7d,
];
pub static AC_LUM_VALS: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];
pub static AC_CHROMA_BITS: [u8; 16] = [
    0x00, 0x02, 0x01, 0x02, 0x04, 0x04, 0x03, 0x04, 0x07, 0x05, 0x04, 0x04, 0x00, 0x01, 0x02, 0x77,
];
pub static AC_CHROMA_VALS: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0,
    0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

pub fn std_table(tc: u8, th: u8) -> Option<(&'static [u8], &'static [u8])> {
    match (tc, th) {
        (0, 0) => Some((&DC_LUM_BITS[..], &DC_LUM_VALS[..])),
        (0, 1) => Some((&DC_CHROMA_BITS[..], &DC_CHROMA_VALS[..])),
        (1, 0) => Some((&AC_LUM_BITS[..], &AC_LUM_VALS[..])),
        (1, 1) => Some((&AC_CHROMA_BITS[..], &AC_CHROMA_VALS[..])),
        _ => None,
    }
}

pub fn analyze(d: &[u8]) -> Result<JpegInfo, String> {
    if d.len() < 4 || d[0] != 0xFF || d[1] != 0xD8 {
        return Err("not a JPEG".to_string());
    }
    let mut qt: [Option<[u8; 64]>; 4] = [None; 4];
    let mut comps: Vec<(u8, u8, u8)> = Vec::new(); // (id, sampling h<<4|v, quant table id)
    let mut width = 0usize;
    let mut height = 0usize;
    let mut dri = 0u16;
    let mut custom = false;
    let mut have_sof = false;
    let mut scan_start = 0usize;
    let mut have_scan = false;
    let mut pos = 2usize;
    while pos + 4 <= d.len() {
        if d[pos] != 0xFF {
            return Err(format!("bad marker at {}", pos));
        }
        let m = d[pos + 1];
        if m == 0xFF {
            pos += 1;
            continue;
        }
        if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) {
            pos += 2;
            continue;
        }
        let ln = ((d[pos + 2] as usize) << 8) | (d[pos + 3] as usize);
        if ln < 2 || pos + 2 + ln > d.len() {
            return Err("truncated segment".to_string());
        }
        let seg = &d[pos + 4..pos + 2 + ln];
        match m {
            0xDB => {
                let mut o = 0usize;
                while o < seg.len() {
                    let pq = seg[o] >> 4;
                    let tq = (seg[o] & 15) as usize;
                    o += 1;
                    if pq != 0 {
                        return Err("16-bit quantisation tables are not supported".to_string());
                    }
                    if o + 64 > seg.len() || tq > 3 {
                        return Err("bad DQT".to_string());
                    }
                    let mut t = [0u8; 64];
                    t.copy_from_slice(&seg[o..o + 64]);
                    qt[tq] = Some(t);
                    o += 64;
                }
            }
            0xC0 => {
                if seg.len() < 6 || seg[0] != 8 {
                    return Err("not 8-bit baseline".to_string());
                }
                height = ((seg[1] as usize) << 8) | (seg[2] as usize);
                width = ((seg[3] as usize) << 8) | (seg[4] as usize);
                if seg[5] != 3 || seg.len() < 6 + 9 {
                    return Err("need 3 components".to_string());
                }
                comps.clear();
                for i in 0..3 {
                    comps.push((seg[6 + 3 * i], seg[7 + 3 * i], seg[8 + 3 * i]));
                }
                have_sof = true;
            }
            0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB => {
                return Err("only baseline JPEG can go over RTP/JPEG".to_string());
            }
            0xC4 => {
                let mut o = 0usize;
                while o + 17 <= seg.len() {
                    let tc = seg[o] >> 4;
                    let th = seg[o] & 15;
                    let bits = &seg[o + 1..o + 17];
                    let n: usize = bits.iter().map(|&b| b as usize).sum();
                    if o + 17 + n > seg.len() {
                        break;
                    }
                    let vals = &seg[o + 17..o + 17 + n];
                    let same = match std_table(tc, th) {
                        Some((sb, sv)) => sb == bits && sv == vals,
                        None => false,
                    };
                    if !same {
                        custom = true;
                    }
                    o += 17 + n;
                }
            }
            0xDD => {
                if seg.len() >= 2 {
                    dri = ((seg[0] as u16) << 8) | (seg[1] as u16);
                }
            }
            0xDA => {
                scan_start = pos + 2 + ln;
                have_scan = true;
                break;
            }
            _ => {}
        }
        pos += 2 + ln;
    }
    if !have_sof || !have_scan {
        return Err("no SOF/SOS".to_string());
    }
    if comps[1].1 != 0x11 || comps[2].1 != 0x11 {
        return Err("chroma components must be 1x1".to_string());
    }
    let base_type: u8 = match comps[0].1 {
        0x21 => 0,
        0x22 => 1,
        other => {
            return Err(format!(
                "unsupported sampling {:02x} (RTP/JPEG only carries 4:2:2 and 4:2:0)",
                other
            ))
        }
    };
    let tq0 = comps[0].2 as usize;
    let tq1 = comps[1].2 as usize;
    if comps[2].2 as usize != tq1 {
        return Err("Cb and Cr use different quantisation tables".to_string());
    }
    let (t0, t1) = match (
        qt.get(tq0).copied().flatten(),
        qt.get(tq1).copied().flatten(),
    ) {
        (Some(a), Some(b)) => (a, b),
        _ => return Err("missing quantisation table".to_string()),
    };
    if width == 0 || height == 0 || width.div_ceil(8) > 255 || height.div_ceil(8) > 255 {
        return Err(format!(
            "picture size {}x{} cannot be described by RTP/JPEG (max 2040)",
            width, height
        ));
    }
    let mut end = d.len();
    while end >= 2 && !(d[end - 2] == 0xFF && d[end - 1] == 0xD9) {
        end -= 1;
    }
    let scan_end = if end >= 2 { end - 2 } else { d.len() };
    if scan_end <= scan_start {
        return Err("empty scan".to_string());
    }
    let rtp_type: u8 = if dri > 0 { base_type + 64 } else { base_type };
    let mut qtables = Vec::with_capacity(128);
    qtables.extend_from_slice(&t0);
    qtables.extend_from_slice(&t1);
    Ok(JpegInfo {
        width: width as u16,
        height: height as u16,
        rtp_type,
        qtables,
        dri,
        scan_start,
        scan_end,
        custom_huffman: custom,
    })
}
