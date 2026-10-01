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
    // bLength, type, MJPEG, format 1, 3 frame descriptors, default frame 2, then the
    // guid would start at index 6.
    let mut extra = vec![11, 0x24, 0x06, 1, 3, 2, 0, 0, 0, 0, 0];
    extra.extend(frame_desc(1, 1920, 1080, 333333));
    extra.extend(frame_desc(2, 1280, 720, 166666));
    extra.extend(frame_desc(3, 640, 480, 333333));
    let alts = vec![RawAlt {
        interface: 1,
        alt: 0,
        class: 14,
        subclass: 2,
        endpoints: vec![],
        extra,
    }];
    let modes = mjpeg_modes(&alts);
    assert_eq!(modes.len(), 3);
    assert_eq!(default_mode(&modes), Some((1280, 720, 60)));
}

#[test]
fn uvc_no_mjpeg() {
    let extra = vec![11, 0x24, 0x04, 1, 3, 0, 0, 0, 0, 0, 0]; // uncompressed only
    let alts = vec![RawAlt {
        interface: 1,
        alt: 0,
        class: 14,
        subclass: 2,
        endpoints: vec![],
        extra,
    }];
    assert!(default_mode(&mjpeg_modes(&alts)).is_none());
}

/// The values the user's real card reported in its log:
/// interface 3, alt 1, endpoint 0x82, 256 bytes/packet, 1 ch, 16 bit, 96000 Hz.
fn ms2109_like() -> Vec<RawAlt> {
    let mut extra = vec![7, 0x24, 0x01, 1, 1, 1, 0]; // AS_GENERAL, PCM
    extra.extend_from_slice(&[11, 0x24, 0x02, 1, 1, 2, 16, 1, 0x00, 0x77, 0x01]); // FORMAT_TYPE_I, 96000
    vec![
        RawAlt {
            interface: 2,
            alt: 0,
            class: 1,
            subclass: 1,
            endpoints: vec![],
            extra: vec![],
        },
        RawAlt {
            interface: 3,
            alt: 0,
            class: 1,
            subclass: 2,
            endpoints: vec![],
            extra: vec![],
        },
        RawAlt {
            interface: 3,
            alt: 1,
            class: 1,
            subclass: 2,
            endpoints: vec![RawEndpoint {
                address: 0x82,
                attributes: 0x05,
                max_packet: 256,
                extra: vec![7, 0x25, 1, 0, 0, 0, 0],
            }],
            extra,
        },
    ]
}

#[test]
fn audio_alt_parsing_matches_real_card() {
    let alts = audio_alts(&ms2109_like());
    assert_eq!(alts.len(), 1);
    let a = &alts[0];
    assert_eq!(
        (a.interface, a.alt, a.endpoint, a.max_packet),
        (3, 1, 0x82, 256)
    );
    assert_eq!(
        (a.channels, a.subframe, a.bits, a.pcm, a.freq_ctl),
        (1, 2, 16, true, false)
    );
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
        endpoints: vec![RawEndpoint {
            address: 0x82,
            attributes: 5,
            max_packet: 100,
            extra: vec![9],
        }],
        extra: vec![200, 0x24, 2, 1, 1, 2, 16, 3], // length runs past the end
    }];
    let a = audio_alts(&alts);
    assert_eq!(a.len(), 1);
    assert!(choose_audio(&a, 48000, 2).is_none());
}

// ------------------------------------------------------- the mode list

/// One MJPEG format holding `frames` frame descriptors, default frame `default_idx`.
fn mjpeg_format(default_idx: u8, frames: Vec<Vec<u8>>) -> Vec<RawAlt> {
    let mut extra = vec![
        11,
        0x24,
        0x06,
        1,
        frames.len() as u8,
        default_idx,
        0,
        0,
        0,
        0,
        0,
    ];
    for f in frames {
        extra.extend(f);
    }
    vec![RawAlt {
        interface: 1,
        alt: 0,
        class: 14,
        subclass: 2,
        endpoints: vec![],
        extra,
    }]
}

/// A frame descriptor for one size that lists `intervals` as discrete rates.
fn discrete_frame(idx: u8, w: u16, h: u16, default_interval: u32, intervals: &[u32]) -> Vec<u8> {
    let mut d = vec![0u8; 27 + 4 * intervals.len()];
    d[0] = d.len() as u8;
    d[1] = 0x24;
    d[2] = 0x07;
    d[3] = idx;
    d[5..7].copy_from_slice(&w.to_le_bytes());
    d[7..9].copy_from_slice(&h.to_le_bytes());
    d[21..25].copy_from_slice(&default_interval.to_le_bytes());
    d[25] = 2; // discrete intervals
    d[26] = intervals.len() as u8;
    for (i, interval) in intervals.iter().enumerate() {
        let at = 27 + 4 * i;
        d[at..at + 4].copy_from_slice(&interval.to_le_bytes());
    }
    d
}

