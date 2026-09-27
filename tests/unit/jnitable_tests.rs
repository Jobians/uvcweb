//! The JNI table plumbing, stood up on a table built by hand.
//!
//! The real table is libart's, and there is no libart on this machine to test
//! against, so the tests build one: an array of function pointers of a fixed
//! size, where every word answers with poison and only the four places the module
//! says JNI keeps its functions answer for real. A number in the module that
//! points one word off then lands on poison and fails here, rather than jumping
//! somewhere random on a phone.

use super::*;
use std::cell::{Cell, RefCell};

/// Enough words for everything the module reaches for, plus room to spare.
const WORDS: usize = 260;
/// The word poison answers with, which no JNI would ever return.
const POISON: i32 = 0x7fff_ffff;
/// The version a real phone answers, JNI 1.6.
const JNI_1_6: i32 = 0x0001_0006;

// What the fake functions answer, kept per thread because each test runs on its
// own. The env a fake is given is the table - that is the whole point of the
// tests - so this is where the per-test state has to live instead.
thread_local! {
    static LENGTH: Cell<i32> = const { Cell::new(0) };
    static VERSION: Cell<i32> = const { Cell::new(POISON) };
    static BYTES: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static LONGS: RefCell<Vec<i64>> = const { RefCell::new(Vec::new()) };
    static AT: Cell<(i32, i32)> = const { Cell::new((-1, -1)) };
}

fn reset(version: i32) {
    LENGTH.with(|v| v.set(0));
    VERSION.with(|v| v.set(version));
    BYTES.with(|b| b.borrow_mut().clear());
    LONGS.with(|l| l.borrow_mut().clear());
    AT.with(|a| a.set((-1, -1)));
}

unsafe extern "system" fn fake_get_version(_env: *mut c_void, out: *mut i32) -> i32 {
    let got = VERSION.with(|v| v.get());
    *out = got;
    got
}

unsafe extern "system" fn fake_array_length(_env: *mut c_void, _array: *mut c_void) -> i32 {
    LENGTH.with(|v| v.get())
}

unsafe extern "system" fn fake_set_bytes(
    _env: *mut c_void,
    _array: *mut c_void,
    start: i32,
    len: i32,
    buf: *const u8,
) {
    AT.with(|a| a.set((start, len)));
    let from = std::slice::from_raw_parts(buf, len as usize);
    BYTES.with(|b| b.borrow_mut().extend_from_slice(from));
}

unsafe extern "system" fn fake_set_longs(
    _env: *mut c_void,
    _array: *mut c_void,
    start: i32,
    len: i32,
    buf: *const i64,
) {
    AT.with(|a| a.set((start, len)));
    let from = std::slice::from_raw_parts(buf, len as usize);
    LONGS.with(|l| l.borrow_mut().extend_from_slice(from));
}

/// What every other word answers, so a call that lands there is a wrong word
/// rather than a wrong anything else. It takes the most arguments any of the
/// real functions do and ignores them, so it survives being called as any of
/// them.
unsafe extern "system" fn poison(
    _env: *mut c_void,
    _a: *mut c_void,
    _b: i32,
    _c: i32,
    _d: *const u8,
) -> i32 {
    POISON
}

/// A table with poison everywhere and the four real places filled in.
fn jni_table() -> Vec<*const c_void> {
    let poison = poison as *const c_void;
    let mut words: Vec<*const c_void> = vec![poison; WORDS];
    words[4] = fake_get_version as *const c_void;
    words[171] = fake_array_length as *const c_void;
    words[208] = fake_set_bytes as *const c_void;
    words[212] = fake_set_longs as *const c_void;
    words
}

/// The env a native method is given: a pointer *to* the table pointer, which is
/// what the second argument of a native method is - one more level of pointer
/// than it looks like, and the thing a test has to get right too.
fn env_of(table: &[*const c_void]) -> *mut c_void {
    // The pointer to the table has to outlive the call, or the env handed to the
    // module would point at a local that has gone. A test can afford to leak it.
    let pointer: *const *const c_void = table.as_ptr();
    Box::into_raw(Box::new(pointer)) as *mut c_void
}

