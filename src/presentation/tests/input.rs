use crate::domain::{Action, ClockSource, RefusalReason, RuntimeEpoch, Slot};

use super::{epoch, event, input_profile, sample};
use crate::presentation::{
    input::{
        AjazzDecodedInput, CtDecodedInput, GestureTracker, Intent, LogicalBinding,
        MicroMicCoalescer, PhysicalEventKind, RoleError, normalize_ajazz, normalize_ct,
        normalize_micro, validate_binding_shapes, validate_live_bindings,
    },
    profiles::{self, ControlKind},
};

fn binding(control: &str, intent: Intent) -> LogicalBinding {
    LogicalBinding {
        control: control.into(),
        intent,
        adjacency_confirmed: false,
    }
}

fn selected(control: &str, slot: u8) -> LogicalBinding {
    binding(control, Intent::SelectSlot(Slot::new(slot).unwrap()))
}

#[test]
fn micro_mic_keeps_its_separate_400ms_two_switch_coalescing() {
    let mut mic = MicroMicCoalescer::new(7, epoch()).unwrap();
    for (control, kind, ticks, emitted) in [
        ("ACT10", PhysicalEventKind::Press, 0, true),
        ("ACT10", PhysicalEventKind::Press, 1, false),
        ("ACT11", PhysicalEventKind::Press, 2, false),
        ("ACT10", PhysicalEventKind::Release, 3, false),
        ("ACT10", PhysicalEventKind::Press, 4, false),
        ("ACT11", PhysicalEventKind::Release, 5, false),
        ("ACT10", PhysicalEventKind::Release, 6, false),
        ("ACT10", PhysicalEventKind::Press, 399, false),
        ("ACT10", PhysicalEventKind::Release, 400, false),
        ("ACT11", PhysicalEventKind::Press, 400, true),
        ("ACT11", PhysicalEventKind::Release, 401, false),
        ("ACT10", PhysicalEventKind::Press, 799, false),
        ("ACT10", PhysicalEventKind::Release, 800, false),
        ("ACT10", PhysicalEventKind::Press, 800, true),
        ("ACT06", PhysicalEventKind::Press, 801, false),
    ] {
        assert_eq!(mic.observe(&event(control, kind, ticks)), Ok(emitted));
    }
    let mut changed = event("ACT10", PhysicalEventKind::Release, 801);
    changed.at.source = ClockSource::WindowsQpc;
    assert_eq!(mic.observe(&changed), Err(RefusalReason::ClockMismatch));
    mic.reset(8, epoch()).unwrap();
    let mut first = event("ACT10", PhysicalEventKind::Press, 0);
    first.connection_generation = 8;
    assert_eq!(mic.observe(&first), Ok(true));
}

#[test]
fn i01_duplicate_presses_overlap_and_unmatched_releases() {
    let (profile, scope) = input_profile(profiles::ajazz_rev2());
    let bindings = [selected("button_0", 1), selected("button_1", 2)];
    let mut tracker = GestureTracker::new(7, epoch()).unwrap();
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Release, 0)
        ),
        Ok(None)
    );
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Press, 1)
        ),
        Ok(None)
    );
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Press, 2)
        ),
        Ok(None)
    );
    let request = tracker
        .observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Release, 3),
        )
        .unwrap()
        .unwrap();
    assert_eq!(request.intent, bindings[0].intent);
    assert_eq!(request.delta, 0);
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Release, 4)
        ),
        Ok(None)
    );

    let mut tracker = GestureTracker::new(7, epoch()).unwrap();
    for (control, kind, ticks) in [
        ("button_0", PhysicalEventKind::Press, 0),
        ("button_1", PhysicalEventKind::Press, 1),
        ("button_0", PhysicalEventKind::Release, 2),
        ("button_1", PhysicalEventKind::Release, 3),
    ] {
        assert_eq!(
            tracker.observe(&profile, &scope, &bindings, &event(control, kind, ticks)),
            Ok(None)
        );
    }
    tracker
        .observe(
            &profile,
            &scope,
            &bindings,
            &event("button_1", PhysicalEventKind::Press, 4),
        )
        .unwrap();
    assert!(
        tracker
            .observe(
                &profile,
                &scope,
                &bindings,
                &event("button_1", PhysicalEventKind::Release, 5)
            )
            .unwrap()
            .is_some()
    );
}

