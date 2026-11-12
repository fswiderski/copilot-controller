use super::super::{
    Profile,
    image::{
        ImageReportsError, RgbError, diagnostic_tile_rgb, encode_jpeg, prepare_rgb,
        rgb_image_reports,
    },
    layout::diagnostic_layout,
    output::{PacketError, REPORT_PAYLOAD_LEN},
};
use jpeg_decoder::{Decoder, PixelFormat};
use jpeg_encoder::{ColorType, Encoder};
use sha2::{Digest, Sha256};
use std::io::Cursor;

fn pixel(rgb: &[u8], side: usize, x: usize, y: usize) -> [u8; 3] {
    let offset = (y * side + x) * 3;
    rgb[offset..offset + 3].try_into().unwrap()
}

fn set_pixel(rgb: &mut [u8], side: usize, x: usize, y: usize, color: [u8; 3]) {
    let offset = (y * side + x) * 3;
    rgb[offset..offset + 3].copy_from_slice(&color);
}

fn quadrant_image(side: usize) -> Vec<u8> {
    let mut image = vec![0; side * side * 3];
    for y in 0..side {
        for x in 0..side {
            let color = match (x < side / 2, y < side / 2) {
                (true, true) => [240, 20, 20],
                (false, true) => [20, 240, 20],
                (true, false) => [20, 20, 240],
                (false, false) => [235, 235, 20],
            };
            set_pixel(&mut image, side, x, y, color);
        }
    }
    image
}

fn independent_clockwise_rgb(source: &[u8], side: usize) -> Vec<u8> {
    let mut output = vec![0; source.len()];
    for dst_y in 0..side {
        for dst_x in 0..side {
            set_pixel(
                &mut output,
                side,
                dst_x,
                dst_y,
                pixel(source, side, dst_y, side - 1 - dst_x),
            );
        }
    }
    output
}

fn assert_near(actual: [u8; 3], expected: [u8; 3]) {
    for channel in 0..3 {
        assert!(
            (i16::from(actual[channel]) - i16::from(expected[channel])).abs() <= 12,
            "actual {actual:?}, expected {expected:?}"
        );
    }
}

#[test]
fn rgb_rotation_is_clockwise_for_every_pixel_and_original_is_unchanged() {
    let original = quadrant_image(60);
    assert_eq!(prepare_rgb(Profile::Original, &original).unwrap(), original);

    let side = 64;
    let mut source = vec![0; side * side * 3];
    for y in 0..side {
        for x in 0..side {
            set_pixel(&mut source, side, x, y, [x as u8, y as u8, (x ^ y) as u8]);
        }
    }
    set_pixel(&mut source, side, 0, 0, [255, 0, 0]);
    set_pixel(&mut source, side, side - 1, 0, [0, 255, 0]);
    set_pixel(&mut source, side, 0, side - 1, [0, 0, 255]);
    let copy = source.clone();
    let rotated = prepare_rgb(Profile::Rev2, &source).unwrap();
    assert_eq!(source, copy);
    assert_eq!(rotated, independent_clockwise_rgb(&source, side));
    assert_eq!(pixel(&rotated, side, side - 1, 0), [255, 0, 0]);
    assert_eq!(pixel(&rotated, side, side - 1, side - 1), [0, 255, 0]);
    assert_eq!(pixel(&rotated, side, 0, 0), [0, 0, 255]);
}

