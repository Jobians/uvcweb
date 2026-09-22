[![CI](https://github.com/jobians/uvcweb/actions/workflows/ci.yml/badge.svg)](https://github.com/jobians/uvcweb/actions/workflows/ci.yml) [![Android](https://github.com/jobians/uvcweb/actions/workflows/android.yml/badge.svg)](https://github.com/jobians/uvcweb/actions/workflows/android.yml)

# uvcweb (Rust)

UVC MJPEG capture card -> web browser and/or RTSP, with live audio read straight
from the card's own USB audio interface. No ALSA, no root, **no external crates**
(std + the system `libuvc` / `libusb` only).

One source tree, two products:

* the **`uvcweb` program** for Termux / Linux (this file), and
* a **library for Android** (`libuvcweb_core.so`) with a ready-made Kotlin app: see `android/README.md`.

Both run the same engine (`src/engine.rs`); the program passes it command line options, the app passes the
same settings over JNI (`src/android.rs`).

## Releases

Pushing a tag like `v0.3.1` builds and publishes a GitHub Release with the Android APK, standalone
`uvcweb` binaries for Termux (arm64-v8a, armeabi-v7a, x86_64 - no libusb/libuvc build needed), and a
Linux binary. See `.github/workflows/release.yml`.

## Build (Termux)

    pkg install rust clang libuvc libusb
    cargo build --release          # links the libusb/libuvc you just installed; do not add --features android-static, that's for the Android build only (see android/README.md)
    cargo test              # optional: 24 tests, none need USB hardware

Needs Rust 1.70 or newer.

## Run

    termux-usb -r -e "./target/release/uvcweb -w 640 -h 480 -f 30" /dev/bus/usb/001/002              # web viewer (like the C version)
    termux-usb -r -e "./target/release/uvcweb -w 640 -h 480 -f 30 -P rtsp" /dev/bus/usb/001/002      # RTSP only
    termux-usb -r -e "./target/release/uvcweb -w 640 -h 480 -f 30 -P web,rtsp -l" /dev/bus/usb/001/002   # both, reachable from the LAN

Old command lines keep working (`-a usb`, `-p 8081`, ...). `--help` lists everything.

| option | meaning |
|---|---|
| `-w W -h H -f FPS` | MJPEG mode (default: the card's own default) |
| `-a usb` / `-a off` | audio from the card (default) / no audio |
| `-ar RATE -ac CH` | preferred audio format; the card's own values win |
| `-P web,rtsp` | which protocols to serve (default `web`) |
| `-p [NAME=]PORT` | port; `web` = 8080, `rtsp` = 8554. With several protocols use `-p rtsp=9000` |
| `-l` | listen on the LAN too (no password!) |
| `-o MS` | RTSP: shift video timestamps by MS milliseconds (+ = video later) if lip-sync is off |

## Watching

* Web: `http://127.0.0.1:8080`. The first tap on the page starts the audio.
  * Tap the picture to hide / show the info bar. **Full screen** hides it automatically.
  * **Landscape** goes full screen and locks the phone to landscape (where the browser allows it).
  * **Rotate** turns the picture 90 degrees each press, for phones with auto-rotate switched off.
  * Keyboard: `f` full screen, `h` hide/show the bar, `r` rotate. Rotate and hide are remembered.
  * `/?lead=60` makes the audio arrive earlier (milliseconds of buffer; default 100).
* RTSP: `rtsp://127.0.0.1:8554/live`
  * VLC (Android / desktop): *Open network stream*.
  * `ffplay -rtsp_transport tcp rtsp://PHONE_IP:8554/live` (needs `-l`); use `udp` instead of `tcp` to test UDP.
  * `ffmpeg -rtsp_transport tcp -i rtsp://PHONE_IP:8554/live -c copy out.mkv`

RTSP video is RTP/JPEG (RFC 2435, baseline 4:2:2 or 4:2:0 up to 2040 px wide/high) and
audio is RTP L16 (uncompressed PCM). Video and audio carry RTCP sender reports so players sync them.
If the card's JPEGs can't be sent this way you get a clear log line instead of garbage.

## Adding a protocol

1. `src/protocols/foo.rs`: implement `Protocol` (`start` binds, spawns threads, returns) and `pub fn create()`.
2. `pub mod foo;` in `src/protocols/mod.rs`.
3. One line in `REGISTRY` there.

`-P foo`, `-p foo=PORT` and `--help` pick it up automatically. Subscribe to the data through
`ctx.hub.next_frame(..)` (JPEG pictures) and `ctx.hub.next_chunk(..)` (S16LE PCM); `web.rs` is the simplest example,
`rtsp.rs` + `rtp.rs` show an RTP based one.

## Layout

    src/main.rs          the program: command line + Ctrl-C, then Engine::start
    src/lib.rs           the library root (uvcweb_core)
    src/engine.rs        start / stop / supervise one session (status log, camera watchdog)
    src/android.rs       JNI entry points (Android only)
    android/             Kotlin app + build scripts
    src/capture.rs       libuvc: open, pick mode, stream, restart
    src/usbaudio.rs      libusb isochronous audio capture
    src/descriptors.rs   USB descriptor parsing (pure, unit tested)
    src/hub.rs           latest picture + audio queue; publish/subscribe
    src/protocols/       web.rs, rtsp.rs, mod.rs (plug-in point)
    src/jpeg.rs, rtp.rs  RTP/JPEG, L16, RTCP building blocks
    tests/unit/          unit tests, one file per module (e.g. hub.rs -> tests/unit/hub_tests.rs),
                         wired in with a one-line `#[path = "..."] mod tests;` so private items
                         stay reachable but the source files themselves stay free of test code
    tests/unit/golden_tests.rs   byte-exact vectors generated from reference/ (Python)
    reference/           the Python prototype that was tested against ffmpeg
