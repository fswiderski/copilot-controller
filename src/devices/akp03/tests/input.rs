use super::super::{
    Profile,
    input::{
        DecodeError, DecodedInput, HexError, HexReportError, decode_hex_report, decode_report,
        parse_hex_report,
    },
};
use serde_json::json;

fn report(code: u8, state: u8, ok_prefix: bool) -> Vec<u8> {
    let mut report = vec![0; 11];
    report[..3].copy_from_slice(b"ACK");
    if ok_prefix {
        report[5..7].copy_from_slice(b"OK");
    }
    report[9] = code;
    report[10] = state;
    report
}

#[test]
fn published_decode_vectors_and_padding() {
    use DecodedInput::{
        Acknowledgment, ButtonPressObserved, ButtonState, EncoderPressObserved, EncoderState,
        EncoderTurn,
    };
    let cases = [
        (
            "41434b00000000000001",
            Profile::Original,
            ButtonPressObserved { index: 0 },
        ),
        (
            "41434B00000000000091",
            Profile::Original,
            EncoderTurn { index: 0, delta: 1 },
        ),
        ("41434b00004f4b00000101", Profile::Original, Acknowledgment),
        (
            "41434b00004f4b00000101",
            Profile::Rev2,
            ButtonState {
                index: 0,
                pressed: true,
            },
        ),
        (
            "41434b00004f4b00000100",
            Profile::Rev2,
            ButtonState {
                index: 0,
                pressed: false,
            },
        ),
        (
            "41434b00004f4b00003501",
            Profile::Rev2,
            EncoderState {
                index: 1,
                pressed: true,
            },
        ),
        (
            "41434b00004f4b00003300",
            Profile::Rev2,
            EncoderState {
                index: 0,
                pressed: false,
            },
        ),
        (
            "41434b00004f4b00003401",
            Profile::Rev2,
            EncoderState {
                index: 2,
                pressed: true,
            },
        ),
        ("41434b00004f4b00000000", Profile::Rev2, Acknowledgment),
        (
            "41434b00000000000033",
            Profile::Original,
            EncoderPressObserved { index: 0 },
        ),
    ];
    for (hex, profile, expected) in cases {
        assert_eq!(decode_hex_report(profile, hex), Ok(expected), "{hex}");
        let mut padded = parse_hex_report(hex).unwrap();
        padded.resize(512, 0);
        assert_eq!(decode_report(profile, &padded), Ok(expected), "{hex}");
    }
    assert_eq!(
        decode_hex_report(Profile::Rev2, "41434b00004f4b0000ff00"),
        Err(HexReportError::Report(DecodeError::UnknownControl {
            code: 0xff
        }))
    );
}

#[test]
fn acknowledgment_precedence_is_profile_specific() {
    for code in [0x00, 0x01, 0x33, 0x91, 0xff] {
        assert_eq!(
            decode_report(Profile::Original, &report(code, 0xff, true)),
            Ok(DecodedInput::Acknowledgment)
        );
    }
    assert_eq!(
        decode_report(Profile::Rev2, &report(0, 0xff, true)),
        Ok(DecodedInput::Acknowledgment)
    );
    assert_eq!(
        decode_report(Profile::Rev2, &report(0x01, 1, true)),
        Ok(DecodedInput::ButtonState {
            index: 0,
            pressed: true
        })
    );
    assert_eq!(
        decode_report(Profile::Rev2, &report(0x91, 0xff, true)),
        Ok(DecodedInput::EncoderTurn { index: 0, delta: 1 })
    );
    assert_eq!(
        decode_report(Profile::Rev2, &report(0xff, 0, true)),
        Err(DecodeError::UnknownControl { code: 0xff })
    );
    for profile in [Profile::Original, Profile::Rev2] {
        assert_eq!(
            decode_report(profile, &report(0, 0xff, false)),
            Ok(DecodedInput::NoControl)
        );
    }
    let mut almost_ok = report(1, 1, true);
    almost_ok[3] = 9;
    assert_eq!(
        decode_report(Profile::Original, &almost_ok),
        Ok(DecodedInput::ButtonPressObserved { index: 0 })
    );
}

