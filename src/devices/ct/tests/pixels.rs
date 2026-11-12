use super::super::pixels::{
    Display, Framebuffer, MAX_PIXEL_BYTES, PixelError, Placement, REGIONS, Region, Rgb,
    decode_rgb565, display_preview, encode_rgb565, overview, prepare_draw, prepare_refresh,
};

fn pixel(image: &super::super::pixels::Image, x: usize, y: usize) -> Rgb {
    image.pixels[y * usize::from(image.width) + x]
}

#[test]
fn rgb565_vectors_byte_order_quantization_and_full_center() {
    let colors = [
        Rgb::new(255, 0, 0),
        Rgb::new(0, 255, 0),
        Rgb::new(0, 0, 255),
        Rgb::new(255, 255, 255),
        Rgb::new(0, 0, 0),
        Rgb::new(0xab, 0xcd, 0xef),
    ];
    let little = [
        0x00, 0xf8, 0xe0, 0x07, 0x1f, 0x00, 0xff, 0xff, 0, 0, 0x7d, 0xae,
    ];
    let big = [
        0xf8, 0x00, 0x07, 0xe0, 0x00, 0x1f, 0xff, 0xff, 0, 0, 0xae, 0x7d,
    ];
    for display in [
        Display::Center,
        Display::Left,
        Display::Right,
        Display::Wheel,
    ] {
        let expected = if display == Display::Wheel {
            &big
        } else {
            &little
        };
        assert_eq!(encode_rgb565(display, 3, 2, &colors).unwrap(), *expected);
        let decoded = decode_rgb565(display, 3, 2, expected).unwrap();
        assert_eq!(&decoded[..5], &colors[..5]);
        assert_eq!(decoded[5], Rgb::new(173, 207, 239));
    }
    assert_eq!(Rgb::new(7, 3, 7).rgb565(), 0);
    assert_eq!(Rgb::new(8, 4, 8).rgb565(), 0x0821);
    assert_eq!(Rgb::new(0xab, 0xcd, 0xef).rgb565(), 0xae7d);
    let pixels = vec![Rgb::new(255, 0, 0); 360 * 270];
    let bytes = encode_rgb565(Display::Center, 360, 270, &pixels).unwrap();
    assert_eq!(bytes.len(), MAX_PIXEL_BYTES);
    assert!(
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .all(|pair| *pair == [0, 0xf8])
    );
}

#[test]
fn dimensions_counts_and_placements_fail_closed() {
    let one = [Rgb::new(1, 2, 3)];
    for (display, width, height) in [
        (Display::Center, 0, 1),
        (Display::Center, 1, 0),
        (Display::Center, 361, 1),
        (Display::Center, 1, 271),
        (Display::Left, 61, 1),
        (Display::Right, 1, 271),
        (Display::Wheel, 241, 1),
    ] {
        assert!(matches!(
            encode_rgb565(display, width, height, &one),
            Err(PixelError::InvalidDimensions { .. })
        ));
    }
    assert_eq!(
        encode_rgb565(Display::Center, 2, 1, &one),
        Err(PixelError::PixelCount {
            expected: 2,
            actual: 1
        })
    );
    assert_eq!(
        encode_rgb565(Display::Center, 1, 1, &[one[0], one[0]]),
        Err(PixelError::PixelCount {
            expected: 1,
            actual: 2
        })
    );
    assert_eq!(
        decode_rgb565(Display::Wheel, 1, 1, &[0]),
        Err(PixelError::ByteCount {
            expected: 2,
            actual: 1
        })
    );
    assert_eq!(
        decode_rgb565(Display::Wheel, 1, 1, &[0, 0, 0]),
        Err(PixelError::ByteCount {
            expected: 2,
            actual: 3
        })
    );
    assert!(Placement::new(Display::Center, 359, 269, 1, 1).is_ok());
    assert_eq!(
        Placement::new(Display::Center, 359, 269, 2, 1),
        Err(PixelError::InvalidPlacement)
    );
    assert!(Placement::new(Display::Center, u16::MAX, 0, 1, 1).is_err());
    assert_eq!(Region::key(12), Err(PixelError::InvalidKey(12)));
    assert!(Framebuffer::from_rgb565(Region::key(0).unwrap(), 1, vec![0; 16_199]).is_err());
}