#[test]
fn i01_hold_boundary_scope_and_complete_binding_list_are_checked() {
    let (profile, scope) = input_profile(profiles::ajazz_rev2());
    let bindings = [selected("button_0", 1)];
    for (end, expected) in [(3000, true), (3001, false)] {
        let mut tracker = GestureTracker::new(7, epoch()).unwrap();
        tracker
            .observe(
                &profile,
                &scope,
                &bindings,
                &event("button_0", PhysicalEventKind::Press, 0),
            )
            .unwrap();
        let result = tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Release, end),
        );
        if expected {
            assert!(result.unwrap().is_some());
        } else {
            assert_eq!(result, Err(RefusalReason::ExpiredIntent));
        }
    }
    let mut tracker = GestureTracker::new(7, epoch()).unwrap();
    tracker
        .observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Press, 0),
        )
        .unwrap();
    let mut changed_scope = scope.clone();
    changed_scope.firmware = Some("different".into());
    assert_eq!(
        tracker.observe(
            &profile,
            &changed_scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Release, 1)
        ),
        Err(RefusalReason::WrongTarget)
    );
    tracker
        .observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Press, 2),
        )
        .unwrap();
    let changed_bindings = [bindings[0].clone(), selected("button_1", 2)];
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &changed_bindings,
            &event("button_0", PhysicalEventKind::Release, 3)
        ),
        Err(RefusalReason::ChangedBinding)
    );
    tracker
        .observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Press, 4),
        )
        .unwrap();
    assert!(
        tracker
            .observe(
                &profile,
                &scope,
                &bindings,
                &event("button_0", PhysicalEventKind::Release, 5)
            )
            .unwrap()
            .is_some()
    );
}

#[test]
fn i01_clock_and_generation_cuts_discard_holds_without_bypassing_debounce() {
    let (profile, scope) = input_profile(profiles::ajazz_rev2());
    let bindings = [selected("button_0", 1)];
    for cut in 0..5 {
        let mut tracker = GestureTracker::new(7, epoch()).unwrap();
        tracker
            .observe(
                &profile,
                &scope,
                &bindings,
                &event("button_0", PhysicalEventKind::Press, 100),
            )
            .unwrap();
        let mut release = event("button_0", PhysicalEventKind::Release, 101);
        match cut {
            0 => release.at.source = ClockSource::WindowsQpc,
            1 => release.at.stamp.frequency = 2000,
            2 => release.at.stamp.frequency = 0,
            3 => release.at.stamp.ticks = 99,
            _ => {
                release.at.stamp.epoch =
                    RuntimeEpoch::parse_str("22222222-2222-4222-8222-222222222222").unwrap()
            }
        }
        assert_eq!(
            tracker.observe(&profile, &scope, &bindings, &release),
            Err(RefusalReason::ClockMismatch)
        );
        tracker.reset(8, epoch()).unwrap();
        let mut unmatched = event("button_0", PhysicalEventKind::Release, 0);
        unmatched.connection_generation = 8;
        assert_eq!(
            tracker.observe(&profile, &scope, &bindings, &unmatched),
            Ok(None)
        );
    }
    let mut tracker = GestureTracker::new(7, epoch()).unwrap();
    tracker
        .observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Press, 0),
        )
        .unwrap();
    tracker
        .observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Release, 10),
        )
        .unwrap();
    let mut stale = event("button_0", PhysicalEventKind::Press, 11);
    stale.connection_generation = 8;
    assert_eq!(
        tracker.observe(&profile, &scope, &bindings, &stale),
        Err(RefusalReason::ChangedGeneration)
    );
    tracker
        .observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Press, 12),
        )
        .unwrap();
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Release, 100)
        ),
        Err(RefusalReason::Duplicate)
    );
    let mut tracker = GestureTracker::new(7, epoch()).unwrap();
    tracker
        .observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Press, 0),
        )
        .unwrap();
    let mut foreign_press = event("button_1", PhysicalEventKind::Press, 1);
    foreign_press.connection_generation = 8;
    assert_eq!(
        tracker.observe(&profile, &scope, &bindings, &foreign_press),
        Err(RefusalReason::ChangedGeneration)
    );
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Release, 2)
        ),
        Ok(None)
    );
}

