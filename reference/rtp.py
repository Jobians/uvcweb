"""Reference implementation of the RTP/JPEG (RFC 2435), L16 (RFC 3551) and RTCP SR logic.
The Rust port (src/protocols/rtp.rs, src/jpeg.rs) mirrors this line for line."""
import json, struct
STD = {k: v for k, v in json.load(open("std_huff.json")).items()}
STD_BY_ID = {(0,0): STD["DC_LUM"], (0,1): STD["DC_CHROMA"], (1,0): STD["AC_LUM"], (1,1): STD["AC_CHROMA"]}

def analyze_jpeg(d):
    if len(d) < 4 or d[0] != 0xFF or d[1] != 0xD8: raise ValueError("not a JPEG")
    qt = {}; comps = []; w = h = 0; dri = 0; custom = False; sof = None
    pos = 2; scan_start = None
    while pos + 4 <= len(d):
        if d[pos] != 0xFF: raise ValueError("bad marker at %d" % pos)
        m = d[pos+1]
        if m == 0xFF: pos += 1; continue
        if m == 0xD8 or m == 0x01 or 0xD0 <= m <= 0xD7: pos += 2; continue
        ln = (d[pos+2] << 8) | d[pos+3]
        if ln < 2 or pos + 2 + ln > len(d): raise ValueError("truncated segment")
        seg = d[pos+4:pos+2+ln]
        if m == 0xDB:
            o = 0
            while o < len(seg):
                pq, tq = seg[o] >> 4, seg[o] & 15; o += 1
                if pq != 0: raise ValueError("16-bit quant tables unsupported")
                if o + 64 > len(seg) or tq > 3: raise ValueError("bad DQT")
                qt[tq] = bytes(seg[o:o+64]); o += 64
        elif m == 0xC0:
            if len(seg) < 6 or seg[0] != 8: raise ValueError("not 8-bit baseline")
            h = (seg[1] << 8) | seg[2]; w = (seg[3] << 8) | seg[4]; n = seg[5]
            if n != 3 or len(seg) < 6 + 3*n: raise ValueError("need 3 components")
            comps = [(seg[6+3*i], seg[7+3*i], seg[8+3*i]) for i in range(3)]
            sof = m
        elif m in (0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB):
            raise ValueError("only baseline JPEG can go over RTP/JPEG")
        elif m == 0xC4:
            o = 0
            while o + 17 <= len(seg):
                tc, th = seg[o] >> 4, seg[o] & 15
                bits = list(seg[o+1:o+17]); n = sum(bits); vals = list(seg[o+17:o+17+n]); o += 17 + n
                std = STD_BY_ID.get((tc, th))
                if std is None or std[0] != bits or std[1] != vals: custom = True
        elif m == 0xDD:
            dri = (seg[0] << 8) | seg[1]
        elif m == 0xDA:
            scan_start = pos + 2 + ln; break
        pos += 2 + ln
    if sof is None or scan_start is None: raise ValueError("no SOF/SOS")
    hv = [c[1] for c in comps]
    if hv[1] != 0x11 or hv[2] != 0x11: raise ValueError("chroma must be 1x1")
    if hv[0] == 0x21: typ = 0
    elif hv[0] == 0x22: typ = 1
    else: raise ValueError("unsupported sampling %02x" % hv[0])
    tq0, tq1 = comps[0][2], comps[1][2]
    if comps[2][2] != tq1: raise ValueError("Cb/Cr use different quant tables")
    if tq0 not in qt or tq1 not in qt: raise ValueError("missing quant table")
    if w == 0 or h == 0 or (w + 7) // 8 > 255 or (h + 7) // 8 > 255: raise ValueError("size unsupported")
    end = len(d)
    while end >= 2 and not (d[end-2] == 0xFF and d[end-1] == 0xD9): end -= 1
    scan_end = end - 2 if end >= 2 else len(d)
    if scan_end <= scan_start: raise ValueError("empty scan")
    return dict(width=w, height=h, typ=typ + (64 if dri else 0), qtables=qt[tq0] + qt[tq1], dri=dri,
                scan_start=scan_start, scan_end=scan_end, custom_huffman=custom)

def rtp_header(marker, pt, seq, ts, ssrc):
    return struct.pack(">BBHII", 0x80, (0x80 if marker else 0) | pt, seq & 0xFFFF, ts & 0xFFFFFFFF, ssrc)

def packetize_jpeg(info, data, ts, ssrc, seq, max_payload=1400):
    scan = data[info["scan_start"]:info["scan_end"]]
    pkts = []; off = 0
    while True:
        first = off == 0
        hdr = bytes([0, (off >> 16) & 0xFF, (off >> 8) & 0xFF, off & 0xFF, info["typ"], 255,
                     (info["width"] + 7) // 8, (info["height"] + 7) // 8])
        if info["typ"] >= 64: hdr += struct.pack(">HH", info["dri"], 0xFFFF)
        if first: hdr += bytes([0, 0]) + struct.pack(">H", len(info["qtables"])) + info["qtables"]
        room = max_payload - len(hdr)
        chunk = scan[off:off+room]; off += len(chunk)
        last = off >= len(scan)
        pkts.append(rtp_header(last, 26, seq, ts, ssrc) + hdr + chunk); seq = (seq + 1) & 0xFFFF
        if last: break
    return pkts, seq

def packetize_l16(pcm_le, chans, pos_frames, ssrc, seq, base_ts, pos0, max_payload=1200):
    """pcm_le: little-endian S16 interleaved. RTP timestamp = base_ts + (pos - pos0) in sample frames."""
    fb = 2 * chans; per = max(1, max_payload // fb); pkts = []
    n_frames = len(pcm_le) // fb; i = 0
    while i < n_frames:
        k = min(per, n_frames - i)
        raw = pcm_le[i*fb:(i+k)*fb]
        be = bytearray(raw); be[0::2], be[1::2] = raw[1::2], raw[0::2]
        pkts.append(rtp_header(False, 97, seq, (base_ts + (pos_frames - pos0) + i) & 0xFFFFFFFF, ssrc) + bytes(be))
        seq = (seq + 1) & 0xFFFF; i += k
    return pkts, seq

def ntp_from_unix_micros(us):
    sec = us // 1_000_000 + 2208988800; frac = ((us % 1_000_000) << 32) // 1_000_000
    return sec & 0xFFFFFFFF, frac & 0xFFFFFFFF

def sender_report(ssrc, unix_us, rtp_ts, pkt_count, octet_count, cname=b"uvcweb"):
    sec, frac = ntp_from_unix_micros(unix_us)
    sr = struct.pack(">BBHIIIIII", 0x80, 200, 6, ssrc, sec, frac, rtp_ts & 0xFFFFFFFF, pkt_count & 0xFFFFFFFF, octet_count & 0xFFFFFFFF)
    item = bytes([1, len(cname)]) + cname + b"\0"
    while (len(item) + 4) % 4: item += b"\0"          # SDES chunk (ssrc + items) padded to 32 bit
    sdes_len = (4 + 4 + len(item)) // 4 - 1
    sdes = struct.pack(">BBHI", 0x81, 202, sdes_len, ssrc) + item
    return sr + sdes
