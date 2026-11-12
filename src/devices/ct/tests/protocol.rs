use std::collections::HashSet;

use super::super::controls::{
    MappingEvidence, PhysicalEventKind, ProfileControlKind, RAW_CONTROLS, RawControlId,
    RecommendedIntent, TOUCH_AREAS, name_input, normalize, profile_controls, touch_area,
};
use super::super::protocol::{
    ButtonState, FrameDecoder, FrameError, InputError, InputEvent, Touch, TouchFamily, TouchPhase,
    decode_input,
};
use super::super::{LiveSupportError, Slot, require_live_support};

fn collect_chunk(decoder: &mut FrameDecoder, mut chunk: &[u8], output: &mut Vec<Vec<u8>>) {
    while !chunk.is_empty() {
        let step = decoder.decode(chunk).unwrap();
        assert!(step.consumed > 0);
        chunk = &chunk[step.consumed..];
        if let Some(frame) = step.frame {
            output.push(frame);
        }
    }
}

fn id(value: u8) -> RawControlId {
    RawControlId::new(value).unwrap()
}

#[test]
fn frames_split_at_every_boundary_and_require_explicit_suffix() {
    let frames: [&[u8]; 4] = [
        &[0x82, 5, 5, 0, 0, 7, 0],
        &[0x82, 4, 4, 0, 0, 0xba],
        &[0x82, 5, 5, 1, 42, 0, 0xff],
        &[0x82, 9, 9, 0x4d, 0, 0, 0, 0x73, 0, 0xe2, 0x13],
    ];
    let bytes: Vec<u8> = frames
        .iter()
        .flat_map(|frame| frame.iter().copied())
        .collect();
    let expected: Vec<Vec<u8>> = frames.iter().map(|frame| frame[2..].to_vec()).collect();
    for split in 0..=bytes.len() {
        let mut decoder = FrameDecoder::default();
        assert_eq!(decoder.decode(&[]).unwrap().consumed, 0);
        let mut got = Vec::new();
        collect_chunk(&mut decoder, &bytes[..split], &mut got);
        collect_chunk(&mut decoder, &bytes[split..], &mut got);
        assert_eq!(got, expected, "split {split}");
        decoder.eof().unwrap();
    }
    let mut decoder = FrameDecoder::default();
    let first = decoder.decode(&bytes).unwrap();
    assert_eq!(first.consumed, frames[0].len());
    assert_eq!(first.frame, Some(expected[0].clone()));
    let second = decoder.decode(&bytes[first.consumed..]).unwrap();
    assert_eq!(second.frame, Some(expected[1].clone()));

    let mut largest = vec![0x82, 255];
    largest.extend(std::iter::repeat_n(0x82, 255));
    let mut decoder = FrameDecoder::new(255).unwrap();
    for chunk in largest.chunks(1) {
        let step = decoder.decode(chunk).unwrap();
        if let Some(payload) = step.frame {
            assert_eq!(payload, vec![0x82; 255]);
        }
    }
    decoder.eof().unwrap();
}

