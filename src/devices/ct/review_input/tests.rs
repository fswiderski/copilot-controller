use super::*;

fn button(id: u8, state: u8) -> [u8; 7] {
    [0x82, 5, 5, 0, 0, id, state]
}

fn wheel(id: u8, delta: i8) -> [u8; 7] {
    [0x82, 5, 5, 1, 0, id, delta as u8]
}

#[test]
fn round_candidates_require_a_fresh_complete_pair_and_ignore_repeated_edges() {
    for (index, action) in [
        Action::TogglePlay,
        Action::SeekStart,
        Action::SeekEnd,
        Action::BackOneSecond,
        Action::ForwardOneSecond,
        Action::Screenshot,
        Action::ToggleDictation,
    ]
    .into_iter()
    .enumerate()
    {
        let id = 7 + index as u8;
        let mut stream = CandidateStream::default();
        assert!(stream.push(&button(id, 0), 0).unwrap().is_empty());
        assert!(stream.push(&button(id, 1), 1).unwrap().is_empty());
        assert!(stream.push(&button(id, 0), 2).unwrap().is_empty());
        assert!(stream.push(&button(id, 0), 3).unwrap().is_empty());
        assert_eq!(
            stream.push(&button(id, 1), 4).unwrap(),
            [CandidateAction { action }]
        );
        assert!(stream.push(&button(id, 1), 5).unwrap().is_empty());
        assert!(stream.push(&button(id, 0), 6).unwrap().is_empty());
        // A suppressed press stays suppressed even if held past the cooldown.
        assert!(stream.push(&button(id, 1), 100).unwrap().is_empty());
        assert!(stream.push(&button(id, 0), 101).unwrap().is_empty());
        assert_eq!(
            stream.push(&button(id, 1), 102).unwrap(),
            [CandidateAction { action }]
        );
    }
}

#[test]
fn buttons_are_independent_and_cooldown_rearms_at_its_boundary() {
    let mut stream = CandidateStream::default();
    let prime = [button(7, 1), button(8, 1), button(7, 0), button(8, 0)].concat();
    assert!(stream.push(&prime, 0).unwrap().is_empty());
    assert_eq!(
        stream
            .push(&[button(8, 1), button(7, 1)].concat(), 1)
            .unwrap(),
        [
            CandidateAction {
                action: Action::SeekStart
            },
            CandidateAction {
                action: Action::TogglePlay
            },
        ]
    );
    assert!(stream.push(&button(7, 0), 75).unwrap().is_empty());
    assert!(stream.push(&button(7, 1), 76).unwrap().is_empty());
    assert!(stream.push(&button(7, 0), 76).unwrap().is_empty());
    assert_eq!(
        stream.push(&button(7, 1), 77).unwrap(),
        [CandidateAction {
            action: Action::TogglePlay
        }]
    );
}

#[test]
fn fragmented_and_coalesced_frames_preserve_unit_wheel_candidates() {
    let bytes = [wheel(0, -2), wheel(0, 3), wheel(0, 0)].concat();
    for split in 0..=bytes.len() {
        let mut stream = CandidateStream::default();
        let mut actions = stream.push(&bytes[..split], 10).unwrap();
        actions.extend(stream.push(&bytes[split..], 10).unwrap());
        assert_eq!(
            actions
                .into_iter()
                .map(|value| value.action)
                .collect::<Vec<_>>(),
            [
                Action::PreviousFrame,
                Action::PreviousFrame,
                Action::NextFrame,
                Action::NextFrame,
                Action::NextFrame
            ]
        );
        assert_eq!(stream.eof(), Ok(()));
    }
}