#[test]
fn jpeg_is_quality_ninety_with_correct_dimensions_and_transported_orientation() {
    for profile in [Profile::Original, Profile::Rev2] {
        let side = usize::from(profile.key_size());
        let source = quadrant_image(side);
        let oriented = if profile == Profile::Rev2 {
            independent_clockwise_rgb(&source, side)
        } else {
            source.clone()
        };
        let mut reference = Vec::new();
        Encoder::new(&mut reference, 90)
            .encode(&oriented, side as u16, side as u16, ColorType::Rgb)
            .unwrap();
        let jpeg = encode_jpeg(profile, &source).unwrap();
        assert_eq!(jpeg, reference);
        assert!(jpeg.starts_with(&[0xff, 0xd8]));
        assert!(jpeg.ends_with(&[0xff, 0xd9]));

        let mut decoder = Decoder::new(Cursor::new(&jpeg));
        let decoded = decoder.decode().unwrap();
        let info = decoder.info().unwrap();
        assert_eq!((info.width, info.height), (side as u16, side as u16));
        assert_eq!(info.pixel_format, PixelFormat::RGB24);
        let top_left = pixel(&decoded, side, side / 4, side / 4);
        let top_right = pixel(&decoded, side, side * 3 / 4, side / 4);
        let bottom_left = pixel(&decoded, side, side / 4, side * 3 / 4);
        let bottom_right = pixel(&decoded, side, side * 3 / 4, side * 3 / 4);
        let (tl, tr, bl, br) = if profile == Profile::Original {
            ([240, 20, 20], [20, 240, 20], [20, 20, 240], [235, 235, 20])
        } else {
            ([20, 20, 240], [240, 20, 20], [235, 235, 20], [20, 240, 20])
        };
        assert_near(top_left, tl);
        assert_near(top_right, tr);
        assert_near(bottom_left, bl);
        assert_near(bottom_right, br);

        let reports = rgb_image_reports(profile, 5, &source).unwrap();
        let announced = u16::from_be_bytes([reports[0][11], reports[0][12]]) as usize;
        assert_eq!(announced, jpeg.len());
        assert_eq!(reports[0][13], 6);
        let mut transported = Vec::new();
        for report in &reports[1..reports.len() - 1] {
            let needed = (announced - transported.len()).min(REPORT_PAYLOAD_LEN);
            transported.extend_from_slice(&report[1..1 + needed]);
        }
        assert_eq!(transported, jpeg);
    }
}

#[test]
fn rgb_size_and_key_validation_precede_encoding() {
    for profile in [Profile::Original, Profile::Rev2] {
        let expected = usize::from(profile.key_size()).pow(2) * 3;
        for actual in [0, expected - 1, expected + 1] {
            assert!(matches!(
                prepare_rgb(profile, &vec![0; actual]),
                Err(RgbError::InvalidLength { profile: p, expected: e, actual: a })
                    if p == profile && e == expected && a == actual
            ));
        }
        let other_size = match profile {
            Profile::Original => 64 * 64 * 3,
            Profile::Rev2 => 60 * 60 * 3,
        };
        assert!(matches!(
            encode_jpeg(profile, &vec![0; other_size]),
            Err(RgbError::InvalidLength { .. })
        ));
        for key in [6, 8, 255] {
            assert!(matches!(
                rgb_image_reports(profile, key, &[]),
                Err(ImageReportsError::Packet(PacketError::InvalidKey { key: k })) if k == key
            ));
            assert_eq!(
                diagnostic_tile_rgb(profile, key),
                Err(PacketError::InvalidKey { key })
            );
        }
        assert!(matches!(
            rgb_image_reports(profile, 0, &[]),
            Err(ImageReportsError::Rgb(RgbError::InvalidLength { .. }))
        ));
    }
}

#[test]
fn all_numbered_logical_tiles_match_independent_full_image_goldens() {
    let cases = [
        (
            Profile::Original,
            [
                "16d55235e0a4168e7260c1b320f4b62bb1c2df1f6a7c961d657e837668af1059",
                "438144a38a149bc9c62500ab380dc0fdd966e926ed3cd59cfbfa9e427d999366",
                "72cafed3f8747bd619155e1a2abd674f50871f05a48b2143597d90777627d3d8",
                "256c0a8c12146ef5d54bc90079abadc2413eeb8c2d209fe1ba49bf33c3cd5a42",
                "62632f56712647a6fcdbaee4f0d392e102f35b40d917ac4d4e28e4b5cf45d4da",
                "a6338df28d1163cdb4ca75153f16e0613eb9985972f16de3a27131bf610f136c",
            ],
        ),
        (
            Profile::Rev2,
            [
                "4879ae3ffab01f354a9ab7becaac4773eda7f1e53413e935b157c742a0364ea4",
                "63ce3d7a9bfc90c02551d1614b09171783db364da4a066a3061463eeae21a501",
                "c212a689c17a667934231d36b252932f433beb8c2b80d06abcbb7b54913f5248",
                "9f190b303fe2378768eb80de700089fc3fef152d8be077da60066dd1319316f1",
                "ea9ecd568d1f7a743228412d9927bf79006a0e86f2be7022119b22b5312886b0",
                "e709c74a9a2e7402bb7bef5d9fee6168f97e7296eef6fef9632f9e643788e4bf",
            ],
        ),
    ];
    for (profile, goldens) in cases {
        for (key, expected) in goldens.into_iter().enumerate() {
            let tile = diagnostic_tile_rgb(profile, key as u8).unwrap();
            assert_eq!(
                format!("{:x}", Sha256::digest(tile)),
                expected,
                "{profile:?} key {key}"
            );
        }
    }
}