#[test]
fn every_button_push_and_turn_code_is_mapped_without_toggles() {
    let buttons = [
        (0x01, 0),
        (0x02, 1),
        (0x03, 2),
        (0x04, 3),
        (0x05, 4),
        (0x06, 5),
        (0x25, 6),
        (0x30, 7),
        (0x31, 8),
    ];
    let encoders = [(0x33, 0), (0x35, 1), (0x34, 2)];
    for (code, index) in buttons {
        for state in [0, 1, 2, 255] {
            let input = report(code, state, false);
            assert_eq!(
                decode_report(Profile::Original, &input),
                Ok(DecodedInput::ButtonPressObserved { index })
            );
            assert_eq!(
                decode_report(Profile::Original, &input),
                Ok(DecodedInput::ButtonPressObserved { index })
            );
        }
        for (state, pressed) in [(0, false), (1, true)] {
            assert_eq!(
                decode_report(Profile::Rev2, &report(code, state, true)),
                Ok(DecodedInput::ButtonState { index, pressed })
            );
        }
    }
    for (code, index) in encoders {
        assert_eq!(
            decode_report(Profile::Original, &report(code, 0, false)),
            Ok(DecodedInput::EncoderPressObserved { index })
        );
        for (state, pressed) in [(0, false), (1, true)] {
            assert_eq!(
                decode_report(Profile::Rev2, &report(code, state, true)),
                Ok(DecodedInput::EncoderState { index, pressed })
            );
        }
    }

    for (code, index, delta) in [
        (0x90, 0, -1),
        (0x91, 0, 1),
        (0x50, 1, -1),
        (0x51, 1, 1),
        (0x60, 2, -1),
        (0x61, 2, 1),
    ] {
        assert_eq!(
            decode_report(Profile::Original, &report(code, 0xff, false)),
            Ok(DecodedInput::EncoderTurn { index, delta })
        );
        assert_eq!(
            decode_report(Profile::Rev2, &report(code, 0xff, true)),
            Ok(DecodedInput::EncoderTurn { index, delta })
        );
    }
}

#[test]
fn all_unassigned_control_codes_fail_except_original_acknowledgments() {
    for code in u8::MIN..=u8::MAX {
        if matches!(
            code,
            0..=6
                | 0x25
                | 0x30
                | 0x31
                | 0x33
                | 0x34
                | 0x35
                | 0x50
                | 0x51
                | 0x60
                | 0x61
                | 0x90
                | 0x91
        ) {
            continue;
        }
        assert_eq!(
            decode_report(Profile::Original, &report(code, 2, false)),
            Err(DecodeError::UnknownControl { code })
        );
        assert_eq!(
            decode_report(Profile::Rev2, &report(code, 2, true)),
            Err(DecodeError::UnknownControl { code })
        );
        assert_eq!(
            decode_report(Profile::Original, &report(code, 2, true)),
            Ok(DecodedInput::Acknowledgment)
        );
    }
}

