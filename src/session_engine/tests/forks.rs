use crate::domain::{Binding, NativeProcessIdentity, RuntimeId, TerminalAnchor};

use super::super::fork::{
    Candidate, CandidateAccess, Discovery, ForkBegin, ForkDecision, ForkError, ForkId,
    ForkObservation, KNOWN_CHILD_RETENTION_MS, MAX_SEEN_SESSIONS, ObservationResult,
    OwnerResolution, PendingFork, PendingForks, UNKNOWN_CHILD_RETENTION_MS,
};
use super::{process, session, slot, slot_map};

fn fork_id(number: u128) -> ForkId {
    ForkId::new(session(number)).unwrap()
}

fn pending(map: &crate::domain::SlotMap) -> PendingFork {
    PendingFork::new(ForkBegin {
        id: fork_id(100),
        slot: slot(1),
        source: session(1),
        binding_revision: map.revision(),
        process: process(10),
        issued_ms: 10_000,
        event_offset: 100,
        seen: vec![session(2)],
    })
    .unwrap()
}

fn accessible(id: u128, owner: NativeProcessIdentity, interactive: bool) -> Candidate {
    Candidate {
        session: session(id),
        access: CandidateAccess::Accessible {
            verified_owner: owner,
            interactive,
        },
    }
}

fn discovery(candidates: &[Candidate]) -> Discovery<'_> {
    Discovery {
        complete: true,
        candidates,
    }
}

#[test]
fn f01_strict_pending_records_and_begin_invariants() {
    let map = slot_map(Binding::new(session(1), "title").unwrap());
    let fork = pending(&map);
    let value = serde_json::to_value(&fork).unwrap();
    assert_eq!(
        serde_json::from_value::<PendingFork>(value.clone()).unwrap(),
        fork
    );
    assert!(serde_json::from_str::<PendingFork>("[]").is_err());
    let mut invalid = value.clone();
    invalid.as_object_mut().unwrap().remove("child");
    assert!(serde_json::from_value::<PendingFork>(invalid).is_err());
    for (field, replacement) in [
        ("id", serde_json::json!(session(0))),
        ("source", serde_json::json!(session(0))),
        ("child", serde_json::json!(session(0))),
        ("child", serde_json::json!(session(1))),
        ("child", serde_json::json!(session(2))),
        ("seen", serde_json::json!([session(2), session(2)])),
        ("conflicted", serde_json::json!(null)),
    ] {
        let mut invalid = value.clone();
        invalid[field] = replacement;
        assert!(
            serde_json::from_value::<PendingFork>(invalid).is_err(),
            "{field}"
        );
    }
    let mut invalid = value.clone();
    invalid["process"]["pid"] = serde_json::json!(0);
    assert!(serde_json::from_value::<PendingFork>(invalid).is_err());
    let mut invalid = value.clone();
    invalid["process"]["creation_time_100ns"] = serde_json::json!(0);
    assert!(serde_json::from_value::<PendingFork>(invalid).is_err());
    let mut invalid = value.clone();
    invalid["unrecognized"] = serde_json::json!("must not survive");
    assert!(serde_json::from_value::<PendingFork>(invalid).is_err());
    let mut invalid = value.clone();
    invalid["private-secret-field"] = serde_json::json!(true);
    assert!(
        !serde_json::from_value::<PendingFork>(invalid)
            .unwrap_err()
            .to_string()
            .contains("private-secret")
    );
    assert!(
        !serde_json::from_str::<PendingForks>("\"private-secret\"")
            .unwrap_err()
            .to_string()
            .contains("private-secret")
    );
    let mut invalid = value.clone();
    invalid["seen"] = serde_json::json!(
        (0..=MAX_SEEN_SESSIONS)
            .map(|number| session(1_000 + number as u128))
            .collect::<Vec<_>>()
    );
    assert!(serde_json::from_value::<PendingFork>(invalid).is_err());
    assert!(ForkId::new(session(0)).is_err());

    let mut forks = PendingForks::default();
    assert_eq!(forks.begin(fork.clone(), &map), Ok(()));
    assert_eq!(forks.begin(fork.clone(), &map), Err(ForkError::DuplicateId));
    let mut same_slot = serde_json::to_value(&fork).unwrap();
    same_slot["id"] = serde_json::json!(session(101));
    assert_eq!(
        forks.begin(serde_json::from_value(same_slot).unwrap(), &map),
        Err(ForkError::DuplicateSlot)
    );
    let mut changed = map.clone();
    changed.set_pinned(slot(1), true, map.revision()).unwrap();
    assert_eq!(
        PendingForks::default().begin(fork.clone(), &changed),
        Err(ForkError::BindingChanged)
    );
    assert_eq!(
        PendingForks::from_json(&serde_json::to_vec(&forks).unwrap()).unwrap(),
        forks
    );
    assert_eq!(
        PendingForks::from_json(&serde_json::to_vec(&vec![fork.clone(), fork]).unwrap()),
        Err(ForkError::InvalidRecord)
    );
}

