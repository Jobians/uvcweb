"""Reference RTSP server (TCP-interleaved + UDP) mirroring src/protocols/rtsp.rs. Synthetic camera + sine audio."""
import io, math, os, random, socket, struct, sys, threading, time
import rtp
from PIL import Image, ImageDraw

RATE, CH = int(os.environ.get("RATE", 96000)), int(os.environ.get("CH", 1))
FPS = 15
FLAVOR = os.environ.get("JFLAVOR", "422")
AUDIO_DELAY_S = float(os.environ.get("AUDIO_DELAY", 0))   # test: audio arrives this much later than video
kw = dict(subsampling=1) if FLAVOR == "422" else dict(subsampling=2) if FLAVOR == "420" else dict(subsampling=1, restart_marker_rows=1)

def make_frames(n=30, w=640, h=480):
    out = []
    for i in range(n):
        im = Image.new("RGB", (w, h), (20 + i * 6 % 200, 60, 140)); d = ImageDraw.Draw(im)
        d.rectangle([20 + i * 15, 100, 120 + i * 15, 200], fill=(250, 250, 30)); d.text((30, 30), f"frame {i}", fill=(255, 255, 255))
        b = io.BytesIO(); im.save(b, "JPEG", quality=85, **kw); out.append(b.getvalue())
    return out
FRAMES = make_frames()

class Hub:
    def __init__(self): self.lock = threading.Condition(); self.vseq = 0; self.vframe = None; self.vat = 0; self.achunks = []; self.anext = 0; self.apos = 0
    def push_video(self, d):
        with self.lock: self.vseq += 1; self.vframe = (self.vseq, d, time.monotonic()); self.lock.notify_all()
    def push_audio(self, pcm):
        with self.lock:
            self.achunks.append((self.anext, self.apos, pcm, time.monotonic())); self.anext += 1; self.apos += len(pcm) // (2 * CH)
            self.achunks = self.achunks[-200:]; self.lock.notify_all()
HUB = Hub()

def feeder():
    t0 = time.monotonic(); i = 0
    while True:
        HUB.push_video(FRAMES[i % len(FRAMES)]); i += 1
        time.sleep(max(0, t0 + i / FPS - time.monotonic()))
def audio_feeder():
    time.sleep(AUDIO_DELAY_S)
    t0 = time.monotonic(); n = 0; step = RATE // 100
    while True:
        s = [int(12000 * math.sin(2 * math.pi * 440 * (n + k) / RATE)) for k in range(step)]
        pcm = b"".join(struct.pack("<h", v) * CH for v in s); HUB.push_audio(pcm); n += step
        time.sleep(max(0, t0 + n / RATE - time.monotonic()))

def next_video(last, timeout=0.5):
    with HUB.lock:
        end = time.monotonic() + timeout
        while HUB.vframe is None or HUB.vframe[0] <= last:
            r = end - time.monotonic()
            if r <= 0: return None
            HUB.lock.wait(r)
        return HUB.vframe
def next_audio(nxt, timeout=0.5):
    with HUB.lock:
        end = time.monotonic() + timeout
        while True:
            if HUB.achunks:
                if nxt < HUB.achunks[0][0]: nxt = HUB.achunks[0][0]
                if nxt < HUB.anext: return HUB.achunks[nxt - HUB.achunks[0][0]], nxt + 1
            r = end - time.monotonic()
            if r <= 0: return None, nxt
            HUB.lock.wait(r)

class Sink:
    def __init__(self, tcp, wr=None, rtp_ch=0, rtcp_ch=1, usock=None, csock=None, peer=None, peer_rtcp=None):
        self.tcp, self.wr, self.rtp_ch, self.rtcp_ch, self.usock, self.csock, self.peer, self.peer_rtcp = tcp, wr, rtp_ch, rtcp_ch, usock, csock, peer, peer_rtcp
    def _tcp(self, ch, pkt):
        with self.wr[1]: self.wr[0].sendall(b"$" + bytes([ch]) + struct.pack(">H", len(pkt)) + pkt)
    def send_rtp(self, pkt): self._tcp(self.rtp_ch, pkt) if self.tcp else self.usock.sendto(pkt, self.peer)
    def send_rtcp(self, pkt): self._tcp(self.rtcp_ch, pkt) if self.tcp else self.csock.sendto(pkt, self.peer_rtcp)