#[test]
fn framing_faults_eof_and_reset_never_replay_a_prefix() {
    assert_eq!(
        FrameDecoder::new(2).unwrap_err(),
        FrameError::InvalidMaximum(2)
    );
    assert_eq!(
        FrameDecoder::new(256).unwrap_err(),
        FrameError::InvalidMaximum(256)
    );
    let mut minimal = FrameDecoder::new(3).unwrap();
    assert_eq!(
        minimal.decode(&[0x82, 3, 3, 0, 0]).unwrap().frame,
        Some(vec![3, 0, 0])
    );
    assert_eq!(
        minimal.decode(&[0x82, 4]),
        Err(FrameError::InvalidLength {
            length: 4,
            maximum: 3
        })
    );
    let invalid = [
        (
            vec![0x88, 0x82, 3, 3, 0, 0],
            FrameError::UnexpectedMarker(0x88),
        ),
        (
            vec![0x82, 0],
            FrameError::InvalidLength {
                length: 0,
                maximum: 8,
            },
        ),
        (
            vec![0x82, 1],
            FrameError::InvalidLength {
                length: 1,
                maximum: 8,
            },
        ),
        (
            vec![0x82, 2],
            FrameError::InvalidLength {
                length: 2,
                maximum: 8,
            },
        ),
        (
            vec![0x82, 9],
            FrameError::InvalidLength {
                length: 9,
                maximum: 8,
            },
        ),
    ];
    for (bytes, expected) in invalid {
        let mut decoder = FrameDecoder::new(8).unwrap();
        assert_eq!(decoder.decode(&bytes), Err(expected));
        assert!(decoder.is_faulted());
        assert_eq!(decoder.decode(&[]), Err(FrameError::Faulted));
        assert_eq!(
            decoder.decode(&[0x82, 3, 3, 0, 0]),
            Err(FrameError::Faulted)
        );
        assert_eq!(decoder.eof(), Err(FrameError::Faulted));
        decoder.reset();
        assert!(!decoder.is_faulted());
        assert_eq!(
            decoder.decode(&[0x82, 3, 3, 0, 0]).unwrap().frame,
            Some(vec![3, 0, 0])
        );
        decoder.eof().unwrap();
    }

    let frame = [0x82, 5, 5, 0, 0, 7, 0];
    for end in 1..frame.len() {
        let mut decoder = FrameDecoder::default();
        assert!(decoder.decode(&frame[..end]).unwrap().frame.is_none());
        assert_eq!(
            decoder.eof(),
            Err(FrameError::TruncatedFrame),
            "prefix {end}"
        );
        assert_eq!(decoder.eof(), Err(FrameError::Faulted));
    }
    let mut decoder = FrameDecoder::default();
    decoder.decode(&frame[..3]).unwrap();
    assert_eq!(decoder.decode(&[]).unwrap().consumed, 0);
    decoder.reset();
    assert_eq!(
        decoder.decode(&frame[3..]),
        Err(FrameError::UnexpectedMarker(0))
    );
    decoder.reset();
    assert_eq!(
        decoder.decode(&frame).unwrap().frame,
        Some(frame[2..].to_vec())
    );
    let mut partial = FrameDecoder::default();
    partial.decode(&[0x82, 5, 65, 66, 67]).unwrap();
    assert!(!format!("{partial:?}").contains("65, 66, 67"));
}