#[test]
fn clock_taint_blocks_turns_until_reset_or_matched_clean_release() {
    let (profile, scope) = input_profile(profiles::ajazz_rev2());
    let bindings = [
        selected("button_0", 1),
        binding("encoder_1_turn", Intent::MoveCursor),
    ];
    for reset in [false, true] {
        let mut tracker = GestureTracker::new(7, epoch()).unwrap();
        let mut bad = event("encoder_1_turn", PhysicalEventKind::Turn(1), 0);
        bad.at.stamp.frequency = 0;
        assert_eq!(
            tracker.observe(&profile, &scope, &bindings, &bad),
            Err(RefusalReason::ClockMismatch)
        );
        assert_eq!(
            tracker.observe(
                &profile,
                &scope,
                &bindings,
                &event("encoder_1_turn", PhysicalEventKind::Turn(1), 1)
            ),
            Ok(None)
        );
        if reset {
            tracker.reset(7, epoch()).unwrap();
        } else {
            assert_eq!(
                tracker.observe(
                    &profile,
                    &scope,
                    &bindings,
                    &event("button_0", PhysicalEventKind::Release, 2)
                ),
                Ok(None)
            );
            assert_eq!(
                tracker.observe(
                    &profile,
                    &scope,
                    &bindings,
                    &event("encoder_1_turn", PhysicalEventKind::Turn(1), 3)
                ),
                Ok(None)
            );
            assert_eq!(
                tracker.observe(
                    &profile,
                    &scope,
                    &bindings,
                    &event("button_0", PhysicalEventKind::Press, 4)
                ),
                Ok(None)
            );
            let mut bad_release = event("button_0", PhysicalEventKind::Release, 5);
            bad_release.at.stamp.frequency = 0;
            assert_eq!(
                tracker.observe(&profile, &scope, &bindings, &bad_release),
                Err(RefusalReason::ClockMismatch)
            );
            assert_eq!(
                tracker.observe(
                    &profile,
                    &scope,
                    &bindings,
                    &event("encoder_1_turn", PhysicalEventKind::Turn(1), 6)
                ),
                Ok(None)
            );
            assert_eq!(
                tracker.observe(
                    &profile,
                    &scope,
                    &bindings,
                    &event("button_0", PhysicalEventKind::Release, 7)
                ),
                Ok(None)
            );
        }
        let turn = tracker
            .observe(
                &profile,
                &scope,
                &bindings,
                &event("encoder_1_turn", PhysicalEventKind::Turn(-1), 8),
            )
            .unwrap()
            .unwrap();
        assert_eq!((turn.intent, turn.delta), (Intent::MoveCursor, -1));
    }
}

#[test]
fn i01_unit_turns_kind_checks_and_turn_during_hold() {
    let (profile, scope) = input_profile(profiles::ajazz_rev2());
    let bindings = [
        selected("button_0", 1),
        binding("encoder_1_turn", Intent::MoveCursor),
    ];
    let mut tracker = GestureTracker::new(7, epoch()).unwrap();
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("encoder_1_turn", PhysicalEventKind::Turn(2), 0)
        ),
        Ok(None)
    );
    tracker
        .observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Press, 1),
        )
        .unwrap();
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("encoder_1_turn", PhysicalEventKind::Turn(-1), 2)
        ),
        Ok(None)
    );
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Release, 3)
        ),
        Ok(None)
    );
    let turn = tracker
        .observe(
            &profile,
            &scope,
            &bindings,
            &event("encoder_1_turn", PhysicalEventKind::Turn(-1), 4),
        )
        .unwrap()
        .unwrap();
    assert_eq!((turn.intent, turn.delta), (Intent::MoveCursor, -1));
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("button_0", PhysicalEventKind::Turn(1), 5)
        ),
        Err(RefusalReason::Unsupported)
    );
    tracker
        .observe(
            &profile,
            &scope,
            &bindings,
            &event("encoder_1_turn", PhysicalEventKind::Press, 6),
        )
        .unwrap();
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("encoder_1_turn", PhysicalEventKind::Release, 7)
        ),
        Err(RefusalReason::Unsupported)
    );
    let (micro, micro_scope) = input_profile(profiles::micro());
    let dpad = [binding("DPAD_UP", Intent::Command(Action::Up))];
    let mut tracker = GestureTracker::new(7, epoch()).unwrap();
    assert_eq!(
        tracker.observe(
            &micro,
            &micro_scope,
            &dpad,
            &event("DPAD_UP", PhysicalEventKind::Turn(1), 0)
        ),
        Err(RefusalReason::Unsupported)
    );
    let (ct, ct_scope) = input_profile(profiles::ct());
    let strip = [binding("strip_left", Intent::ShowHealth)];
    let mut tracker = GestureTracker::new(7, epoch()).unwrap();
    assert_eq!(
        tracker.observe(
            &ct,
            &ct_scope,
            &strip,
            &event("strip_left", PhysicalEventKind::Turn(1), 0)
        ),
        Err(RefusalReason::Unsupported)
    );
}

