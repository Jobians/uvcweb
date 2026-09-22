#!/usr/bin/env bash
# Builds the Rust library (libuvcweb_core.so) for Android and puts it where Gradle packages it:
#     app/src/main/jniLibs/<abi>/libuvcweb_core.so
#
# One-time setup:
#     rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
#     cargo install cargo-ndk
#     export ANDROID_NDK_HOME=/path/to/ndk/<version>
#     ./build-native-deps.sh
#
# Usage: ./build-rust.sh [--help]
#
# Options (environment variables):
#   ABIS="arm64-v8a armeabi-v7a x86_64"   which CPU types to build (must match build-native-deps.sh)
#   ANDROID_API=26                        minimum Android version (must match build-native-deps.sh)
#   NATIVE_DEPS_DIR=...                   where build-native-deps.sh wrote its output (default:
#                                         ./native-deps; set this if you built it with OUT=... there)
set -euo pipefail

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  # print the header comment (everything from line 2 up to the first non-comment line)
  awk 'NR==1 {next} /^#/ {sub(/^# ?/, ""); print; next} {exit}' "$0"
  exit 0
fi

HERE="$(cd "$(dirname "$0")" && pwd)"
ABIS="${ABIS:-arm64-v8a armeabi-v7a x86_64}"
ANDROID_API="${ANDROID_API:-26}"
NATIVE_DEPS_DIR="${NATIVE_DEPS_DIR:-$HERE/native-deps}"

die() { echo "ERROR: $*" >&2; exit 1; }
note() { echo "==> $*"; }

abi_to_target() {
  case "$1" in
    arm64-v8a)   echo aarch64-linux-android ;;
    armeabi-v7a) echo armv7-linux-androideabi ;;
    x86_64)      echo x86_64-linux-android ;;
    x86)         echo i686-linux-android ;;
    *)           die "unknown ABI: $1 (expected one of: arm64-v8a armeabi-v7a x86_64 x86)" ;;
  esac
}

# ---------------------------------------------------------------- checks

[ -n "${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}" ] || die "set ANDROID_NDK_HOME to your NDK folder"
command -v cargo-ndk >/dev/null 2>&1 || die "cargo-ndk not found: cargo install cargo-ndk"

for abi in $ABIS; do
  [ -f "$NATIVE_DEPS_DIR/$abi/lib/libuvcusb.a" ] \
    || die "missing $NATIVE_DEPS_DIR/$abi/lib/libuvcusb.a - run ./build-native-deps.sh first (with matching ABIS/ANDROID_API/OUT)"
done

# A missing rustup target produces a confusing linker error much later; catch it here instead.
# Skipped quietly if rustup itself isn't the toolchain manager in use.
if command -v rustup >/dev/null 2>&1; then
  installed="$(rustup target list --installed 2>/dev/null || true)"
  for abi in $ABIS; do
    target="$(abi_to_target "$abi")"
    if [ -n "$installed" ] && ! grep -qx "$target" <<< "$installed"; then
      die "rustup target '$target' is not installed: rustup target add $target"
    fi
  done
fi

# ---------------------------------------------------------------- build

export UVCWEB_NATIVE_DIR="$NATIVE_DEPS_DIR"
# Android 15 devices with 16 KB memory pages want 16 KB aligned libraries.
export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-z,max-page-size=16384"

cargo_ndk_targets=()
for abi in $ABIS; do
  cargo_ndk_targets+=(-t "$abi")
done

note "building for: $ABIS"
cd "$HERE/.."
cargo ndk "${cargo_ndk_targets[@]}" --platform "$ANDROID_API" \
  -o "$HERE/app/src/main/jniLibs" \
  build --release --lib --features android-static

echo
note "done. Libraries written to app/src/main/jniLibs:"
for abi in $ABIS; do
  so="$HERE/app/src/main/jniLibs/$abi/libuvcweb_core.so"
  if [ -f "$so" ]; then
    note "  $abi: $(du -h "$so" | cut -f1)"
  else
    echo "WARNING: expected $so but it was not produced" >&2
  fi
done
note "Open the android/ folder in Android Studio and run the app."