#[test]
fn input_vectors_are_exact_and_keep_raw_touch_data() {
    let vectors: &[(&[u8], InputEvent)] = &[
        (
            &[5, 0, 0, 7, 0],
            InputEvent::Button {
                transaction: 0,
                control: id(7),
                state: ButtonState::Down,
            },
        ),
        (
            &[5, 0, 0, 7, 1],
            InputEvent::Button {
                transaction: 0,
                control: id(7),
                state: ButtonState::Up,
            },
        ),
        (
            &[4, 0, 0, 0xba],
            InputEvent::Tick {
                transaction: 0,
                value: 0xba,
            },
        ),
        (
            &[5, 1, 42, 0, 0xff],
            InputEvent::Rotation {
                transaction: 42,
                control: id(0),
                delta: -1,
            },
        ),
        (
            &[9, 0x4d, 0, 0, 0, 115, 0, 226, 19],
            InputEvent::Touch(Touch {
                transaction: 0,
                family: TouchFamily::Standard,
                phase: TouchPhase::Contact,
                unknown: 0,
                x: 115,
                y: 226,
                contact_id: 19,
            }),
        ),
        (
            &[9, 0x6d, 0, 0, 1, 0xbf, 0, 0x4c, 18],
            InputEvent::Touch(Touch {
                transaction: 0,
                family: TouchFamily::Standard,
                phase: TouchPhase::End,
                unknown: 0,
                x: 447,
                y: 76,
                contact_id: 18,
            }),
        ),
        (
            &[9, 0x52, 0, 0, 0, 150, 0, 142, 0],
            InputEvent::Touch(Touch {
                transaction: 0,
                family: TouchFamily::Ct,
                phase: TouchPhase::Contact,
                unknown: 0,
                x: 150,
                y: 142,
                contact_id: 0,
            }),
        ),
        (
            &[9, 0x72, 0x23, 0x91, 0, 150, 0, 142, 0],
            InputEvent::Touch(Touch {
                transaction: 0x23,
                family: TouchFamily::Ct,
                phase: TouchPhase::End,
                unknown: 0x91,
                x: 150,
                y: 142,
                contact_id: 0,
            }),
        ),
    ];
    for (bytes, expected) in vectors {
        assert_eq!(decode_input(bytes).unwrap(), *expected);
    }
    let off_screen = decode_input(&[9, 0x4d, 6, 9, 0xff, 0xff, 0xff, 0xff, 1]).unwrap();
    assert!(matches!(
        off_screen,
        InputEvent::Touch(Touch {
            x: 65_535,
            y: 65_535,
            unknown: 9,
            ..
        })
    ));
    assert!(name_input(&off_screen).is_none());
    assert_eq!(decode_input(&[]), Err(InputError::TooShort));
    assert!(matches!(
        decode_input(&[4, 0, 0, 7, 0]),
        Err(InputError::LengthMismatch { .. })
    ));
    assert!(matches!(
        decode_input(&[3, 0, 0]),
        Err(InputError::InvalidMessageLength { .. })
    ));
    assert_eq!(
        decode_input(&[5, 0, 0, 7, 2]),
        Err(InputError::InvalidButtonState(2))
    );
    assert_eq!(
        decode_input(&[5, 0, 0, 0x1b, 0]),
        Err(InputError::InvalidButton(0x1b))
    );
    assert_eq!(
        decode_input(&[5, 1, 0, 7, 1]),
        Err(InputError::InvalidRotation(7))
    );
    assert_eq!(
        decode_input(&[3, 7, 0]),
        Err(InputError::UnsupportedOpcode(7))
    );
    assert!(matches!(
        decode_input(&[8, 0x52, 0, 0, 0, 0, 0, 0]),
        Err(InputError::InvalidMessageLength { .. })
    ));
}

#[test]
fn touch_naming_requires_the_correct_family_contact_and_rectangle() {
    let mut touch = Touch {
        transaction: 31,
        family: TouchFamily::Standard,
        phase: TouchPhase::Contact,
        unknown: 44,
        x: 0,
        y: 0,
        contact_id: 1,
    };
    for (x, y, expected) in [
        (60, 0, Some("key_00")),
        (419, 269, Some("key_11")),
        (420, 0, Some("strip_right")),
        (0, 269, Some("strip_left")),
        (115, 226, Some("key_08")),
        (447, 76, Some("strip_right")),
        (480, 0, None),
        (100, 270, None),
    ] {
        touch.x = x;
        touch.y = y;
        assert_eq!(touch_area(&touch).map(|area| area.name), expected);
    }
    touch.x = 60;
    touch.y = 0;
    touch.contact_id = 0;
    assert!(touch_area(&touch).is_none());
    touch.family = TouchFamily::Ct;
    touch.contact_id = 5;
    assert!(touch_area(&touch).is_none());
    touch.contact_id = 0;
    touch.x = 239;
    touch.y = 239;
    assert_eq!(touch_area(&touch).unwrap().name, "wheel_screen");
    touch.x = 240;
    touch.y = 0;
    assert!(touch_area(&touch).is_none());
    touch.x = 0;
    touch.y = 240;
    assert!(touch_area(&touch).is_none());
}

