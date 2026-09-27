//! Checks that the app's Java side and this crate's JNI entry points agree.
//!
//! A native method is called by name and by shape: the argument list the app
//! declares is the argument list the native code is handed, and the value it
//! returns is whatever the native code returns. Nothing checks that the two
//! sides match, so a change to either can leave the other quietly wrong - and
//! wrong here is not a compile error, it is a null array or a length read from
//! whatever happened to be in a register, a long way from the line that was
//! changed.
//!
//! So both files are read as text and compared. That is not as good as a build
//! that cannot go wrong, but it is the only check that sees both sides at once,
//! and it is the one that would have caught the app asking for a `LongArray`
//! back from a function that fills one it was given.

/// The app's declarations, as written in Kotlin.
const KOTLIN: &str = include_str!("../android/app/src/main/java/com/uvcweb/app/Native.kt");

/// This crate's entry points, as written in Rust.
const RUST: &str = include_str!("android.rs");

#[cfg(test)]
#[path = "../tests/unit/contract_tests.rs"]
mod tests;