#[test]
fn framebuffers_and_command_bodies_retain_region_and_generation() {
    assert_eq!(REGIONS.len(), 15);
    for (index, region) in REGIONS[..12].iter().copied().enumerate() {
        let p = region.placement();
        assert_eq!(p.display(), Display::Center);
        assert_eq!(
            (p.x(), p.y(), p.width(), p.height()),
            ((index % 4) as u16 * 90, (index / 4) as u16 * 90, 90, 90)
        );
    }
    let key = Region::key(11).unwrap();
    let key_frame =
        Framebuffer::from_rgb(key, 0x1234, &vec![Rgb::new(255, 0, 0); 90 * 90]).unwrap();
    assert_eq!(key_frame.region(), key);
    assert_eq!(key_frame.generation(), 0x1234);
    assert_eq!(key_frame.bytes().len(), 16_200);
    let preview = key_frame.preview().unwrap();
    assert_eq!((preview.width, preview.height), (90, 90));
    assert!(
        preview
            .pixels
            .iter()
            .all(|pixel| *pixel == Rgb::new(255, 0, 0))
    );
    let command = key_frame.draw_command().unwrap();
    assert_eq!(command.opcode, 0x10);
    assert_eq!(command.generation, 0x1234);
    assert_eq!(&command.body[..10], &[0, b'A', 1, 14, 0, 180, 0, 90, 0, 90]);
    assert_eq!(&command.body[10..], key_frame.bytes());

    for (region, count, display) in [
        (Region::LeftStrip, 32_400, Display::Left),
        (Region::RightStrip, 32_400, Display::Right),
        (Region::Wheel, 115_200, Display::Wheel),
    ] {
        let frame = Framebuffer::from_rgb565(region, 9, vec![0; count]).unwrap();
        assert_eq!(frame.bytes().len(), count);
        assert_eq!(frame.draw_command().unwrap().body[1], display.id());
    }
    for display in [
        Display::Center,
        Display::Left,
        Display::Right,
        Display::Wheel,
    ] {
        let refresh = prepare_refresh(display, 77);
        assert_eq!(
            (refresh.opcode, refresh.body, refresh.generation),
            (0x0f, vec![0, display.id()], 77)
        );
    }
    let p = Placement::new(Display::Center, 0, 0, 1, 1).unwrap();
    assert_eq!(
        prepare_draw(p, &[0], 1),
        Err(PixelError::ByteCount {
            expected: 2,
            actual: 1
        })
    );
}

#[test]
fn previews_preserve_placement_order_and_nearest_neighbor_scale() {
    let key = Region::key(0).unwrap();
    let red = Framebuffer::from_rgb(key, 1, &vec![Rgb::new(255, 0, 0); 90 * 90]).unwrap();
    let blue = Framebuffer::from_rgb(key, 2, &vec![Rgb::new(0, 0, 255); 90 * 90]).unwrap();
    let wheel =
        Framebuffer::from_rgb(Region::Wheel, 3, &vec![Rgb::new(0, 255, 0); 240 * 240]).unwrap();
    let preview = overview(&[red.clone(), blue.clone(), wheel], 2).unwrap();
    assert_eq!((preview.width, preview.height), (960, 1052));
    assert_eq!(pixel(&preview, 0, 0), Rgb::new(64, 64, 64));
    assert_eq!(pixel(&preview, 120, 0), Rgb::new(0, 0, 255));
    assert_eq!(pixel(&preview, 121, 1), Rgb::new(0, 0, 255));
    assert_eq!(pixel(&preview, 2 * 120, 2 * 286), Rgb::new(0, 255, 0));
    assert_eq!(pixel(&preview, 2 * 120, 2 * 285), Rgb::new(64, 64, 64));
    assert!(matches!(overview(&[], 0), Err(PixelError::InvalidScale(0))));
    assert!(matches!(overview(&[], 5), Err(PixelError::InvalidScale(5))));

    let center = display_preview(Display::Center, &[red, blue], Rgb::new(3, 4, 5)).unwrap();
    assert_eq!((center.width, center.height), (360, 270));
    assert_eq!(pixel(&center, 0, 0), Rgb::new(0, 0, 255));
    assert_eq!(pixel(&center, 90, 0), Rgb::new(3, 4, 5));
}