#[test]
fn f01_all_six_slots_are_bounded_and_seen_limit_is_inclusive() {
    let mut map = crate::domain::SlotMap::default();
    for number in 1..=6_u8 {
        map.bind(
            slot(number),
            Binding::new(session(u128::from(number)), "title").unwrap(),
            map.revision(),
            None,
            None,
        )
        .unwrap();
    }
    let mut forks = PendingForks::default();
    for number in 1..=6_u8 {
        let seen = (0..MAX_SEEN_SESSIONS)
            .map(|index| session(1_000 + index as u128))
            .collect();
        let fork = PendingFork::new(ForkBegin {
            id: fork_id(100 + u128::from(number)),
            slot: slot(number),
            source: session(u128::from(number)),
            binding_revision: map.revision(),
            process: process(10),
            issued_ms: 10,
            event_offset: 0,
            seen,
        })
        .unwrap();
        forks.begin(fork, &map).unwrap();
    }
    assert_eq!(forks.len(), 6);
    let bytes = serde_json::to_vec(&forks).unwrap();
    assert_eq!(PendingForks::from_json(&bytes).unwrap(), forks);
    let mut too_many: Vec<_> = forks.iter().cloned().collect();
    too_many.push(too_many[0].clone());
    assert_eq!(
        PendingForks::from_json(&serde_json::to_vec(&too_many).unwrap()),
        Err(ForkError::InvalidRecord)
    );
}

#[test]
fn f01_observation_offset_age_seen_and_conflicts() {
    let map = slot_map(Binding::new(session(1), "title").unwrap());
    let mut fork = pending(&map);
    let observe = |fork: &mut PendingFork, child, timestamp_ms, next_offset| {
        fork.observe(ForkObservation {
            child,
            timestamp_ms,
            next_offset,
        })
    };
    assert_eq!(
        observe(&mut fork, session(3), 10_000, 100),
        ObservationResult::IgnoredOffset
    );
    assert_eq!(
        observe(&mut fork, session(3), 8_999, 101),
        ObservationResult::Advanced
    );
    assert_eq!(fork.event_offset(), 101);
    assert_eq!(
        observe(&mut fork, session(1), 9_000, 102),
        ObservationResult::Advanced
    );
    assert_eq!(
        observe(&mut fork, session(2), 9_000, 103),
        ObservationResult::Advanced
    );
    assert_eq!(
        observe(&mut fork, session(0), 9_000, 104),
        ObservationResult::Advanced
    );
    assert_eq!(
        observe(&mut fork, session(3), 9_000, 105),
        ObservationResult::ChildRecorded
    );
    assert_eq!(
        observe(&mut fork, session(3), 10_000, 106),
        ObservationResult::Advanced
    );
    assert_eq!(
        observe(&mut fork, session(4), 10_000, 107),
        ObservationResult::Conflict
    );
    assert_eq!(fork.child(), Some(session(3)));
    assert!(fork.conflicted());

    let mut raw = serde_json::to_value(pending(&map)).unwrap();
    raw["issued_ms"] = serde_json::json!(500);
    let mut early: PendingFork = serde_json::from_value(raw).unwrap();
    assert_eq!(
        observe(&mut early, session(3), 0, 101),
        ObservationResult::ChildRecorded
    );
}