#[test]
fn i01_debounce_boundaries_do_not_extend_on_rejection_and_commands_stay_disabled() {
    let (profile, scope) = input_profile(profiles::ajazz_rev2());
    for (id, intent, period) in [
        ("button_0", Intent::SelectSlot(Slot::new(1).unwrap()), 300),
        ("button_8", Intent::Command(Action::Stop), 500),
    ] {
        let bindings = [binding(id, intent)];
        let mut tracker = GestureTracker::new(7, epoch()).unwrap();
        tracker
            .observe(
                &profile,
                &scope,
                &bindings,
                &event(id, PhysicalEventKind::Press, 0),
            )
            .unwrap();
        let first = tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event(id, PhysicalEventKind::Release, 10),
        );
        if intent == Intent::Command(Action::Stop) {
            assert_eq!(first, Err(RefusalReason::Disabled));
        } else {
            assert!(first.unwrap().is_some());
        }
        tracker
            .observe(
                &profile,
                &scope,
                &bindings,
                &event(id, PhysicalEventKind::Press, 11),
            )
            .unwrap();
        assert_eq!(
            tracker.observe(
                &profile,
                &scope,
                &bindings,
                &event(id, PhysicalEventKind::Release, 10 + period - 1)
            ),
            Err(RefusalReason::Duplicate)
        );
        tracker
            .observe(
                &profile,
                &scope,
                &bindings,
                &event(id, PhysicalEventKind::Press, 10 + period - 1),
            )
            .unwrap();
        let boundary = tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event(id, PhysicalEventKind::Release, 10 + period),
        );
        if intent == Intent::Command(Action::Stop) {
            assert_eq!(boundary, Err(RefusalReason::Disabled));
        } else {
            assert!(boundary.unwrap().is_some());
        }
    }
}

#[test]
fn i01_held_limit_unknown_control_and_full_input_evidence_fail_closed() {
    let (mut profile, scope) = input_profile(profiles::ajazz_rev2());
    let template = profile.controls[0].clone();
    for index in 0..65 {
        let mut control = template.clone();
        control.id = format!("fixture_{index:02}");
        control.recommended = None;
        profile.controls.push(control);
    }
    let mut tracker = GestureTracker::new(7, epoch()).unwrap();
    for index in 0..64 {
        assert_eq!(
            tracker.observe(
                &profile,
                &scope,
                &[],
                &event(
                    &format!("fixture_{index:02}"),
                    PhysicalEventKind::Press,
                    index
                )
            ),
            Ok(None)
        );
    }
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &[],
            &event("fixture_64", PhysicalEventKind::Press, 64)
        ),
        Err(RefusalReason::Busy)
    );
    for index in 0..64 {
        assert_eq!(
            tracker.observe(
                &profile,
                &scope,
                &[],
                &event(
                    &format!("fixture_{index:02}"),
                    PhysicalEventKind::Release,
                    65 + index
                )
            ),
            Err(RefusalReason::Busy)
        );
    }
    let bindings = [selected("fixture_00", 1)];
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("fixture_00", PhysicalEventKind::Press, 129)
        ),
        Err(RefusalReason::Busy)
    );
    tracker.reset(7, epoch()).unwrap();
    tracker
        .observe(
            &profile,
            &scope,
            &bindings,
            &event("fixture_00", PhysicalEventKind::Press, 130),
        )
        .unwrap();
    assert!(
        tracker
            .observe(
                &profile,
                &scope,
                &bindings,
                &event("fixture_00", PhysicalEventKind::Release, 131)
            )
            .unwrap()
            .is_some()
    );

    let mut tracker = GestureTracker::new(7, epoch()).unwrap();
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("missing", PhysicalEventKind::Press, 0)
        ),
        Err(RefusalReason::Unsupported)
    );
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &bindings,
            &event("missing", PhysicalEventKind::Release, 1)
        ),
        Err(RefusalReason::Unsupported)
    );
    let default = profiles::ajazz_rev2();
    assert!(validate_live_bindings(&default, &default.scope, &[selected("button_0", 1)]).is_err());
    let oversized = vec![selected("button_0", 1); 65];
    let mut tracker = GestureTracker::new(7, epoch()).unwrap();
    assert_eq!(
        tracker.observe(
            &profile,
            &scope,
            &oversized,
            &event("button_0", PhysicalEventKind::Press, 0)
        ),
        Err(RefusalReason::Unsupported)
    );
}

