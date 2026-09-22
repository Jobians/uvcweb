//! Tiny timestamped logger: `say!("text {}", x)` prints `[HH:MM:SS] text ...`.
//!
//! * normal builds: to stderr
//! * Android: to logcat (tag "uvcweb")
//! * everywhere: also appended to the file named by $UVCWEB_LOG_FILE, if set
//!   (the Android app sets it so it can show the log on screen).

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::raw::c_void;
use std::sync::Mutex;

extern "C" {
    fn time(t: *mut i64) -> isize;
    fn localtime_r(t: *const i64, out: *mut c_void) -> *mut c_void;
}

static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);

/// Local wall-clock time as HH:MM:SS.
pub fn stamp() -> String {
    // `struct tm` starts with tm_sec, tm_min, tm_hour (ints) on every libc we care about.
    // Give libc a buffer that is larger than any `struct tm`.
    let mut tm = [0i64; 8];
    let mut t: i64 = 0;
    unsafe {
        time(&mut t);
        localtime_r(&t, tm.as_mut_ptr() as *mut c_void);
        let p = tm.as_ptr() as *const i32;
        format!("{:02}:{:02}:{:02}", *p.add(2), *p.add(1), *p)
    }
}

/// (Re)open the log file named by $UVCWEB_LOG_FILE. Called at the start of every session.
pub fn init_file_from_env() {
    let mut g = LOG_FILE.lock().unwrap_or_else(|e| e.into_inner());
    *g = None;
    if let Ok(path) = std::env::var("UVCWEB_LOG_FILE") {
        if !path.is_empty() {
            *g = OpenOptions::new().create(true).append(true).open(path).ok();
        }
    }
}

#[cfg(target_os = "android")]
#[link(name = "log")]
extern "C" {
    fn __android_log_write(prio: i32, tag: *const std::os::raw::c_char, text: *const std::os::raw::c_char) -> i32;
}

#[cfg(target_os = "android")]
fn write_platform(line: &str) {
    let tag = std::ffi::CString::new("uvcweb").unwrap_or_default();
    let text = std::ffi::CString::new(line.replace('\0', " ")).unwrap_or_default();
    unsafe {
        __android_log_write(4, tag.as_ptr(), text.as_ptr()); // 4 = ANDROID_LOG_INFO
    }
    eprintln!("{}", line); // Termux CLI runs as a normal process with a real stderr/tty
}

#[cfg(not(target_os = "android"))]
fn write_platform(line: &str) {
    eprintln!("{}", line);
}

pub fn log(args: fmt::Arguments) {
    let line = format!("[{}] {}", stamp(), args);
    write_platform(&line);
    if let Ok(mut g) = LOG_FILE.lock() {
        if let Some(f) = g.as_mut() {
            let _ = writeln!(f, "{}", line);
        }
    }
}

/// Usable from other crates (the `uvcweb` binary) as `uvcweb_core::say!`.
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => { $crate::log::log(format_args!($($arg)*)) };
}
