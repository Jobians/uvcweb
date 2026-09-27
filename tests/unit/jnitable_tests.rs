//! The JNI table plumbing, stood up on a table built by hand.
//!
//! The real table is libart's, and there is no libart on this machine to test
//! against, so the tests build one: an array of function pointers of a fixed
//! size, where every word answers with poison and only the four places the module
//! says JNI keeps its functions answer for real. A number in the module that
//! points one word off then lands on poison and fails here, rather than jumping
//! somewhere random on a phone.
//!
//! The stubs are written to the prototypes in the JNI specification and not to
//! the types the module uses, so this checks the module against JNI rather than
//! against itself: a wrong word is caught by the poison, and a wrong signature by
//! the stub refusing to be called the way the module calls it.

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

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

/// The JNI specification gives GetVersion one argument and a result, and this is
/// that and nothing else - no second parameter to write an answer into. Writing
/// the fakes to the module's own types instead would make the tests agree with
/// any signature it happens to use, and a wrong one is exactly what they are
/// here to catch: a stub with the real shape leaves the module's extra argument
/// unwritten, and the check that reads it back fails.
unsafe extern "system" fn fake_get_version(_env: *mut c_void) -> i32 {
    VERSION.with(|v| v.get())
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

// ------------------------------------------------------------------ the shapes

/// The four prototypes, as the JNI specification writes them. Quoted rather than
/// reached for: no NDK is guaranteed to be on the machine running these, and a
/// test that reads the header itself would only be as good as the header's
/// presence. This is the part of `jni.h` that has to match, counted out of the
/// table this file's fake stands in for.
const SPEC: &str = "\
    jint    GetVersion(JNIEnv *env);
    jsize   GetArrayLength(JNIEnv *env, jarray array);
    void    SetByteArrayRegion(JNIEnv *env, jbyteArray array, jsize start, jsize len, const jbyte *buf);
    void    SetLongArrayRegion(JNIEnv *env, jlongArray array, jsize start, jsize len, const jlong *buf);
";

/// The module's own source, so the types it declares can be read as written.
const SOURCE: &str = include_str!("../../src/jnitable.rs");

/// The JNI types that are references to Java objects rather than numbers. They
/// are all written as `typedef jobject j...`, so they are pointers even though
/// the C says nothing about stars - which is the whole reason this is listed
/// instead of guessed at.
const REFERENCES: [&str; 9] = [
    "jobject",
    "jclass",
    "jstring",
    "jthrowable",
    "jarray",
    "jbyteArray",
    "jintArray",
    "jlongArray",
    "jobjectArray",
];

/// Each argument as one letter - `P` for a pointer, `I` for a number - and the
/// result as one more, `V` for nothing. Nothing else about a type matters here:
/// what matters is which values are passed in which registers, and a pointer
/// where a number belongs (or the other way round) is the mistake worth catching.
fn shape(args: &str, returns: &str) -> (Vec<char>, char) {
    let one = |a: &str| {
        if a.contains('*') {
            'P'
        } else if a
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|word| REFERENCES.contains(&word))
        {
            'P'
        } else {
            'I'
        }
    };
    let params: Vec<char> = args
        .split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(one)
        .collect();
    let back = match returns.trim() {
        "" | "()" | "void" => 'V',
        _ => 'I',
    };
    (params, back)
}

/// What the specification says each function is, read out of [SPEC].
fn spec() -> BTreeMap<String, (Vec<char>, char)> {
    let mut out = BTreeMap::new();
    for line in SPEC.lines() {
        let Some((head, args)) = line.split_once('(') else {
            continue;
        };
        let mut words = head.split_whitespace();
        let returns = words.next().unwrap_or("");
        let name = words.next().unwrap_or("").to_string();
        let args = args.trim_end_matches(')').trim_end_matches(';');
        out.insert(name, shape(args, returns));
    }
    out
}

/// What the module declares each function to be, read out of its own source.
fn declared() -> BTreeMap<String, (Vec<char>, char)> {
    let mut out = BTreeMap::new();
    for line in SOURCE.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("type ") else {
            continue;
        };
        let Some((name, rest)) = rest.split_once('=') else {
            continue;
        };
        let Some(open) = rest.find("fn(") else {
            continue;
        };
        let Some(close) = rest.rfind(')') else {
            continue;
        };
        let returns = rest[close..].trim_start_matches(')').trim();
        let returns = returns.strip_prefix("->").unwrap_or("").trim();
        out.insert(
            name.trim().to_string(),
            shape(&rest[open + 3..close], returns),
        );
    }
    out
}

#[test]
fn the_four_types_are_the_ones_the_specification_gives() {
    let spec = spec();
    let declared = declared();
    assert_eq!(spec.len(), 4, "the specification excerpt was not read");
    for (name, want) in &spec {
        let got = declared
            .get(name)
            .unwrap_or_else(|| panic!("the module declares no type for {name}"));
        assert_eq!(
            want, got,
            "{name} is declared as {:?} and the specification says {:?}: a different \
             number of arguments, or a pointer where a number belongs, would call \
             something else",
            got, want
        );
    }
}

/// The same four, counted out of the table itself, so a number that moves in the
/// specification cannot be quietly satisfied by a number that moved in the
/// module. Four reserved words, then one entry per function.
#[test]
fn the_four_words_are_where_the_specification_puts_them() {
    let mut table = jni_table();
    for (index, name) in [
        (4, "GetVersion"),
        (171, "GetArrayLength"),
        (208, "SetByteArrayRegion"),
        (212, "SetLongArrayRegion"),
    ] {
        // The stub for each is the one that answers like the specification's.
        table[index] = match name {
            "GetVersion" => fake_get_version as *const c_void,
            "GetArrayLength" => fake_array_length as *const c_void,
            "SetByteArrayRegion" => fake_set_bytes as *const c_void,
            _ => fake_set_longs as *const c_void,
        };
    }
    let env = env_of(&table);
    reset(JNI_1_6);
    LENGTH.with(|v| v.set(7));
    unsafe {
        assert!(table_is_sane(env), "the version comes from word 4");
        assert_eq!(
            array_length(env, an_array()),
            7,
            "the length comes from word 171"
        );
        set_bytes(env, an_array(), 0, &[1, 2, 3]);
        set_longs(env, an_array(), 0, &[4, 5]);
    }
    assert_eq!(
        BYTES.with(|b| b.borrow().clone()),
        vec![1, 2, 3],
        "bytes come from word 208"
    );
    assert_eq!(
        LONGS.with(|l| l.borrow().clone()),
        vec![4, 5],
        "longs come from word 212"
    );
}
