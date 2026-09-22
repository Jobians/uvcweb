//! Unit tests for `src/config.rs`, kept in this separate file so the module itself stays clean.
//! Included there as `#[path = "../tests/unit/config_tests.rs"] mod tests;` -- it is still logically part of
//! that module (private items stay reachable through `use super::*;`), just not stored inline.

use super::*;

fn a(s: &str) -> Vec<String> {
    s.split_whitespace().map(|x| x.to_string()).collect()
}

#[test]
fn default_is_web_on_8080() {
    let c = parse(&a("uvcweb -w 640 -h 480 -f 30 7")).unwrap();
    assert_eq!((c.width, c.height, c.fps, c.fd), (640, 480, 30, 7));
    assert_eq!(c.protocols, vec![("web".to_string(), 8080)]);
    assert!(c.audio);
}

#[test]
fn several_protocols_and_ports() {
    let c = parse(&a("uvcweb -P web,rtsp -p rtsp=9000 -l 7")).unwrap();
    assert_eq!(c.protocols, vec![("web".to_string(), 8080), ("rtsp".to_string(), 9000)]);
    assert!(c.lan);
    assert!(parse(&a("uvcweb -P web,rtsp -p 9000 7")).is_err());
    assert!(parse(&a("uvcweb -P nope 7")).is_err());
    assert!(parse(&a("uvcweb -P web,rtsp -p web=8554 7")).is_err()); // port clash
}

#[test]
fn old_command_lines_still_work() {
    let c = parse(&a("uvcweb -w 640 -h 480 -f 30 -a usb 7")).unwrap();
    assert!(c.audio);
    let c = parse(&a("uvcweb -a off -p 8081 7")).unwrap();
    assert!(!c.audio);
    assert_eq!(c.protocols, vec![("web".to_string(), 8081)]);
    assert!(parse(&a("uvcweb -a default 7")).is_err());
}
