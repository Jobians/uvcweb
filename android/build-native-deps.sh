#!/usr/bin/env bash
# Builds libusb + libuvc for Android as ONE static archive per CPU type:
#     native-deps/<abi>/lib/libuvcusb.a
# The Rust library links that archive when the "android-static" feature is on
# (see ../build.rs, ../src/ffi.rs and ./build-rust.sh).
#
# Needs the Android NDK (export ANDROID_NDK_HOME=/path/to/ndk/<version>).
# Works on Linux, macOS and Windows WSL.
#
# Source: clone libusb and libuvc yourself into third_party/ (repo root) and this script uses
# them as-is, no network access and no `git clone` here:
#     git clone --branch v1.0.30 https://github.com/libusb/libusb.git third_party/libusb
#     git clone --branch v0.0.8  https://github.com/libuvc/libuvc.git third_party/libuvc
# Without third_party/, it falls back to cloning into a scratch folder itself (needs git).
# Re-run this script after changing anything in either source tree; every object file is always
# rebuilt from scratch, so local patches are never missed.
#
# Usage: ./build-native-deps.sh [--help]
#
# Options (environment variables):
#   ABIS="arm64-v8a armeabi-v7a x86_64"   which CPU types to build (default: those three)
#   ANDROID_API=26                        minimum Android version (default 26 = Android 8)
#   LIBUSB_REF=v1.0.30  LIBUVC_REF=v0.0.8 git tags to use for the fallback clone
#   LIBUSB_DIR=... LIBUVC_DIR=...         use these exact source folders (skips third_party/ and
#                                         the fallback clone entirely)
#   OUT=...                               where to write native-deps (default: alongside this script)
set -euo pipefail

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  # print the header comment (everything from line 2 up to the first non-comment line)
  awk 'NR==1 {next} /^#/ {sub(/^# ?/, ""); print; next} {exit}' "$0"
  exit 0
fi

# ---------------------------------------------------------------- configuration

ANDROID_API="${ANDROID_API:-26}"
ABIS="${ABIS:-arm64-v8a armeabi-v7a x86_64}"
LIBUSB_REF="${LIBUSB_REF:-v1.0.30}"
LIBUVC_REF="${LIBUVC_REF:-v0.0.8}"

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/.." && pwd)"
OUT="${OUT:-$HERE/native-deps}"
WORK="$OUT/.src"

# The files libusb's own android/jni/libusb.mk builds.
LIBUSB_SRC="core.c descriptor.c hotplug.c io.c strerror.c sync.c
            os/events_posix.c os/linux_usbfs.c os/threads_posix.c os/linux_netlink.c"
# The files libuvc's CMakeLists builds. frame-mjpeg.c is left out: it needs libjpeg, and this
# project never asks libuvc to decode a frame, only to hand the raw MJPEG bytes to the browser.
LIBUVC_SRC="ctrl ctrl-gen device diag frame init misc stream"

die() { echo "ERROR: $*" >&2; exit 1; }
note() { echo "==> $*"; }

# ---------------------------------------------------------------- NDK toolchain

find_ndk_bin() {
  local ndk="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}"
  [ -n "$ndk" ] && [ -d "$ndk" ] || die "set ANDROID_NDK_HOME to your NDK folder (Android Studio > SDK Manager > SDK Tools > NDK)"
  local host
  case "$(uname -s)" in
    Linux)  host=linux-x86_64 ;;
    Darwin) host=darwin-x86_64 ;;   # the NDK ships x86_64 tools for Apple silicon too
    *)      die "use Linux, macOS or WSL" ;;
  esac
  local bin="$ndk/toolchains/llvm/prebuilt/$host/bin"
  [ -d "$bin" ] || die "NDK toolchain not found: $bin"
  echo "$bin"
}

abi_to_triple() {
  case "$1" in
    arm64-v8a)   echo aarch64-linux-android ;;
    armeabi-v7a) echo armv7a-linux-androideabi ;;
    x86_64)      echo x86_64-linux-android ;;
    x86)         echo i686-linux-android ;;
    *)           die "unknown ABI: $1 (expected one of: arm64-v8a armeabi-v7a x86_64 x86)" ;;
  esac
}

# ---------------------------------------------------------------- source resolution

# Sets $1 (a variable name) to the libusb/libuvc source directory: an explicit override wins,
# then a tree already vendored at the repo root, then a scratch clone as a last resort.
resolve_source() {
  local var="$1" dir_override="$2" vendor_probe="$3" vendor_dir="$4" scratch_dir="$5" \
        ref="$6" url="$7" what="$8"
  local dir="$dir_override"
  if [ -z "$dir" ]; then
    if [ -d "$vendor_probe" ]; then
      dir="$vendor_dir"
    else
      dir="$scratch_dir"
    fi
  fi
  if [ ! -d "$dir" ]; then
    command -v git >/dev/null || die "git is needed to download $what (or clone it yourself into $vendor_dir, or set $var)"
    note "cloning $what $ref (no local copy found in $vendor_dir)"
    git clone --depth 1 --branch "$ref" "$url" "$dir"
  fi
  printf -v "$var" '%s' "$dir"
}

check_files() {
  local base="$1" what="$2" ref_var="$3"; shift 3
  local f
  for f in "$@"; do
    [ -f "$base/$f" ] || die "$what source missing: $base/$f (a different $ref_var? the file list near the top of this script may need adjusting)"
  done
}