#[test]
fn report_lengths_headers_and_states_are_strict() {
    for profile in [Profile::Original, Profile::Rev2] {
        for length in 0..=513 {
            let mut input = vec![0; length];
            if length >= 3 {
                input[..3].copy_from_slice(b"ACK");
            }
            if (profile.minimum_input_report_len()..=512).contains(&length) {
                assert_eq!(decode_report(profile, &input), Ok(DecodedInput::NoControl));
            } else {
                assert_eq!(
                    decode_report(profile, &input),
                    Err(DecodeError::InvalidLength {
                        profile,
                        actual: length
                    })
                );
            }
        }
        assert!(matches!(
            decode_report(profile, &vec![0; 1024]),
            Err(DecodeError::InvalidLength { actual: 1024, .. })
        ));
        let mut wrong = report(1, 1, false);
        wrong[0] = 0;
        assert_eq!(
            decode_report(profile, &wrong),
            Err(DecodeError::InvalidHeader)
        );
        let mut leading_id = vec![0];
        leading_id.extend_from_slice(&report(1, 1, false));
        assert_eq!(
            decode_report(profile, &leading_id),
            Err(DecodeError::InvalidHeader)
        );
        for code in [0x07, 0x24, 0x32, 0xff] {
            assert_eq!(
                decode_report(profile, &report(code, 0, false)),
                Err(DecodeError::UnknownControl { code })
            );
        }
    }
    for code in [0x01, 0x25, 0x30, 0x31, 0x33, 0x35, 0x34] {
        for state in [2, 255] {
            assert_eq!(
                decode_report(Profile::Rev2, &report(code, state, true)),
                Err(DecodeError::InvalidPushState { code, state })
            );
        }
    }
    let mut arbitrary = report(0x25, 1, false);
    arbitrary[3..9].copy_from_slice(&[42, 0, 0xff, 0xfe, 2, 3]);
    assert_eq!(
        decode_report(Profile::Rev2, &arbitrary),
        Ok(DecodedInput::ButtonState {
            index: 6,
            pressed: true
        })
    );
}

#[test]
fn strict_hex_rejects_non_ascii_separators_and_bad_lengths() {
    let good = "41434b00000000000001";
    assert_eq!(
        parse_hex_report(good),
        parse_hex_report(&good.to_ascii_uppercase())
    );
    for invalid in [
        "",
        "41434b0000000000000",
        "41434b000000000000010",
        " 41434b00000000000001",
        "41434b00000000000001\n",
        "41434b00-00000000001",
        "41434b0000000000000z",
        "41434b0000000000000é",
    ] {
        assert!(parse_hex_report(invalid).is_err(), "{invalid:?}");
    }
    assert_eq!(
        parse_hex_report(&"a".repeat(1026)),
        Err(HexError::InvalidLength { bytes: 1026 })
    );
    assert_eq!(
        parse_hex_report("41434b0000000000000z"),
        Err(HexError::InvalidDigit { offset: 19 })
    );
    assert_eq!(
        decode_hex_report(Profile::Rev2, good),
        Err(HexReportError::Report(DecodeError::InvalidLength {
            profile: Profile::Rev2,
            actual: 10
        }))
    );
    let mut max = good.to_owned();
    max.push_str(&"00".repeat(502));
    assert_eq!(max.len(), 1024);
    assert!(decode_hex_report(Profile::Original, &max).is_ok());
}

#[test]
fn decoded_inputs_have_stable_tagged_json() {
    for (input, value) in [
        (
            DecodedInput::Acknowledgment,
            json!({"kind":"acknowledgment"}),
        ),
        (DecodedInput::NoControl, json!({"kind":"no_control"})),
        (
            DecodedInput::ButtonPressObserved { index: 5 },
            json!({"kind":"button_press_observed","index":5}),
        ),
        (
            DecodedInput::EncoderPressObserved { index: 2 },
            json!({"kind":"encoder_press_observed","index":2}),
        ),
        (
            DecodedInput::EncoderTurn {
                index: 1,
                delta: -1,
            },
            json!({"kind":"encoder_turn","index":1,"delta":-1}),
        ),
        (
            DecodedInput::ButtonState {
                index: 8,
                pressed: false,
            },
            json!({"kind":"button_state","index":8,"pressed":false}),
        ),
        (
            DecodedInput::EncoderState {
                index: 0,
                pressed: true,
            },
            json!({"kind":"encoder_state","index":0,"pressed":true}),
        ),
    ] {
        assert_eq!(serde_json::to_value(input).unwrap(), value);
        assert_eq!(
            serde_json::from_value::<DecodedInput>(value).unwrap(),
            input
        );
    }
    assert!(
        serde_json::from_value::<DecodedInput>(json!({"kind":"acknowledgment","extra":0})).is_err()
    );
}
