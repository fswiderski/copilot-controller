use super::super::{
    Profile,
    controls::{
        ControlId, ControlKind, PhysicalEventKind, Recommendation, RecommendedAction, normalize,
        profile_controls,
    },
    input::{DecodedInput, decode_report},
};
use serde_json::json;
use std::{collections::HashSet, str::FromStr};

#[test]
fn profile_has_stable_controls_recommendations_and_stop_adjacency() {
    let controls = profile_controls(Profile::Original);
    assert_eq!(controls, profile_controls(Profile::Rev2));
    assert_eq!(controls.len(), 15);
    assert_eq!(
        controls
            .iter()
            .map(|control| control.id)
            .collect::<HashSet<_>>()
            .len(),
        15
    );
    for (index, control) in controls[..6].iter().enumerate() {
        assert_eq!(control.kind, ControlKind::Press);
        assert_eq!(
            control.recommended,
            Some(Recommendation::SelectSlot(index as u8 + 1))
        );
    }
    assert_eq!(controls[6].id, ControlId::Encoder1Turn);
    assert_eq!(controls[6].recommended, Some(Recommendation::MoveCursor));
    assert_eq!(controls[7].id, ControlId::Encoder1Press);
    assert_eq!(controls[7].recommended, Some(Recommendation::CommitCursor));
    let stop = &controls[8];
    assert_eq!(stop.id, ControlId::Button8);
    assert_eq!(
        stop.recommended,
        Some(Recommendation::Command(RecommendedAction::Stop))
    );
    assert!(stop.isolated_stop);
    assert!(!stop.near_selection);
    assert_eq!(stop.adjacent, &[ControlId::Button7]);
    let button7 = controls
        .iter()
        .find(|control| control.id == ControlId::Button7)
        .unwrap();
    assert_eq!(button7.adjacent, &[ControlId::Button8]);
    assert!(
        controls[9..]
            .iter()
            .all(|control| control.recommended.is_none())
    );
    assert_eq!(
        controls
            .iter()
            .filter(|control| control.isolated_stop)
            .count(),
        1
    );
    for control in controls {
        assert_eq!(
            control.kind,
            if control.id.as_str().ends_with("_turn") {
                ControlKind::Turn
            } else {
                ControlKind::Press
            }
        );
    }
    assert_eq!(
        serde_json::to_value(Recommendation::SelectSlot(1)).unwrap(),
        json!({"select_slot":1})
    );
    assert_eq!(
        serde_json::to_value(Recommendation::Command(RecommendedAction::Stop)).unwrap(),
        json!({"command":"stop"})
    );
}

#[test]
fn control_ids_are_stable_strings_and_reject_unknown_names() {
    for id in ControlId::ALL {
        assert_eq!(ControlId::from_str(id.as_str()), Ok(id));
        let serialized = serde_json::to_string(&id).unwrap();
        assert_eq!(serialized, format!("\"{}\"", id.as_str()));
        assert_eq!(serde_json::from_str::<ControlId>(&serialized).unwrap(), id);
    }
    for invalid in ["button_9", "button_00", "Button_0", "encoder_3_press", ""] {
        assert!(ControlId::from_str(invalid).is_err());
        assert!(serde_json::from_value::<ControlId>(json!(invalid)).is_err());
    }
}