def unix_us_of(anchor_wall_us, anchor_inst, inst): return anchor_wall_us + int((inst - anchor_inst) * 1e6)

def video_sender(sink, alive, anchor, av_off_us):
    aw, ai = anchor; ssrc = random.getrandbits(32); seq = random.getrandbits(16); base = random.getrandbits(32)
    last = 0; pk = ob = 0; next_sr = 0; warned = False
    while alive[0]:
        f = next_video(last)
        now = time.monotonic()
        if f is not None:
            last, data, at = f
            try: info = rtp.analyze_jpeg(data)
            except ValueError as e: print("video frame skipped:", e); continue
            ts = (base + int((at - ai) * 90000)) & 0xFFFFFFFF
            pkts, seq = rtp.packetize_jpeg(info, data, ts, ssrc, seq)
            try:
                if pk == 0: sink.send_rtcp(rtp.sender_report(ssrc, unix_us_of(aw, ai, at) + av_off_us, ts, pk, ob))
                for p in pkts: sink.send_rtp(p)
            except OSError: return
            pk += len(pkts); ob += sum(len(p) - 12 for p in pkts)
        if now >= next_sr and pk:
            ts = (base + int((now - ai) * 90000)) & 0xFFFFFFFF
            try: sink.send_rtcp(rtp.sender_report(ssrc, unix_us_of(aw, ai, now) + av_off_us, ts, pk, ob))
            except OSError: return
            next_sr = now + 2