#[test]
fn all_touch_regions_partition_their_qualified_coordinate_spaces() {
    let mut touch = Touch {
        transaction: 0,
        family: TouchFamily::Standard,
        phase: TouchPhase::End,
        unknown: 0,
        x: 0,
        y: 0,
        contact_id: 1,
    };
    for area in TOUCH_AREAS {
        touch.family = area.family;
        touch.contact_id = if area.family == TouchFamily::Standard {
            1
        } else {
            0
        };
        for (x, y) in [
            (area.rect.x, area.rect.y),
            (
                area.rect.x + area.rect.width - 1,
                area.rect.y + area.rect.height - 1,
            ),
        ] {
            touch.x = x;
            touch.y = y;
            assert_eq!(
                touch_area(&touch).map(|mapped| mapped.name),
                Some(area.name)
            );
        }
    }
    for (family, width, height) in [
        (TouchFamily::Standard, 480, 270),
        (TouchFamily::Ct, 240, 240),
    ] {
        for y in 0..height {
            for x in 0..width {
                let count = TOUCH_AREAS
                    .iter()
                    .filter(|area| area.family == family && area.rect.contains(x, y))
                    .count();
                assert_eq!(count, 1, "{family:?} ({x},{y})");
            }
        }
    }
}

#[test]
fn control_map_profile_and_conversion_are_not_gesture_authority() {
    let raw_names: HashSet<_> = RAW_CONTROLS.iter().map(|control| control.name).collect();
    let touch_names: HashSet<_> = TOUCH_AREAS.iter().map(|area| area.name).collect();
    assert_eq!(raw_names.len(), 27);
    assert_eq!(touch_names.len(), 15);
    assert!(raw_names.is_disjoint(&touch_names));
    for (index, control) in RAW_CONTROLS.iter().enumerate() {
        assert_eq!(usize::from(control.id.value()), index);
        assert!(control.capabilities.press);
        assert_eq!(control.capabilities.rotate, index < 7);
        assert_eq!(control.capabilities.rgb_led, (7..=14).contains(&index));
        assert!(!control.capabilities.touch && !control.capabilities.display);
    }
    for area in TOUCH_AREAS {
        assert!(area.capabilities.touch && area.capabilities.display);
        assert!(
            !area.capabilities.press && !area.capabilities.rotate && !area.capabilities.rgb_led
        );
    }
    assert_eq!(
        RAW_CONTROLS[7].mapping_evidence,
        MappingEvidence::SingleCapturedPressRelease
    );
    assert_eq!(RAW_CONTROLS[15].mapping_evidence, MappingEvidence::Inferred);
    assert_eq!(
        RAW_CONTROLS[8].mapping_evidence,
        MappingEvidence::SourceReported
    );
    for control in RAW_CONTROLS {
        for (state, expected) in [
            (0, PhysicalEventKind::Press),
            (1, PhysicalEventKind::Release),
        ] {
            let event = decode_input(&[5, 0, 0, control.id.value(), state]).unwrap();
            assert_eq!(name_input(&event).unwrap().name(), control.name);
            let normalized = normalize(&event, 5, ()).unwrap();
            assert_eq!(
                (normalized.control, normalized.kind),
                (control.name, expected)
            );
        }
        if control.capabilities.rotate {
            for (wire, delta) in [(0xff, -1), (1, 1)] {
                let event = decode_input(&[5, 1, 0, control.id.value(), wire]).unwrap();
                assert_eq!(name_input(&event).unwrap().name(), control.name);
                let normalized = normalize(&event, 5, ()).unwrap();
                assert_eq!(normalized.control, format!("{}_turn", control.name));
                assert_eq!(normalized.kind, PhysicalEventKind::Turn(delta));
            }
        }
    }
    let impossible_rotation = InputEvent::Rotation {
        transaction: 0,
        control: id(7),
        delta: 1,
    };
    assert!(name_input(&impossible_rotation).is_none());
    assert!(normalize(&impossible_rotation, 1, ()).is_none());

    let profile = profile_controls();
    assert_eq!(profile.len(), 49);
    assert_eq!(
        profile
            .iter()
            .map(|item| item.id)
            .collect::<HashSet<_>>()
            .len(),
        49
    );
    assert!(profile.iter().all(|control| {
        control.role_geometry.near_selection.is_none()
            && control.role_geometry.isolated_stop.is_none()
            && control.role_geometry.adjacent_controls.is_none()
    }));
    for (key, slot) in [
        ("key_00", 1),
        ("key_01", 2),
        ("key_04", 3),
        ("key_05", 4),
        ("key_08", 5),
        ("key_09", 6),
    ] {
        let control = profile.iter().find(|item| item.id == key).unwrap();
        assert_eq!(
            control.recommendation,
            Some(RecommendedIntent::SelectSlot(Slot::new(slot).unwrap()))
        );
        assert_eq!(control.kind, ProfileControlKind::Press);
    }
    for (name, kind, recommendation) in [
        ("strip_left", ProfileControlKind::TouchSlide, None),
        (
            "wheel_screen",
            ProfileControlKind::Press,
            Some(RecommendedIntent::CommitCursor),
        ),
        (
            "round_1",
            ProfileControlKind::Press,
            Some(RecommendedIntent::Stop),
        ),
        (
            "wheel_turn",
            ProfileControlKind::Wheel,
            Some(RecommendedIntent::MoveCursor),
        ),
    ] {
        let control = profile.iter().find(|item| item.id == name).unwrap();
        assert_eq!(
            (control.kind, control.recommendation),
            (kind, recommendation)
        );
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Sample {
        source: &'static str,
        epoch: u64,
        ticks: u64,
        frequency: u64,
    }
    let event = decode_input(&[5, 0, 77, 7, 0]).unwrap();
    assert_eq!(name_input(&event).unwrap().name(), "round_1");
    let sample = Sample {
        source: "synthetic",
        epoch: 5,
        ticks: 91,
        frequency: 1000,
    };
    let normalized = normalize(&event, 19, sample).unwrap();
    assert_eq!(normalized.control, "round_1");
    assert_eq!(normalized.kind, PhysicalEventKind::Press);
    assert_eq!(normalized.connection_generation, 19);
    assert_eq!(
        normalized.at,
        Sample {
            source: "synthetic",
            epoch: 5,
            ticks: 91,
            frequency: 1000
        }
    );

    for (byte, expected_delta, admitted) in [
        (0x80, -128, false),
        (0x7f, 127, false),
        (0, 0, false),
        (0xff, -1, true),
        (1, 1, true),
    ] {
        let raw = decode_input(&[5, 1, 0, 0, byte]).unwrap();
        assert!(matches!(raw, InputEvent::Rotation { delta, .. } if delta == expected_delta));
        let normalized = normalize(&raw, 1, ());
        assert_eq!(normalized.is_some(), admitted);
        if let Some(normalized) = normalized {
            assert_eq!(normalized.control, "wheel_turn");
            assert_eq!(normalized.kind, PhysicalEventKind::Turn(expected_delta));
        }
    }
    let touch = decode_input(&[9, 0x4d, 0, 0, 0, 60, 0, 0, 1]).unwrap();
    assert_eq!(name_input(&touch).unwrap().name(), "key_00");
    assert!(normalize(&touch, 1, ()).is_none());
    let tick = InputEvent::Tick {
        transaction: 0,
        value: 0,
    };
    assert!(name_input(&tick).is_none());
    assert!(normalize(&tick, 1, ()).is_none());
    assert_eq!(
        require_live_support(0x2ec2, 3),
        Err(LiveSupportError::HardwareUnqualified)
    );
    assert_eq!(
        require_live_support(0x1234, 3),
        Err(LiveSupportError::UnsupportedIdentity)
    );
}