#[test]
fn all_six_diagnostic_tiles_survive_the_complete_jpeg_framing_path() {
    let profile = Profile::Rev2;
    let side = usize::from(profile.key_size());
    let backgrounds = [
        [160, 25, 25],
        [25, 145, 25],
        [25, 25, 160],
        [20, 130, 130],
        [140, 20, 140],
        [150, 130, 20],
    ];
    for placement in diagnostic_layout(profile) {
        let tile = diagnostic_tile_rgb(profile, placement.key).unwrap();
        let reports = rgb_image_reports(profile, placement.key, &tile).unwrap();
        let length = usize::from(u16::from_be_bytes([reports[0][11], reports[0][12]]));
        assert_eq!(reports[0][13], placement.number);
        assert_eq!(reports.len(), length.div_ceil(REPORT_PAYLOAD_LEN) + 2);

        let mut jpeg = Vec::with_capacity(length);
        for report in &reports[1..reports.len() - 1] {
            let count = (length - jpeg.len()).min(REPORT_PAYLOAD_LEN);
            jpeg.extend_from_slice(&report[1..1 + count]);
        }
        let mut decoder = Decoder::new(Cursor::new(jpeg));
        let decoded = decoder.decode().unwrap();
        let info = decoder.info().unwrap();
        assert_eq!((info.width, info.height), (64, 64));
        assert_near(
            pixel(&decoded, side, side - 5, 5),
            backgrounds[usize::from(placement.key)],
        );
    }
}

#[test]
fn diagnostic_strip_arrow_and_digit_have_inclusive_boundaries() {
    for profile in [Profile::Original, Profile::Rev2] {
        let side = usize::from(profile.key_size());
        let center = side / 2;
        let rgb = diagnostic_tile_rgb(profile, 0).unwrap();
        let bg = [160, 25, 25];
        let black = [0, 0, 0];
        let white = [255, 255, 255];
        assert_eq!(pixel(&rgb, side, 16, 3), bg);
        assert_eq!(pixel(&rgb, side, 17, 2), bg);
        assert_eq!(pixel(&rgb, side, 17, 3), black);
        assert_eq!(pixel(&rgb, side, side - 18, side - 4), black);
        assert_eq!(pixel(&rgb, side, side - 17, side - 4), bg);
        assert_eq!(pixel(&rgb, side, 17, side - 3), bg);
        assert_eq!(pixel(&rgb, side, center, 5), white);
        assert_eq!(pixel(&rgb, side, center - 1, 5), black);
        assert_eq!(pixel(&rgb, side, center - 7, 12), white);
        assert_eq!(pixel(&rgb, side, center - 8, 12), black);
        assert_eq!(pixel(&rgb, side, center, 19), white);
        assert_eq!(pixel(&rgb, side, center, 20), black);
        assert_eq!(pixel(&rgb, side, center - 7, 26), black);
        assert_eq!(pixel(&rgb, side, center - 2, 26), white);
        assert_eq!(pixel(&rgb, side, center + 3, 26), black);
        assert_eq!(pixel(&rgb, side, center - 7, 31), white);
        assert_eq!(pixel(&rgb, side, center + 7, 50), white);
        assert_eq!(pixel(&rgb, side, center, 51), black);
    }
}