#[test]
fn f01_evaluation_order_deadlines_owner_and_candidate_filters() {
    let map = slot_map(Binding::new(session(1), "title").unwrap());
    let fork = pending(&map);
    let owner = OwnerResolution::Live(process(10));
    let eligible = [accessible(3, process(10), true)];
    let empty = discovery(&[]);

    let mut changed = map.clone();
    changed.set_pinned(slot(1), true, map.revision()).unwrap();
    assert_eq!(
        fork.evaluate(&changed, 0, OwnerResolution::Unresolved, empty),
        ForkDecision::BindingChanged
    );
    assert_eq!(
        fork.evaluate(&map, 9_999, OwnerResolution::Exited, empty),
        ForkDecision::ClockRegressed
    );
    assert_eq!(
        fork.evaluate(&map, 10_000 + UNKNOWN_CHILD_RETENTION_MS, owner, empty),
        ForkDecision::Deferred
    );
    assert_eq!(
        fork.evaluate(
            &map,
            10_001 + UNKNOWN_CHILD_RETENTION_MS,
            OwnerResolution::Unresolved,
            empty
        ),
        ForkDecision::Expired
    );
    assert_eq!(
        fork.evaluate(
            &map,
            10_000,
            OwnerResolution::Unresolved,
            discovery(&eligible)
        ),
        ForkDecision::Deferred
    );
    assert_eq!(
        fork.evaluate(&map, 10_000, OwnerResolution::Exited, discovery(&eligible)),
        ForkDecision::LifetimeChanged
    );
    let mut reused = process(10);
    reused.creation_time_100ns += 1;
    assert_eq!(
        fork.evaluate(
            &map,
            10_000,
            OwnerResolution::Live(reused),
            discovery(&eligible)
        ),
        ForkDecision::LifetimeChanged
    );
    assert_eq!(
        fork.evaluate(
            &map,
            10_000,
            owner,
            Discovery {
                complete: false,
                candidates: &eligible
            }
        ),
        ForkDecision::Deferred
    );
    let inaccessible = [Candidate {
        session: session(3),
        access: CandidateAccess::Inaccessible,
    }];
    assert_eq!(
        fork.evaluate(&map, 10_000, owner, discovery(&inaccessible)),
        ForkDecision::Deferred
    );
    let many = (0..=MAX_SEEN_SESSIONS)
        .map(|id| accessible(1_000 + id as u128, process(10), true))
        .collect::<Vec<_>>();
    assert_eq!(
        fork.evaluate(&map, 10_000, owner, discovery(&many)),
        ForkDecision::Deferred
    );
    let duplicate = [eligible[0], eligible[0]];
    assert_eq!(
        fork.evaluate(&map, 10_000, owner, discovery(&duplicate)),
        ForkDecision::Ambiguous
    );
    let filtered = [
        accessible(0, process(10), true),
        accessible(1, process(10), true),
        accessible(2, process(10), true),
        accessible(4, process(11), true),
        accessible(5, process(10), false),
        eligible[0],
    ];
    assert_eq!(
        fork.evaluate(&map, 10_000, owner, discovery(&filtered)),
        ForkDecision::Rebind { child: session(3) }
    );
    let multiple = [eligible[0], accessible(4, process(10), true)];
    assert_eq!(
        fork.evaluate(&map, 10_000, owner, discovery(&multiple)),
        ForkDecision::Ambiguous
    );

    let mut known = pending(&map);
    known.observe(ForkObservation {
        child: session(3),
        timestamp_ms: 10_000,
        next_offset: 101,
    });
    assert_eq!(
        known.evaluate(
            &map,
            10_000 + KNOWN_CHILD_RETENTION_MS,
            owner,
            discovery(&[])
        ),
        ForkDecision::Deferred
    );
    assert_eq!(
        known.evaluate(
            &map,
            10_001 + KNOWN_CHILD_RETENTION_MS,
            owner,
            discovery(&eligible)
        ),
        ForkDecision::Expired
    );
    assert_eq!(
        known.evaluate(
            &map,
            10_001,
            owner,
            discovery(&[accessible(4, process(10), true)])
        ),
        ForkDecision::Deferred
    );
    assert_eq!(
        known.evaluate(&map, 10_001, owner, discovery(&multiple)),
        ForkDecision::Rebind { child: session(3) }
    );
    known.observe(ForkObservation {
        child: session(4),
        timestamp_ms: 10_001,
        next_offset: 102,
    });
    assert_eq!(
        known.evaluate(&map, 10_001, OwnerResolution::Unresolved, empty),
        ForkDecision::Deferred
    );
    assert_eq!(
        known.evaluate(&map, 10_001, OwnerResolution::Exited, empty),
        ForkDecision::LifetimeChanged
    );
    assert_eq!(
        known.evaluate(
            &map,
            10_001,
            owner,
            Discovery {
                complete: false,
                candidates: &[]
            }
        ),
        ForkDecision::Ambiguous
    );
}

