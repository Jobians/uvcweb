# uvcweb for Android

The same Rust code that runs as the `uvcweb` program in Termux, packaged as a library inside a small
Kotlin app. The app opens the USB capture card with Android's USB host API, hands the file descriptor to
the library (exactly what `termux-usb -e` does), and runs the camera, the audio and the **web** and/or
**RTSP** servers inside a foreground service.

    Kotlin app  --JNI-->  libuvcweb_core.so (Rust)  --> libuvc + libusb (C, built statically into it)

Everything is written without libraries: the app uses only Android framework classes, the Rust side only std.

## Vendoring libusb / libuvc (optional but recommended)

`build-native-deps.sh` needs the libusb and libuvc source. By default it clones them itself into a
scratch folder, which needs `git` and network access every time you wipe that folder. To avoid that,
clone them yourself, once, into these exact paths **at the repository root** (not under `android/`:
these two C libraries aren't Android-specific in themselves, this build just happens to be their only
consumer today):

    git clone --branch v1.0.30 https://github.com/libusb/libusb.git third_party/libusb
    git clone --branch v0.0.8  https://github.com/libuvc/libuvc.git third_party/libuvc

`build-native-deps.sh` checks for `third_party/libusb` and `third_party/libuvc` first and uses them
as-is if present (no cloning, no network access at all). `third_party/` is in `.gitignore` at the repo
root, since these are large third-party trees; remove those two lines there if you'd rather commit
them for fully offline, reproducible builds. An explicit `LIBUSB_DIR=` / `LIBUVC_DIR=` environment
variable still wins over both, if you keep the sources somewhere else entirely.

## What you need

* Android Studio (any recent one), with the **NDK** installed (SDK Manager > SDK Tools > NDK)
* Rust via rustup, then:

      rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
      cargo install cargo-ndk

* `git` (the script downloads libusb and libuvc)
* Linux, macOS or Windows WSL for the two build scripts

## Build

    export ANDROID_NDK_HOME=~/Android/Sdk/ndk/<version>     # your NDK folder

    cd android
    ./build-native-deps.sh    # libusb + libuvc for Android  -> native-deps/<abi>/lib/libuvcusb.a
    ./build-rust.sh           # the Rust library             -> app/src/main/jniLibs/<abi>/libuvcweb_core.so

Then open the `android/` folder in Android Studio and press Run (a real phone; an emulator has no USB host).
`ABIS="arm64-v8a"` before either script builds just one CPU type (faster; most phones are arm64-v8a).

## Use

1. Plug the capture card into the phone (USB-C / OTG adapter) and open the app.
2. Choose Web viewer and/or RTSP, ports, and the video mode (0 x 0 = the card's default).
3. **Start.** Android asks for Camera and Microphone permission (it insists on them for USB video and
   audio devices even though the phone's own camera and mic are never used), then for USB access to the card.
4. **Record** records the picture and sound into an `.mp4` file (see below). **Open viewer** shows the web
   page full screen inside the app (rotate, landscape, record and hide buttons work there too). Other apps can use the URLs shown under the status line, e.g. VLC on `rtsp://127.0.0.1:8554/live`.
5. "Allow other devices on the network" makes the servers reachable from your LAN (no password),
   and also advertises them over mDNS/Bonjour under the name shown in that same field (default
   "uvcweb", editable). Other devices can find it by name instead of typing the IP address - for
   example VLC's "Local Network" browser, or any mDNS/Bonjour browser app. Discovery is Android-app
   only; the Termux program has no mDNS support.

The log at the bottom is the same log as the Termux program prints.

### Recording

**Record** writes an **`.mp4`**: H.264 video and AAC sound, made with the phone's own
hardware encoder. A file that way is roughly 5-10 times smaller than the card's own JPEGs
and plays in any browser, VLC or phone gallery, and it can be shared as it is.

* The card sends JPEG, not the YUV an encoder wants, so the phone decodes each picture
  and converts it. That is the one part that costs real time. A picture that is too late to
  be encoded in time is skipped, and the log says how many were; the ones that got in keep
  the timestamps the card gave them, so the file plays at the rate the phone managed and the
  sound stays in step. Expect a dropped picture now and then on a slow phone, none at all on
  a fast one.
* Video is capped at 960 pixels wide. Anything larger is scaled down first, which keeps a
  4K stream from costing a whole core per picture - the encoder would scale it down anyway.
* If the phone has no encoder that will start, the app says so and records an `.avi` of the
  card's own pictures instead (the same recorder the viewer page and the Termux program
  use), so you always get a file.
* Files land in the app's own storage: `Android/data/com.uvcweb.app/files/record/` (or the
  internal files folder if the device has no external storage). A file manager can open
  that folder; the app has no screen for playing recordings back. A file is written under a
  `.part` name and only renamed to `.mp4` once it is complete, so a recording cut short by
  a crash or a pulled battery is never mistaken for one that plays.
* The button turns into **Stop recording** while one runs, and the status line shows how
  long it has been going, how many pictures and how many MB. Stopping the camera finishes
  the file by itself; the log then says what was written.
* The viewer page's **Record** button (and the Termux program) record the card's own
  pictures to `.avi` instead - bigger, but untouched, which is what you want when the card's
  own compression is the thing under test.

## If something does not work

Send me the first error you get, plus the log shown in the app. Where trouble is most likely:

* `build-native-deps.sh` compile errors: libusb / libuvc versions. `LIBUSB_REF` / `LIBUVC_REF` select the tags.
* `build-rust.sh` linker errors mentioning `libusb_*` or `uvc_*`: the static archive was not found or is for
  another CPU type. `cargo` should print `-L .../native-deps/<abi>/lib`.
* App: "could not open the capture card": permission dialog refused, or the card was unplugged.
* App: "the record folder could not be used - see the log": the phone's storage is full, or the app's
  external folder is gone (moved to another card). The log line names the folder that failed.
* App: "no H.264 encoder here ... writing AVI instead": the phone has no encoder that will take
  pictures in the formats Android offers. The AVI is bigger, but the recording works.
* Log says `the encoders did not finish in time`: the phone was too busy to flush; the `.part`
  file is left where it is and the recording is not usable.
* Log says `USB audio: cannot claim interface ... BUSY`: Android's own USB audio driver holds the audio
  interface; the video will still work.
* Adjust the Gradle / Android Gradle Plugin versions in `build.gradle.kts` if your Android Studio asks you to.

## Files

    build-native-deps.sh   libusb + libuvc -> libuvcusb.a (one static archive per CPU type)
    build-rust.sh          cargo-ndk build of ../ (the Rust crate) into app/src/main/jniLibs
    app/src/main/java/com/uvcweb/app/
        Native.kt          the JNI functions (must match ../src/android.rs)
        CaptureService.kt  foreground service: owns the USB connection and the Rust engine
        H264Recorder.kt    MP4 recording: JPEG -> YUV -> H.264, sound -> AAC, muxed by MediaMuxer
        MainActivity.kt    settings, permissions, Start/Stop, Record, status and log
        ViewerActivity.kt  the web viewer page in a full-screen WebView
        Settings.kt, Util.kt