/// Something that is not the table, standing in for a Java array.
static AN_ARRAY: [u8; 8] = [0; 8];

fn an_array() -> *mut c_void {
    AN_ARRAY.as_ptr() as *mut c_void
}

#[test]
fn a_jni_table_is_recognised() {
    reset(JNI_1_6);
    let table = jni_table();
    assert!(
        unsafe { table_is_sane(env_of(&table)) },
        "a real table must pass"
    );
}

#[test]
fn something_that_is_not_a_table_is_refused() {
    let mut table = jni_table();
    table[4] = poison as *const c_void; // the version is what says it
    assert!(
        !unsafe { table_is_sane(env_of(&table)) },
        "poison must not pass"
    );

    reset(0); // a table that answers zero is not one either
    let table = jni_table();
    assert!(
        !unsafe { table_is_sane(env_of(&table)) },
        "a zero version must not pass"
    );
}

#[test]
fn the_length_of_an_array_comes_from_its_own_place() {
    let table = jni_table();
    for len in [1i32, 7, 4096, 65_536] {
        reset(JNI_1_6);
        LENGTH.with(|v| v.set(len));
        let got = unsafe { array_length(env_of(&table), an_array()) };
        assert_eq!(got, len, "the module must call the function at word 171");
    }
}

#[test]
fn bytes_are_written_where_they_were_told_to_go() {
    let table = jni_table();
    let picture = [1u8, 2, 3, 4, 5];

    reset(JNI_1_6);
    unsafe { set_bytes(env_of(&table), an_array(), 0, &picture) };
    assert_eq!(BYTES.with(|b| b.borrow().clone()), picture.to_vec());
    assert_eq!(
        AT.with(|a| a.get()),
        (0, 5),
        "the slice must be handed over at once"
    );

    reset(JNI_1_6);
    unsafe { set_bytes(env_of(&table), an_array(), 3, &picture[1..]) };
    assert_eq!(
        AT.with(|a| a.get()),
        (3, 4),
        "a start further in must be passed on"
    );
    assert_eq!(BYTES.with(|b| b.borrow().clone()), vec![2, 3, 4, 5]);
}

#[test]
fn longs_are_written_where_they_were_told_to_go() {
    let table = jni_table();
    let numbers = [1i64, -1, 9_223_372_036_854_775_000];
    reset(JNI_1_6);
    unsafe { set_longs(env_of(&table), an_array(), 0, &numbers) };
    assert_eq!(LONGS.with(|l| l.borrow().clone()), numbers.to_vec());
    assert_eq!(
        AT.with(|a| a.get()),
        (0, 3),
        "the slice must be handed over at once"
    );
}

#[test]
fn a_word_that_moved_would_be_caught() {
    // The reason the words are read out of the header rather than trusted: one
    // that moved would call the wrong function. Here that means the neighbour,
    // which is poison, and poison does not look like an answer.
    let mut table = jni_table();
    table.swap(171, 172);
    reset(JNI_1_6);
    LENGTH.with(|v| v.set(512));
    let got = unsafe { array_length(env_of(&table), an_array()) };
    assert_eq!(got, POISON, "a moved word must not reach the real function");
}

#[test]
fn the_table_starts_with_four_reserved_words() {
    // GetVersion is the first function, so it is the fifth word: four reserved
    // ones come first. This is why the numbers are 4 and not 0.
    let table = jni_table();
    assert_eq!(table[0], poison as *const c_void, "word 0 is reserved");
    assert_eq!(table[3], poison as *const c_void, "word 3 is reserved");
    assert_eq!(table[4], fake_get_version as *const c_void);
    // And the table a native method is given points at its first word.
    assert_eq!(
        unsafe { word(env_of(&table), 0) },
        poison as *const c_void,
        "the env points at word 0"
    );
    assert_eq!(
        std::mem::size_of::<*const c_void>(),
        std::mem::size_of::<usize>()
    );
}
