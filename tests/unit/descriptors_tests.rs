//! Unit tests for `src/descriptors.rs`, kept in this separate file so the module itself stays clean.
//! Included there as `#[path = "../tests/unit/descriptors_tests.rs"] mod tests;` -- it is still logically part of
//! that module (private items stay reachable through `use super::*;`), just not stored inline.

use super::*;

fn frame_desc(idx: u8, w: u16, h: u16, interval: u32) -> Vec<u8> {
    let mut d = vec![30, 0x24, 0x07, idx, 0];
    d.extend_from_slice(&w.to_le_bytes());
    d.extend_from_slice(&h.to_le_bytes());
    d.extend_from_slice(&[0u8; 12]); // min/max bitrate, max frame buffer
    d.extend_from_slice(&interval.to_le_bytes());
    d.push(1); // one discrete interval
    d.extend_from_slice(&interval.to_le_bytes());
    assert_eq!(d.len(), 30);
    d
}

#[test]
fn uvc_default_mode() {
    let mut extra = vec![11, 0x24, 0x06, 1, 3, 0, 2, 0, 0, 0, 0]; // MJPEG format, default frame 2
    extra.extend(frame_desc(1, 1920, 1080, 333333));
    extra.extend(frame_desc(2, 1280, 720, 166666));
    extra.extend(frame_desc(3, 640, 480, 333333));
    let alts = vec![RawAlt { interface: 1, alt: 0, class: 14, subclass: 2, endpoints: vec![], extra }];
    let modes = mjpeg_modes(&alts);
    assert_eq!(modes.len(), 3);
    assert_eq!(default_mode(&modes), Some((1280, 720, 60)));
}

#[test]
fn uvc_no_mjpeg() {
    let extra = vec![11, 0x24, 0x04, 1, 3, 0, 0, 0, 0, 0, 0]; // uncompressed only
    let alts = vec![RawAlt { interface: 1, alt: 0, class: 14, subclass: 2, endpoints: vec![], extra }];
    assert!(default_mode(&mjpeg_modes(&alts)).is_none());
}

/// The values the user's real card reported in its log:
/// interface 3, alt 1, endpoint 0x82, 256 bytes/packet, 1 ch, 16 bit, 96000 Hz.
fn ms2109_like() -> Vec<RawAlt> {
    let mut extra = vec![7, 0x24, 0x01, 1, 1, 1, 0]; // AS_GENERAL, PCM
    extra.extend_from_slice(&[11, 0x24, 0x02, 1, 1, 2, 16, 1, 0x00, 0x77, 0x01]); // FORMAT_TYPE_I, 96000
    vec![
        RawAlt { interface: 2, alt: 0, class: 1, subclass: 1, endpoints: vec![], extra: vec![] },
        RawAlt { interface: 3, alt: 0, class: 1, subclass: 2, endpoints: vec![], extra: vec![] },
        RawAlt {
            interface: 3,
            alt: 1,
            class: 1,
            subclass: 2,
            endpoints: vec![RawEndpoint { address: 0x82, attributes: 0x05, max_packet: 256, extra: vec![7, 0x25, 1, 0, 0, 0, 0] }],
            extra,
        },
    ]
}

#[test]
fn audio_alt_parsing_matches_real_card() {
    let alts = audio_alts(&ms2109_like());
    assert_eq!(alts.len(), 1);
    let a = &alts[0];
    assert_eq!((a.interface, a.alt, a.endpoint, a.max_packet), (3, 1, 0x82, 256));
    assert_eq!((a.channels, a.subframe, a.bits, a.pcm, a.freq_ctl), (1, 2, 16, true, false));
    assert_eq!(a.rates, vec![96000]);
    // the card only does 96 kHz mono, whatever we ask for
    let c = choose_audio(&alts, 48000, 2).unwrap();
    assert_eq!(pick_rate(c, 48000), 96000);
}

#[test]
fn audio_selection_prefers_requested_format() {
    let mut alts = audio_alts(&ms2109_like());
    let mut stereo = alts[0].clone();
    stereo.alt = 2;
    stereo.channels = 2;
    stereo.rates = vec![44100, 48000];
    let mut wide = alts[0].clone();
    wide.alt = 3;
    wide.bits = 24;
    wide.subframe = 3; // not 16-bit: must never be chosen
    alts.push(stereo);
    alts.push(wide);
    let c = choose_audio(&alts, 48000, 2).unwrap();
    assert_eq!((c.alt, pick_rate(c, 48000)), (2, 48000));
    let c = choose_audio(&alts, 96000, 1).unwrap();
    assert_eq!(c.alt, 1);
    assert_eq!(pick_rate(&alts[1], 96000), 48000); // nearest offered rate
}

#[test]
fn malformed_descriptors_do_not_panic() {
    let alts = vec![RawAlt {
        interface: 3,
        alt: 1,
        class: 1,
        subclass: 2,
        endpoints: vec![RawEndpoint { address: 0x82, attributes: 5, max_packet: 100, extra: vec![9] }],
        extra: vec![200, 0x24, 2, 1, 1, 2, 16, 3], // length runs past the end
    }];
    let a = audio_alts(&alts);
    assert_eq!(a.len(), 1);
    assert!(choose_audio(&a, 48000, 2).is_none());
}