def audio_sender(sink, alive, anchor):
    aw, ai = anchor; ssrc = random.getrandbits(32); seq = random.getrandbits(16); base = random.getrandbits(32)
    nxt = HUB.anext; pos0 = None; pk = ob = 0; next_sr = 0; last_pair = None
    while alive[0]:
        c, nxt = next_audio(nxt)
        if c is not None:
            _, pos, pcm, at = c
            if pos0 is None: pos0 = pos
            pkts, seq = rtp.packetize_l16(pcm, CH, pos, ssrc, seq, base, pos0)
            try:
                for p in pkts: sink.send_rtp(p)
            except OSError: return
            pk += len(pkts); ob += sum(len(p) - 12 for p in pkts)
            end_rtp = (base + (pos - pos0) + len(pcm) // (2 * CH)) & 0xFFFFFFFF
            last_pair = (unix_us_of(aw, ai, at), end_rtp)
            if pk == len(pkts):     # very first chunk: send SR right away
                try: sink.send_rtcp(rtp.sender_report(ssrc, last_pair[0], last_pair[1], pk, ob))
                except OSError: return
        now = time.monotonic()
        if last_pair and now >= next_sr:
            try: sink.send_rtcp(rtp.sender_report(ssrc, last_pair[0], last_pair[1], pk, ob))
            except OSError: return
            next_sr = now + 2

def sdp(host):
    s = ["v=0", "o=- 1 1 IN IP4 0.0.0.0", "s=uvcweb", "c=IN IP4 0.0.0.0", "t=0 0", "a=control:*", "a=range:npt=0-",
         "m=video 0 RTP/AVP 26", "a=rtpmap:26 JPEG/90000", "a=control:trackID=0",
         "m=audio 0 RTP/AVP 97", f"a=rtpmap:97 L16/{RATE}/{CH}", "a=control:trackID=1"]
    return "\r\n".join(s) + "\r\n"

def handle(conn, addr):
    rf = conn.makefile("rb"); wr = (conn, threading.Lock()); sid = "%08X" % random.getrandbits(32)
    tracks = {}; alive = [True]; playing = False
    def respond(code, reason, cseq, extra=(), body=b""):
        h = f"RTSP/1.0 {code} {reason}\r\nCSeq: {cseq}\r\nServer: uvcweb\r\n" + "".join(f"{k}: {v}\r\n" for k, v in extra) + f"Content-Length: {len(body)}\r\n\r\n"
        with wr[1]: conn.sendall(h.encode() + body)
    conn.settimeout(65)
    try:
        while True:
            b = rf.peek(1)[:1]
            if not b: break
            if b == b"$":
                hd = rf.read(4); rf.read(struct.unpack(">H", hd[2:4])[0]); continue
            line = rf.readline().decode().strip()
            if not line: continue
            method, url, _ = line.split(" ", 2); hdrs = {}
            while True:
                l = rf.readline().decode().strip()
                if not l: break
                k, v = l.split(":", 1); hdrs[k.strip().lower()] = v.strip()
            if "content-length" in hdrs: rf.read(int(hdrs["content-length"]))
            cseq = hdrs.get("cseq", "0")
            if method == "OPTIONS":
                respond(200, "OK", cseq, [("Public", "OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN, GET_PARAMETER, SET_PARAMETER")])
            elif method == "DESCRIBE":
                body = sdp(url).encode()
                respond(200, "OK", cseq, [("Content-Base", url.rstrip("/") + "/"), ("Content-Type", "application/sdp")], body)
            elif method == "SETUP":
                tid = int(url.rsplit("trackID=", 1)[1]) if "trackID=" in url else 0
                chosen = None
                for spec in hdrs.get("transport", "").split(","):
                    toks = [t.strip() for t in spec.split(";")]
                    if "multicast" in toks: continue
                    if toks[0] == "RTP/AVP/TCP":
                        il = next((t for t in toks if t.startswith("interleaved=")), "interleaved=0-1")[12:].split("-")
                        chosen = ("tcp", int(il[0]), int(il[1])); break
                    if toks[0] in ("RTP/AVP", "RTP/AVP/UDP"):
                        cp = next((t for t in toks if t.startswith("client_port=")), None)
                        if cp: a, b2 = cp[12:].split("-"); chosen = ("udp", int(a), int(b2)); break
                if not chosen: respond(461, "Unsupported Transport", cseq); continue
                if chosen[0] == "tcp":
                    tracks[tid] = Sink(True, wr, chosen[1], chosen[2])
                    tr = f"RTP/AVP/TCP;unicast;interleaved={chosen[1]}-{chosen[2]}"
                else:
                    while True:
                        p = random.randrange(20000, 40000) & ~1
                        try:
                            us = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); us.bind(("0.0.0.0", p))
                            cs = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); cs.bind(("0.0.0.0", p + 1)); break
                        except OSError: continue
                    ip = addr[0]
                    tracks[tid] = Sink(False, usock=us, csock=cs, peer=(ip, chosen[1]), peer_rtcp=(ip, chosen[2]))
                    tr = f"RTP/AVP;unicast;client_port={chosen[1]}-{chosen[2]};server_port={p}-{p+1}"
                respond(200, "OK", cseq, [("Transport", tr), ("Session", sid + ";timeout=60")])
            elif method == "PLAY":
                if not tracks: respond(455, "Method Not Valid in This State", cseq); continue
                if not playing:
                    playing = True; anchor = (int(time.time() * 1e6), time.monotonic())
                    for tid, sink in tracks.items():
                        threading.Thread(target=video_sender if tid == 0 else audio_sender, daemon=True,
                                         args=(sink, alive, anchor) + ((0,) if tid == 0 else ())).start()
                respond(200, "OK", cseq, [("Range", "npt=0.000-"), ("Session", sid)])
            elif method in ("GET_PARAMETER", "SET_PARAMETER"):
                respond(200, "OK", cseq, [("Session", sid)])
            elif method == "TEARDOWN":
                respond(200, "OK", cseq, [("Session", sid)]); break
            else:
                respond(501, "Not Implemented", cseq)
    except (OSError, ValueError) as e: pass
    finally: alive[0] = False; conn.close()

if __name__ == "__main__":
    port = int(sys.argv[1]); threading.Thread(target=feeder, daemon=True).start(); threading.Thread(target=audio_feeder, daemon=True).start()
    srv = socket.socket(); srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); srv.bind(("127.0.0.1", port)); srv.listen(8)
    print("listening", port, flush=True)
    while True:
        c, a = srv.accept(); threading.Thread(target=handle, args=(c, a), daemon=True).start()