#[test]
fn f01_rebind_preserves_metadata_and_is_atomic_on_failure() {
    let mut binding = Binding::new(session(1), "original title").unwrap();
    binding.set_pinned(true);
    binding.set_custom_label(Some("operator label")).unwrap();
    binding.set_label(Some("other label")).unwrap();
    binding.set_initial_idle_confirmed(true);
    let anchor = TerminalAnchor::new(
        22,
        10,
        "synthetic creation",
        RuntimeId::new("1,-2").unwrap(),
    )
    .unwrap();
    binding.set_target(Some(anchor.clone()));
    let mut map = slot_map(binding);
    let revision = map.revision();
    let mut forks = PendingForks::default();
    let fork = pending(&map);
    forks.begin(fork, &map).unwrap();
    forks
        .observe(
            fork_id(100),
            ForkObservation {
                child: session(3),
                timestamp_ms: 10_000,
                next_offset: 101,
            },
        )
        .unwrap();
    let candidates = [accessible(3, process(10), true)];
    assert_eq!(
        forks
            .evaluate_and_apply(
                fork_id(100),
                &mut map,
                10_001,
                OwnerResolution::Live(process(10)),
                discovery(&candidates),
            )
            .unwrap(),
        ForkDecision::Rebind { child: session(3) }
    );
    assert!(forks.is_empty());
    assert_eq!(map.revision(), revision + 1);
    let followed = map.get(slot(1)).unwrap();
    assert_eq!(followed.session_id(), session(3));
    assert_eq!(followed.fork_parent(), Some(session(1)));
    assert!(followed.pinned());
    assert_eq!(followed.custom_label(), Some("operator label"));
    assert_eq!(followed.label(), Some("other label"));
    assert_eq!(followed.target(), Some(&anchor));
    assert!(!followed.initial_idle_confirmed());

    let mut map = slot_map(Binding::new(session(1), "title").unwrap());
    map.bind(
        slot(2),
        Binding::new(session(3), "existing").unwrap(),
        map.revision(),
        None,
        None,
    )
    .unwrap();
    let before = map.clone();
    let mut forks = PendingForks::default();
    forks.begin(pending(&map), &map).unwrap();
    assert_eq!(
        forks.evaluate_and_apply(
            fork_id(100),
            &mut map,
            10_000,
            OwnerResolution::Live(process(10)),
            discovery(&candidates)
        ),
        Err(ForkError::DuplicateSession)
    );
    assert_eq!(map, before);
    assert_eq!(forks.len(), 1);

    let mut raw =
        serde_json::to_value(slot_map(Binding::new(session(1), "title").unwrap())).unwrap();
    raw["revision"] = serde_json::json!(u64::MAX);
    let mut exhausted: crate::domain::SlotMap = serde_json::from_value(raw).unwrap();
    let before = exhausted.clone();
    let mut forks = PendingForks::default();
    forks.begin(pending(&exhausted), &exhausted).unwrap();
    assert_eq!(
        forks.evaluate_and_apply(
            fork_id(100),
            &mut exhausted,
            10_000,
            OwnerResolution::Live(process(10)),
            discovery(&candidates)
        ),
        Err(ForkError::RevisionExhausted)
    );
    assert_eq!(exhausted, before);
    assert_eq!(forks.len(), 1);
}