/// A frame descriptor for one size that can be shot anywhere in a range of rates.
fn range_frame(idx: u8, w: u16, h: u16, lo: u32, hi: u32) -> Vec<u8> {
    let mut d = vec![0u8; 35];
    d[0] = 35;
    d[1] = 0x24;
    d[2] = 0x07;
    d[3] = idx;
    d[5..7].copy_from_slice(&w.to_le_bytes());
    d[7..9].copy_from_slice(&h.to_le_bytes());
    let mid = (lo + hi) / 2;
    d[21..25].copy_from_slice(&mid.to_le_bytes());
    d[25] = 1; // a range
    d[27..31].copy_from_slice(&lo.to_le_bytes());
    d[31..35].copy_from_slice(&hi.to_le_bytes());
    d
}

fn triples(modes: &[VideoMode]) -> Vec<(u32, u32, u32)> {
    modes.iter().map(|m| (m.width, m.height, m.fps)).collect()
}

#[test]
fn every_discrete_rate_of_every_size_is_listed() {
    let alts = mjpeg_format(
        1,
        vec![
            discrete_frame(1, 1920, 1080, 333333, &[333333, 500000, 166666]),
            discrete_frame(2, 1280, 720, 333333, &[333333]),
        ],
    );
    // Biggest picture first, fastest rate first inside a picture.
    assert_eq!(
        triples(&mjpeg_mode_list(&alts)),
        vec![
            (1920, 1080, 60),
            (1920, 1080, 30),
            (1920, 1080, 20),
            (1280, 720, 30),
        ]
    );
}

#[test]
fn the_cards_own_default_is_the_one_it_names() {
    let alts = mjpeg_format(
        2,
        vec![
            discrete_frame(1, 1920, 1080, 333333, &[333333]),
            discrete_frame(2, 1280, 720, 166666, &[166666, 333333]),
        ],
    );
    let modes = mjpeg_mode_list(&alts);
    let defaults: Vec<_> = modes.iter().filter(|m| m.is_default).collect();
    assert_eq!(defaults.len(), 1);
    assert_eq!(
        (defaults[0].width, defaults[0].height, defaults[0].fps),
        (1280, 720, 60)
    );
    // The card's own default is what the card would start on, and it is the mode the
    // camera path picks when the app asks for no size of its own.
    let defaults2 = mjpeg_modes(&alts);
    assert_eq!(default_mode(&defaults2), Some((1280, 720, 60)));
}

#[test]
fn a_rate_the_card_ads_is_the_rate_the_card_is_asked_for() {
    // 333667 in 100 ns units is 29.97 frames a second. libuvc matches a requested rate
    // against a discrete interval with 10_000_000 / interval == fps, exactly, so the
    // only rate that selects this interval is 29 - listing it as 30 would be refused.
    let alts = mjpeg_format(1, vec![discrete_frame(1, 640, 480, 333667, &[333667])]);
    assert_eq!(triples(&mjpeg_mode_list(&alts)), vec![(640, 480, 29)]);
}

#[test]
fn a_rate_range_offers_its_default_and_its_two_ends() {
    let alts = mjpeg_format(1, vec![range_frame(1, 1280, 720, 100000, 500000)]);
    // 100000 = 100 fps, 500000 = 20 fps, and the midpoint 300000 = 33 fps.
    assert_eq!(
        triples(&mjpeg_mode_list(&alts)),
        vec![(1280, 720, 100), (1280, 720, 33), (1280, 720, 20)]
    );
}

#[test]
fn the_same_size_and_rate_twice_is_one_entry() {
    let alts = mjpeg_format(
        1,
        vec![
            discrete_frame(1, 640, 480, 333333, &[333333, 500000]),
            discrete_frame(2, 640, 480, 333333, &[500000, 333333]),
        ],
    );
    assert_eq!(
        triples(&mjpeg_mode_list(&alts)),
        vec![(640, 480, 30), (640, 480, 20)]
    );
}

#[test]
fn no_mjpeg_format_means_no_modes() {
    assert!(mjpeg_mode_list(&[]).is_empty());
    let uncompressed = vec![RawAlt {
        interface: 1,
        alt: 0,
        class: 14,
        subclass: 2,
        endpoints: vec![],
        extra: vec![11, 0x24, 0x04, 1, 1, 1, 0, 0, 0, 0, 0],
    }];
    assert!(mjpeg_mode_list(&uncompressed).is_empty());
}

#[test]
fn a_truncated_interval_list_does_not_panic() {
    // Says three discrete intervals and then stops: the two that are there are real,
    // the third is not invented.
    let mut f = discrete_frame(1, 640, 480, 333333, &[333333, 500000]);
    f[26] = 3; // one more than the descriptor carries
    let alts = mjpeg_format(1, vec![f]);
    assert_eq!(
        triples(&mjpeg_mode_list(&alts)),
        vec![(640, 480, 30), (640, 480, 20)]
    );
}

#[test]
fn a_zero_interval_is_not_a_rate() {
    let mut f = discrete_frame(1, 640, 480, 0, &[0, 333333]);
    f[21..25].copy_from_slice(&0u32.to_le_bytes());
    let alts = mjpeg_format(1, vec![f]);
    assert_eq!(triples(&mjpeg_mode_list(&alts)), vec![(640, 480, 30)]);
}