# libuvc normally generates this header with CMake; do the same with sed. Always regenerated
# (it's a few milliseconds of work) so a freshly vendored or patched libuvc never builds against a
# stale header left over from an earlier run.
generate_libuvc_config_header() {
  local libuvc_dir="$1"
  local cfg_in="$libuvc_dir/include/libuvc/libuvc_config.h.in"
  local cfg_out="$libuvc_dir/include/libuvc/libuvc_config.h"

  [ -f "$cfg_in" ] || die "missing $cfg_in"

  local version="${LIBUVC_REF#v}"
  local major minor patch

  IFS=. read -r major minor patch <<< "$version"

  : "${major:=0}"
  : "${minor:=0}"
  : "${patch:=0}"

  sed -E \
    -e "s/@libuvc_VERSION_MAJOR@/$major/g" \
    -e "s/@libuvc_VERSION_MINOR@/$minor/g" \
    -e "s/@libuvc_VERSION_PATCH@/$patch/g" \
    -e "s/@libuvc_VERSION@/$version/g" \
    -e 's/^#cmakedefine.*$//' \
    -e 's/@[A-Za-z_]+@/0/g' \
    "$cfg_in" > "$cfg_out"
}

# libusb's own build (autotools/CMake) generates version_describe.h from `git describe` at build
# time; core.c includes it just to fill in one cosmetic version-string field, nothing this project
# reads. Building outside that system, as this script does, never produces it, so write a harmless
# placeholder instead. version_nano.h is normally tracked in git and libusb.git clones already have
# it, but some tags/mirrors omit it, so a fallback is generated for that too if it's missing.
generate_libusb_version_headers() {
  local dir="$1/libusb"
  if [ ! -f "$dir/version_nano.h" ]; then
    echo "#define LIBUSB_NANO 0" > "$dir/version_nano.h"
  fi
  printf '#define LIBUSB_DESCRIBE "%s"\n' "$LIBUSB_REF" > "$dir/version_describe.h"
}

# ---------------------------------------------------------------- per-ABI build

build_abi() {
  local abi="$1" cc="$2"
  local obj="$WORK/obj/$abi"
  rm -rf "$obj"
  mkdir -p "$obj" "$OUT/$abi/lib"
  note "$abi"

  local f
  for f in $LIBUSB_SRC; do
    "$cc" -O2 -fPIC -w -DHAVE_CONFIG_H \
      -I"$LIBUSB_DIR/android" -I"$LIBUSB_DIR/libusb" -I"$LIBUSB_DIR/libusb/os" \
      -c "$LIBUSB_DIR/libusb/$f" -o "$obj/usb_${f//\//_}.o"
  done
  for f in $LIBUVC_SRC; do
    "$cc" -O2 -fPIC -w -std=gnu99 \
      -I"$LIBUVC_DIR/include" -I"$LIBUSB_DIR/libusb" \
      -c "$LIBUVC_DIR/src/$f.c" -o "$obj/uvc_$f.o"
  done

  local archive="$OUT/$abi/lib/libuvcusb.a"
  rm -f "$archive"
  "$AR" rcs "$archive" "$obj"/*.o
  local n_obj size
  n_obj=$(find "$obj" -name '*.o' | wc -l | tr -d ' ')
  size=$(du -h "$archive" | cut -f1)
  note "  -> $archive  ($n_obj objects, $size)"
}

# ---------------------------------------------------------------- main

NDK_BIN="$(find_ndk_bin)"
AR="$NDK_BIN/llvm-ar"
[ -x "$AR" ] || die "missing $AR"

mkdir -p "$WORK"
resolve_source LIBUSB_DIR "${LIBUSB_DIR:-}" \
  "$REPO_ROOT/third_party/libusb/libusb" "$REPO_ROOT/third_party/libusb" "$WORK/libusb" \
  "$LIBUSB_REF" https://github.com/libusb/libusb.git libusb
resolve_source LIBUVC_DIR "${LIBUVC_DIR:-}" \
  "$REPO_ROOT/third_party/libuvc/src" "$REPO_ROOT/third_party/libuvc" "$WORK/libuvc" \
  "$LIBUVC_REF" https://github.com/libuvc/libuvc.git libuvc
note "libusb source: $LIBUSB_DIR"
note "libuvc source:  $LIBUVC_DIR"

# shellcheck disable=SC2086  # $LIBUSB_SRC / $LIBUVC_SRC are intentionally word-split file lists
check_files "$LIBUSB_DIR/libusb" libusb LIBUSB_REF $LIBUSB_SRC
[ -f "$LIBUSB_DIR/android/config.h" ] || die "missing $LIBUSB_DIR/android/config.h"
uvc_files=""
for f in $LIBUVC_SRC; do uvc_files="$uvc_files $f.c"; done
# shellcheck disable=SC2086
check_files "$LIBUVC_DIR/src" libuvc LIBUVC_REF $uvc_files

generate_libuvc_config_header "$LIBUVC_DIR"
generate_libusb_version_headers "$LIBUSB_DIR"

for abi in $ABIS; do
  triple="$(abi_to_triple "$abi")"
  cc="$NDK_BIN/${triple}${ANDROID_API}-clang"
  [ -x "$cc" ] || die "missing compiler $cc (is ANDROID_API=$ANDROID_API available in this NDK?)"
  build_abi "$abi" "$cc"
done

note "done. Next: ./build-rust.sh"
