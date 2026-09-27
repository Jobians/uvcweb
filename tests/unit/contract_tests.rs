//! The app's Java declarations and the crate's JNI entry points have to describe
//! the same call. See [crate::contract].

use crate::contract::{KOTLIN, RUST};
use std::collections::BTreeMap;

/// One type as both languages write it. `i32` is the app's `Int`, `u8` its
/// `Boolean`, and nothing at all is what both mean by not returning anything.
/// Anything else is left as it was written, so it still compares.
fn scalar(ty: &str) -> String {
    match ty {
        "" | "()" | "Unit" => "Unit".to_string(),
        "i32" | "Int" => "Int".to_string(),
        "i64" | "Long" => "Long".to_string(),
        "u8" | "Boolean" => "Boolean".to_string(),
        "f64" | "Double" => "Double".to_string(),
        other => other.to_string(),
    }
}

/// What one side of the boundary declares about one function.
#[derive(Debug, PartialEq, Eq)]
struct Decl {
    /// Types in order; the `JNIEnv` and the class are not among them.
    params: Vec<String>,
    /// The Kotlin type the app expects back, or `Unit` for nothing.
    returns: String,
}

/// Every declaration that starts with `start`, which is followed by the name, an
/// argument list that may run over several lines, and then a result.
///
/// The argument list is found by counting brackets rather than by reading lines,
/// because both files put one parameter per line as soon as there is more than
/// one - which is every interesting function.
fn decls(text: &str, start: &str) -> BTreeMap<String, Decl> {
    let bytes = text.as_bytes();
    let mut found = BTreeMap::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(start) {
        let head = from + at + start.len();
        let Some(wide) = text[head..].find('(') else {
            break;
        };
        let name = text[head..head + wide].trim().to_string();
        let mut i = head + wide;
        let mut depth = 0usize;
        let close = loop {
            match bytes.get(i) {
                Some(b'(') => depth += 1,
                Some(b')') => {
                    depth -= 1;
                    if depth == 0 {
                        break i;
                    }
                }
                Some(b'{') | None => break 0, // not a declaration after all
                _ => {}
            }
            i += 1;
        };
        if close == 0 {
            from = i;
            continue;
        }
        let args = &text[head + wide + 1..close];
        let mut params: Vec<String> = args
            .split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(|a| a.rsplit(':').next().unwrap_or(a).trim().to_string())
            .collect();
        // Kotlin writes the result after the bracket, Rust with an arrow; both
        // end the type at the first space or brace. Only the closing line is
        // looked at: the next declaration's comment is not this one's result.
        let rest = text[close + 1..]
            .split('\n')
            .next()
            .unwrap_or("")
            .trim_start();
        let after = rest
            .strip_prefix("->")
            .or_else(|| rest.strip_prefix(':'))
            .map(str::trim_start)
            .unwrap_or(rest);
        let ty: String = after
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != '{' && *c != ',')
            .collect();
        let returns = scalar(&ty);
        // The same names on both sides, so a difference means a real difference:
        // an array is a bare pointer here and a named type in Kotlin, and both
        // are a Java array the caller owns.
        for p in &mut params {
            *p = if p == "*mut c_void" || p.ends_with("Array") {
                "array".to_string()
            } else {
                scalar(p)
            };
        }
        found.insert(name, Decl { params, returns });
        from = close + 1;
    }
    found
}

/// The app's side, as written in Kotlin.
fn kotlin() -> BTreeMap<String, Decl> {
    decls(KOTLIN, "external fun ")
}

/// This crate's entry points. The first two arguments are the `JNIEnv` and the
/// class, which the app never names.
fn rust() -> BTreeMap<String, Decl> {
    let mut found = decls(RUST, "pub extern \"C\" fn Java_com_uvcweb_app_Native_");
    for decl in found.values_mut() {
        if decl.params.len() >= 2 {
            decl.params.drain(..2);
        }
    }
    found
}

#[test]
fn every_native_method_the_app_calls_is_there() {
    let kotlin = kotlin();
    let rust = rust();
    assert!(
        kotlin.len() > 10,
        "the app's declarations were not found, so nothing was compared"
    );
    assert!(
        rust.len() > 10,
        "the crate's entry points were not found, so nothing was compared"
    );
    for name in kotlin.keys() {
        assert!(
            rust.contains_key(name),
            "the app calls Native.{name}, which this crate does not export"
        );
    }
    for name in rust.keys() {
        assert!(
            kotlin.contains_key(name),
            "this crate exports Native.{name}, which the app does not declare"
        );
    }
}

#[test]
fn both_sides_pass_and_return_the_same_number_of_things() {
    let kotlin = kotlin();
    let rust = rust();
    assert!(kotlin.len() > 10, "nothing to compare");
    for (name, k) in &kotlin {
        let r = rust.get(name).unwrap_or_else(|| {
            panic!("the app calls Native.{name}, which this crate does not export")
        });
        assert_eq!(
            k.params, r.params,
            "Native.{name}: the app passes {:?} and the native code takes {:?}",
            k.params, r.params
        );
        assert_eq!(
            k.returns, r.returns,
            "Native.{name}: the app expects {} and the native code returns {}",
            k.returns, r.returns
        );
    }
}

/// The rule the app got wrong: this crate fills a Java array, it never makes
/// one. A method that declares an array as its result is asking for a value
/// this side cannot produce, and is handed null instead.
#[test]
fn no_native_method_hands_back_a_java_array() {
    for (name, decl) in kotlin() {
        assert!(
            !decl.returns.ends_with("Array"),
            "Native.{name} is declared as returning a {}: this crate does not create Java \
             arrays, so pass one in and return how much of it was filled",
            decl.returns
        );
    }
}
