//! The JNI function table, reached without naming a single JNI function.
//!
//! Handing a Java `byte[]` to Rust and filling it needs three functions from the
//! JNI. They cannot simply be called by name: they live in libart, and since
//! Android 7 the loader does not let an app's own library see libart's symbols,
//! so a library that names them fails to load with
//! `UnsatisfiedLinkError: cannot locate symbol "GetArrayLength"` before a single
//! line of it runs. Linking against a library that does export them was the other
//! way out, and this one keeps the crate free of dependencies instead.
//!
//! What every native method is already given is a pointer to the table itself,
//! and its layout is fixed by the JNI specification: an entry sits at a known
//! place in it. That is the way in, and it is what the `jni` crate does too.
//!
//! The words below are counted from the start of the table: four reserved words,
//! then one per function. They were read out of the NDK's own `jni.h`, which is
//! the definition they have to match, and the tests at the bottom of this file
//! stand on a table built by hand, so a number that moves fails them.

use std::os::raw::c_void;

const WORD_GET_VERSION: usize = 4;
const WORD_GET_ARRAY_LENGTH: usize = 171;
const WORD_SET_BYTE_ARRAY_REGION: usize = 208;
const WORD_SET_LONG_ARRAY_REGION: usize = 212;

type GetVersion = unsafe extern "system" fn(*mut c_void, *mut i32) -> i32;
type GetArrayLength = unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32;
type SetByteArrayRegion = unsafe extern "system" fn(*mut c_void, *mut c_void, i32, i32, *const u8);
type SetLongArrayRegion = unsafe extern "system" fn(*mut c_void, *mut c_void, i32, i32, *const i64);

/// The function at one word of the table `env` points at.
///
/// The table is walked as an array of pointers rather than of `c_void`: Rust says
/// a `c_void` is one byte wide, so arithmetic on `*const c_void` counts bytes and
/// word five would be read from byte five. A 32-bit build has four-byte words, so
/// the same code has to leave the stride to the type.
///
/// # Safety
/// `env` must be a `JNIEnv*` and `index` a real place in the table behind it.
unsafe fn word(env: *mut c_void, index: usize) -> *const c_void {
    let table: *const *const c_void = *(env as *const *const *const c_void);
    *table.add(index)
}

/// Whether the table `env` points at really is a JNI table.
///
/// Nothing is written through the table before this has said yes: the JNI
/// version is a fixed shape, so a pointer that is not the table shows up here as
/// a wrong number rather than as damage further along.
///
/// # Safety
/// `env` must be a `JNIEnv*`.
pub unsafe fn table_is_sane(env: *mut c_void) -> bool {
    let mut version: i32 = 0;
    let f: GetVersion = std::mem::transmute(word(env, WORD_GET_VERSION));
    let got = f(env, &mut version);
    // Any JNI 1.x answers 0x0001xxxx and writes the same number back.
    (got as u32 & 0xffff_0000) == 0x0001_0000 && version == got
}

/// How many elements a Java array holds.
///
/// # Safety
/// `env` must be a `JNIEnv*` and `array` a live Java array.
pub unsafe fn array_length(env: *mut c_void, array: *mut c_void) -> i32 {
    let f: GetArrayLength = std::mem::transmute(word(env, WORD_GET_ARRAY_LENGTH));
    f(env, array)
}

/// Copies `buf` into a Java byte array at `start`.
///
/// # Safety
/// `env` must be a `JNIEnv*` and `array` a live Java `byte[]` with room for
/// `buf` from `start`.
pub unsafe fn set_bytes(env: *mut c_void, array: *mut c_void, start: i32, buf: &[u8]) {
    let f: SetByteArrayRegion = std::mem::transmute(word(env, WORD_SET_BYTE_ARRAY_REGION));
    f(env, array, start, buf.len() as i32, buf.as_ptr());
}

/// Copies `buf` into a Java `long[]` at `start`.
///
/// # Safety
/// `env` must be a `JNIEnv*` and `array` a live Java `long[]` with room for
/// `buf` from `start`.
pub unsafe fn set_longs(env: *mut c_void, array: *mut c_void, start: i32, buf: &[i64]) {
    let f: SetLongArrayRegion = std::mem::transmute(word(env, WORD_SET_LONG_ARRAY_REGION));
    f(env, array, start, buf.len() as i32, buf.as_ptr());
}

#[cfg(test)]
#[path = "../tests/unit/jnitable_tests.rs"]
mod tests;