#[test]
fn i02_roles_limits_sensitive_controls_and_bidirectional_adjacency() {
    let mut profile = profiles::ajazz_rev2();
    assert!(matches!(
        validate_binding_shapes(&profile, &vec![selected("button_0", 1); 65]),
        Err(RoleError::TooManyBindings)
    ));
    assert!(matches!(
        validate_binding_shapes(
            &profile,
            &[selected("button_0", 1), selected("button_0", 2)]
        ),
        Err(RoleError::DuplicateControl(_))
    ));
    assert!(matches!(
        validate_binding_shapes(&profile, &[selected("unknown", 1)]),
        Err(RoleError::UnknownControl(_))
    ));
    assert!(matches!(
        validate_binding_shapes(
            &profile,
            &[binding("button_0", Intent::Command(Action::None))]
        ),
        Err(RoleError::UnsupportedAction)
    ));
    for invalid in [
        binding("button_0", Intent::MoveCursor),
        binding("encoder_1_turn", Intent::SelectSlot(Slot::new(1).unwrap())),
        binding("encoder_1_turn", Intent::CommitCursor),
        binding("encoder_1_turn", Intent::Command(Action::Accept)),
        binding("encoder_1_turn", Intent::Command(Action::Send)),
        binding("encoder_1_turn", Intent::Command(Action::Fork)),
        binding("encoder_1_turn", Intent::Command(Action::Stop)),
    ] {
        assert!(matches!(
            validate_binding_shapes(&profile, &[invalid]),
            Err(RoleError::WrongControlKind(_))
        ));
    }
    let mut unsafe_stop = binding("button_0", Intent::Command(Action::Stop));
    unsafe_stop.adjacency_confirmed = true;
    assert!(matches!(
        validate_binding_shapes(&profile, &[unsafe_stop]),
        Err(RoleError::StopNotIsolated(_))
    ));
    assert!(matches!(
        validate_binding_shapes(
            &profile,
            &[binding("button_7", Intent::Command(Action::Accept))]
        ),
        Err(RoleError::AdjacencyConfirmationRequired(_))
    ));
    let stop = binding("button_8", Intent::Command(Action::Stop));
    assert!(validate_binding_shapes(&profile, std::slice::from_ref(&stop)).is_ok());
    let mut accept = binding("button_7", Intent::Command(Action::Accept));
    accept.adjacency_confirmed = true;
    for clear in ["button_8", "button_7"] {
        let original = profile.clone();
        profile
            .controls
            .iter_mut()
            .find(|control| control.id == clear)
            .unwrap()
            .adjacent
            .clear();
        assert!(matches!(
            validate_binding_shapes(&profile, &[stop.clone(), accept.clone()]),
            Err(RoleError::StopAdjacentToAcceptOrSend(_))
        ));
        profile = original;
    }
    let micro = profiles::micro();
    let mut stop = binding("ACT08", Intent::Command(Action::Stop));
    stop.adjacency_confirmed = true;
    assert!(matches!(
        validate_binding_shapes(&micro, &[stop]),
        Err(RoleError::StopNotIsolated(_))
    ));
}