#[test]
fn normalization_keeps_explicit_states_unit_turns_generation_and_clock() {
    let sample = json!({
        "source": "synthetic_clock",
        "stamp": {"epoch": "synthetic", "ticks": 123, "frequency": 1000}
    });
    for (index, id) in [
        ControlId::Button0,
        ControlId::Button1,
        ControlId::Button2,
        ControlId::Button3,
        ControlId::Button4,
        ControlId::Button5,
        ControlId::Button6,
        ControlId::Button7,
        ControlId::Button8,
    ]
    .into_iter()
    .enumerate()
    {
        for (pressed, kind) in [
            (true, PhysicalEventKind::Press),
            (false, PhysicalEventKind::Release),
        ] {
            let result = normalize(
                DecodedInput::ButtonState {
                    index: index as u8,
                    pressed,
                },
                u64::MAX,
                sample.clone(),
            )
            .unwrap();
            assert_eq!(result.control, id);
            assert_eq!(result.kind, kind);
            assert_eq!(result.connection_generation, u64::MAX);
            assert_eq!(result.at, sample);
        }
    }
    for (index, press, turn) in [
        (0, ControlId::Encoder0Press, ControlId::Encoder0Turn),
        (1, ControlId::Encoder1Press, ControlId::Encoder1Turn),
        (2, ControlId::Encoder2Press, ControlId::Encoder2Turn),
    ] {
        for (pressed, kind) in [
            (true, PhysicalEventKind::Press),
            (false, PhysicalEventKind::Release),
        ] {
            let event =
                normalize(DecodedInput::EncoderState { index, pressed }, 42, "clock").unwrap();
            assert_eq!(
                (
                    event.control,
                    event.kind,
                    event.connection_generation,
                    event.at
                ),
                (press, kind, 42, "clock")
            );
        }
        for delta in [-1, 1] {
            let event = normalize(DecodedInput::EncoderTurn { index, delta }, 7, "clock").unwrap();
            assert_eq!(
                (event.control, event.kind),
                (turn, PhysicalEventKind::Turn(delta))
            );
        }
    }
    let event = normalize(
        DecodedInput::EncoderTurn {
            index: 1,
            delta: -1,
        },
        5,
        sample.clone(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(event).unwrap(),
        json!({
            "control":"encoder_1_turn",
            "kind":{"turn":-1},
            "connection_generation":5,
            "at":sample
        })
    );
}

#[test]
fn normalization_drops_observations_invalid_values_and_noncontrols() {
    for input in [
        DecodedInput::Acknowledgment,
        DecodedInput::NoControl,
        DecodedInput::ButtonPressObserved { index: 0 },
        DecodedInput::EncoderPressObserved { index: 2 },
        DecodedInput::ButtonState {
            index: 9,
            pressed: true,
        },
        DecodedInput::ButtonState {
            index: 255,
            pressed: false,
        },
        DecodedInput::EncoderState {
            index: 3,
            pressed: true,
        },
        DecodedInput::EncoderTurn { index: 3, delta: 1 },
        DecodedInput::EncoderTurn { index: 0, delta: 0 },
        DecodedInput::EncoderTurn { index: 0, delta: 2 },
        DecodedInput::EncoderTurn {
            index: 0,
            delta: -2,
        },
        DecodedInput::EncoderTurn {
            index: 0,
            delta: i8::MIN,
        },
    ] {
        assert_eq!(normalize(input, 1, 123), None, "{input:?}");
    }

    // Original push observations never imply a matched release, even when repeated.
    let observed = b"ACK\0\0\0\0\0\0\x01\0";
    for _ in 0..2 {
        let decoded = decode_report(Profile::Original, observed).unwrap();
        assert_eq!(normalize(decoded, 1, 123), None);
    }
    let original_turn = decode_report(Profile::Original, b"ACK\0\0\0\0\0\0\x91").unwrap();
    assert_eq!(
        normalize(original_turn, 1, 123).unwrap().kind,
        PhysicalEventKind::Turn(1)
    );
}

#[test]
fn one_explicit_state_report_does_not_clear_other_controls() {
    let events = [
        DecodedInput::ButtonState {
            index: 0,
            pressed: true,
        },
        DecodedInput::ButtonState {
            index: 1,
            pressed: true,
        },
        DecodedInput::ButtonState {
            index: 0,
            pressed: false,
        },
    ]
    .map(|input| normalize(input, 2, 10).unwrap());
    assert_eq!(
        events.map(|event| (event.control, event.kind)),
        [
            (ControlId::Button0, PhysicalEventKind::Press),
            (ControlId::Button1, PhysicalEventKind::Press),
            (ControlId::Button0, PhysicalEventKind::Release),
        ]
    );
}
