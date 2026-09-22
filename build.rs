// Only does something when the "android-static" feature is on (set by android/build-rust.sh, which
// also sets UVCWEB_NATIVE_DIR): tells the linker where android/build-native-deps.sh put
// libuvcusb.a (libusb + libuvc, one static archive per CPU type). A plain `cargo build` / `cargo
// run` - in Termux or on Linux - never sets this feature, so this file does nothing for them and
// they link the system's shared libusb/libuvc as usual.
fn main() {
    println!("cargo:rerun-if-env-changed=UVCWEB_NATIVE_DIR");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var_os("CARGO_FEATURE_ANDROID_STATIC").is_none() {
        return;
    }

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let abi = match arch.as_str() {
        "aarch64" => "arm64-v8a",
        "arm" => "armeabi-v7a",
        "x86_64" => "x86_64",
        "x86" => "x86",
        _ => "",
    };
    match std::env::var("UVCWEB_NATIVE_DIR") {
        Ok(dir) if !dir.is_empty() && !abi.is_empty() => {
            println!("cargo:rustc-link-search=native={}/{}/lib", dir, abi);
        }
        _ => {
            println!("cargo:warning=the 'android-static' feature is on but UVCWEB_NATIVE_DIR is not set (or unknown CPU): libuvcusb.a will not be found. Build through android/build-rust.sh, which sets both.");
        }
    }
}