#[test]
fn f01_terminal_decisions_remove_pending_only_when_specified() {
    for (now, owner, expected, removed) in [
        (
            9_999,
            OwnerResolution::Unresolved,
            ForkDecision::ClockRegressed,
            false,
        ),
        (
            10_000,
            OwnerResolution::Unresolved,
            ForkDecision::Deferred,
            false,
        ),
        (
            40_001,
            OwnerResolution::Unresolved,
            ForkDecision::Expired,
            true,
        ),
        (
            10_000,
            OwnerResolution::Exited,
            ForkDecision::LifetimeChanged,
            true,
        ),
    ] {
        let mut map = slot_map(Binding::new(session(1), "title").unwrap());
        let mut forks = PendingForks::default();
        forks.begin(pending(&map), &map).unwrap();
        assert_eq!(
            forks
                .evaluate_and_apply(fork_id(100), &mut map, now, owner, discovery(&[]))
                .unwrap(),
            expected
        );
        assert_eq!(forks.is_empty(), removed);
    }
    let mut map = slot_map(Binding::new(session(1), "title").unwrap());
    let mut forks = PendingForks::default();
    forks.begin(pending(&map), &map).unwrap();
    map.set_pinned(slot(1), true, map.revision()).unwrap();
    assert_eq!(
        forks
            .evaluate_and_apply(
                fork_id(100),
                &mut map,
                0,
                OwnerResolution::Unresolved,
                discovery(&[])
            )
            .unwrap(),
        ForkDecision::BindingChanged
    );
    assert!(forks.is_empty());
}

#[test]
fn f01_tail_observation_flows_through_delayed_discovery_to_rebinding() {
    use super::{
        super::{
            reducer::Reducer,
            tail::{TailCursor, poll},
        },
        tail::{FakeBoundary, path},
    };

    let mut map = slot_map(Binding::new(session(1), "title").unwrap());
    let mut forks = PendingForks::default();
    forks.begin(pending(&map), &map).unwrap();
    let notice = serde_json::json!({
        "type":"session.info", "timestamp":10_000,
        "data":{"infoType":"fork", "message":format!("Forked this session into \"hidden\" ({}).", session(3))}
    });
    let mut data = b" \n".repeat(60);
    data.extend(serde_json::to_vec(&notice).unwrap());
    data.push(b'\n');
    let boundary = FakeBoundary::new(data);
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    let report = poll(&boundary, path(), 1_000, &mut cursor, &mut reducer).unwrap();
    assert_eq!(report.records, 1);
    assert_eq!(report.fork_observations.len(), 1);
    assert_eq!(
        forks
            .observe(fork_id(100), report.fork_observations[0])
            .unwrap(),
        ObservationResult::ChildRecorded
    );
    assert_eq!(
        forks
            .evaluate_and_apply(
                fork_id(100),
                &mut map,
                10_001,
                OwnerResolution::Live(process(10)),
                discovery(&[])
            )
            .unwrap(),
        ForkDecision::Deferred
    );
    let candidates = [accessible(3, process(10), true)];
    assert_eq!(
        forks
            .evaluate_and_apply(
                fork_id(100),
                &mut map,
                10_002,
                OwnerResolution::Live(process(10)),
                discovery(&candidates)
            )
            .unwrap(),
        ForkDecision::Rebind { child: session(3) }
    );
    assert_eq!(map.get(slot(1)).unwrap().fork_parent(), Some(session(1)));
    assert!(forks.is_empty());
}