#[test]
fn i02_priority_profiles_and_non_normalized_recommendations_are_explicit() {
    for (intent, priority) in [
        (Intent::SelectSlot(Slot::new(1).unwrap()), 0),
        (Intent::MoveCursor, 1),
        (Intent::CommitCursor, 1),
        (Intent::Command(Action::Stop), 2),
        (Intent::Command(Action::Accept), 3),
        (Intent::Command(Action::Send), 3),
        (Intent::Command(Action::Left), 4),
        (Intent::ShowHealth, 5),
        (Intent::PageNext, 5),
        (Intent::Command(Action::Fork), 5),
    ] {
        assert_eq!(intent.priority(), priority);
    }
    for profile in [
        profiles::micro(),
        profiles::ajazz_original(),
        profiles::ajazz_rev2(),
        profiles::ct(),
    ] {
        let priorities: Vec<_> = profile
            .prioritized_controls()
            .iter()
            .map(|control| control.recommended.map_or(6, Intent::priority))
            .collect();
        assert!(priorities.windows(2).all(|pair| pair[0] <= pair[1]));
    }
    let ct = profiles::ct();
    assert_eq!(ct.controls.len(), 49);
    assert_eq!(
        ct.control("key_04").unwrap().recommended,
        Some(Intent::SelectSlot(Slot::new(3).unwrap()))
    );
    assert_eq!(
        ct.control("round_1").unwrap().recommended,
        Some(Intent::Command(Action::Stop))
    );
    assert!(!ct.control("key_04").unwrap().normalizes_discrete);
    assert_eq!(ct.control("wheel_turn").unwrap().kind, ControlKind::Wheel);
    let micro = profiles::micro();
    assert!(!micro.control("encoder_press").unwrap().normalizes_discrete);
    assert!(!micro.control("DPAD_UP").unwrap().normalizes_discrete);
}

#[test]
fn i02_strict_profile_adapters_only_normalize_documented_discrete_events() {
    let raw = |key: &str, action: serde_json::Value| serde_json::json!({"m":"v.oai.hid","p":{"k":key,"act":action}});
    for value in [
        serde_json::json!(true),
        serde_json::json!(1.0),
        serde_json::json!("1"),
    ] {
        assert!(normalize_micro(&raw("AG00", value), 7, sample(0)).is_none());
    }
    assert_eq!(
        normalize_micro(&raw("ENC_CC", serde_json::json!(2)), 7, sample(0))
            .unwrap()
            .kind,
        PhysicalEventKind::Turn(-1)
    );
    for key in ["ENC_CLK", "ACT10", "ACT11", "DPAD_UP", "AG06", "ag00"] {
        assert!(normalize_micro(&raw(key, serde_json::json!(1)), 7, sample(0)).is_none());
    }
    let original = profiles::ajazz_original();
    let rev2 = profiles::ajazz_rev2();
    assert!(
        normalize_ajazz(
            &original,
            AjazzDecodedInput::ButtonPressObserved(0),
            7,
            sample(0)
        )
        .is_none()
    );
    assert!(
        normalize_ajazz(
            &original,
            AjazzDecodedInput::ButtonState {
                index: 0,
                pressed: false
            },
            7,
            sample(0)
        )
        .is_none()
    );
    assert_eq!(
        normalize_ajazz(
            &rev2,
            AjazzDecodedInput::ButtonState {
                index: 0,
                pressed: false
            },
            7,
            sample(0)
        )
        .unwrap()
        .kind,
        PhysicalEventKind::Release
    );
    assert!(
        normalize_ajazz(
            &rev2,
            AjazzDecodedInput::EncoderTurn { index: 3, delta: 1 },
            7,
            sample(0)
        )
        .is_none()
    );
    assert!(
        normalize_ajazz(
            &rev2,
            AjazzDecodedInput::EncoderTurn { index: 1, delta: 2 },
            7,
            sample(0)
        )
        .is_none()
    );
    assert_eq!(
        normalize_ct(
            CtDecodedInput::Button {
                id: 7,
                pressed: true
            },
            7,
            sample(0)
        )
        .unwrap()
        .control,
        "round_1"
    );
    assert_eq!(
        normalize_ct(CtDecodedInput::Rotation { id: 0, delta: -1 }, 7, sample(0))
            .unwrap()
            .control,
        "wheel_turn"
    );
    assert!(normalize_ct(CtDecodedInput::Rotation { id: 0, delta: 2 }, 7, sample(0)).is_none());
    assert!(normalize_ct(CtDecodedInput::Touch, 7, sample(0)).is_none());
}
