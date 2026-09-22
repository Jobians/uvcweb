//! Unit tests for `src/protocols/web.rs`, kept in this separate file so the module itself stays clean.
//! Included there as `#[path = "../../tests/unit/web_tests.rs"] mod tests;` -- it is still logically part of
//! that module (private items stay reachable through `use super::*;`), just not stored inline.

use super::*;

#[test]
fn wav_header_layout_matches_what_the_page_parses() {
    let h = wav_header(96000, 1);
    assert_eq!(h.len(), 44);
    assert_eq!(&h[0..4], b"RIFF");
    // the page reads channels at byte 22 and the rate at bytes 24..28
    assert_eq!(u16::from_le_bytes([h[22], h[23]]), 1);
    assert_eq!(u32::from_le_bytes([h[24], h[25], h[26], h[27]]), 96000);
    assert_eq!(u32::from_le_bytes([h[28], h[29], h[30], h[31]]), 192000); // byte rate
    assert_eq!(&h[36..40], b"data");
}