#[test]
fn unrelated_controls_ticks_and_touch_never_map_to_actions() {
    let mut stream = CandidateStream::default();
    for id in (0..=0x1a).filter(|id| !(7..=13).contains(id)) {
        for state in [1, 0, 1] {
            assert!(stream.push(&button(id, state), 0).unwrap().is_empty());
        }
    }
    for id in 1..=6 {
        assert!(stream.push(&wheel(id, 127), 0).unwrap().is_empty());
    }
    assert!(stream.push(&[0x82, 4, 4, 0, 0, 42], 0).unwrap().is_empty());
    assert!(
        stream
            .push(&[0x82, 9, 9, 0x52, 0, 0, 0, 0, 0, 0, 0], 0)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn faults_are_terminal_and_discard_the_entire_batch() {
    let cases = [
        (vec![0; MAX_CHUNK_BYTES + 1], InputFailure::ChunkLimit),
        (vec![0x81], InputFailure::Framing),
        (vec![0x82, 5, 5, 0, 0, 7, 2], InputFailure::InvalidInput),
        // A correlated identity reply is explicitly not an input stream.
        (vec![0x82, 4, 4, 3, 1, b'X'], InputFailure::InvalidInput),
        (
            [wheel(0, -128), wheel(0, 1)].concat(),
            InputFailure::ActionLimit,
        ),
        (
            [0x82, 4, 4, 0, 0, 42].repeat(MAX_BATCH_FRAMES + 1),
            InputFailure::FrameLimit,
        ),
    ];
    for (bytes, error) in cases {
        let mut stream = CandidateStream::default();
        assert_eq!(stream.push(&bytes, 10), Err(error));
        assert_eq!(stream.push(&wheel(0, 1), 11), Err(InputFailure::Ended));
    }
    let mut stream = CandidateStream::default();
    let bytes = [wheel(0, 1).as_slice(), &[0xff]].concat();
    assert_eq!(stream.push(&bytes, 0), Err(InputFailure::Framing));
}

#[test]
fn exact_action_limit_handles_signed_minimum_without_overflow() {
    let mut stream = CandidateStream::default();
    let actions = stream.push(&wheel(0, -128), 0).unwrap();
    assert_eq!(actions.len(), MAX_BATCH_ACTIONS);
    assert!(
        actions
            .iter()
            .all(|value| value.action == Action::PreviousFrame)
    );
}

#[test]
fn eof_and_transport_boundaries_discard_held_and_partial_input() {
    let mut stream = CandidateStream::default();
    stream.push(&button(7, 1), 0).unwrap();
    stream.push(&button(7, 0), 1).unwrap();
    assert_eq!(stream.eof(), Ok(()));
    assert_eq!(stream.push(&button(7, 1), 2), Err(InputFailure::Ended));
    let mut next = CandidateStream::default();
    assert!(next.push(&button(7, 1), 2).unwrap().is_empty());

    next.push(&button(7, 0), 3).unwrap();
    next.end();
    assert_eq!(next.push(&button(7, 1), 4), Err(InputFailure::Ended));
    for prefix in 1..7 {
        let mut partial = CandidateStream::default();
        partial.push(&wheel(0, 1)[..prefix], 0).unwrap();
        assert_eq!(partial.eof(), Err(InputFailure::Framing));
        assert_eq!(partial.push(&wheel(0, 1), 1), Err(InputFailure::Ended));
    }
}

#[test]
fn regressing_clock_faults_before_any_actions() {
    let mut stream = CandidateStream::default();
    stream.push(&[], 10).unwrap();
    assert_eq!(
        stream.push(&wheel(0, 1), 9),
        Err(InputFailure::ClockRegression)
    );
    assert_eq!(stream.push(&wheel(0, 1), 11), Err(InputFailure::Ended));
}

#[test]
fn proposed_wire_is_action_only_strict_and_bounded() {
    for action in [
        Action::TogglePlay,
        Action::SeekStart,
        Action::SeekEnd,
        Action::BackOneSecond,
        Action::ForwardOneSecond,
        Action::Screenshot,
        Action::ToggleDictation,
        Action::PreviousFrame,
        Action::NextFrame,
    ] {
        let candidate = CandidateAction { action };
        let line = candidate.json_line();
        assert!(line.len() <= 32);
        assert_eq!(line.bytes().filter(|byte| *byte == b'\n').count(), 1);
        assert!(line.ends_with('\n'));
        assert_eq!(
            serde_json::from_str::<CandidateAction>(line).unwrap(),
            candidate
        );
        assert_eq!(line.trim_end(), serde_json::to_string(&candidate).unwrap());
    }
    for invalid in [
        r#"{"action":"toggle_play","command":"anything"}"#,
        r#"{"action":"toggle_play","action":"seek_start"}"#,
        r#"{"status":"ready"}"#,
        r#"{"action":"launch"}"#,
        r#"{"action":null}"#,
        r#"{}"#,
    ] {
        assert!(serde_json::from_str::<CandidateAction>(invalid).is_err());
    }
}
