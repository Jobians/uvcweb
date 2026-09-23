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
4. **Open viewer** shows the web page full screen inside the app (rotate, landscape and hide buttons work
   there too). Other apps can use the URLs shown under the status line, e.g. VLC on `rtsp://127.0.0.1:8554/live`.
5. "Allow other devices on the network" makes the servers reachable from your LAN (no password),
   and also advertises them over mDNS/Bonjour under the name shown in that same field (default
   "uvcweb", editable). Other devices can find it by name instead of typing the IP address - for
   example VLC's "Local Network" browser, or any mDNS/Bonjour browser app. Discovery is Android-app
   only; the Termux program has no mDNS support.

The log at the bottom is the same log as the Termux program prints.

## If something does not work

Send me the first error you get, plus the log shown in the app. Where trouble is most likely:

* `build-native-deps.sh` compile errors: libusb / libuvc versions. `LIBUSB_REF` / `LIBUVC_REF` select the tags.
* `build-rust.sh` linker errors mentioning `libusb_*` or `uvc_*`: the static archive was not found or is for
  another CPU type. `cargo` should print `-L .../native-deps/<abi>/lib`.
* App: "could not open the capture card": permission dialog refused, or the card was unplugged.
* Log says `USB audio: cannot claim interface ... BUSY`: Android's own USB audio driver holds the audio
  interface; the video will still work.
* Adjust the Gradle / Android Gradle Plugin versions in `build.gradle.kts` if your Android Studio asks you to.

## Files

    build-native-deps.sh   libusb + libuvc -> libuvcusb.a (one static archive per CPU type)
    build-rust.sh          cargo-ndk build of ../ (the Rust crate) into app/src/main/jniLibs
    app/src/main/java/com/uvcweb/app/
        Native.kt          the JNI functions (must match ../src/android.rs)
        CaptureService.kt  foreground service: owns the USB connection and the Rust engine
        MainActivity.kt    settings, permissions, Start/Stop, status and log
        ViewerActivity.kt  the web viewer page in a full-screen WebView
        Settings.kt, Util.kt
