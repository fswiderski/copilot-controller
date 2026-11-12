use crate::domain::{Action, ClockSample, ClockSource, RefusalReason as R, Status};

use super::super::{
    hook::PendingKind,
    policy::{
        DialogSelection, DraftKind, Evidence, Intent, Reservations, UiEvidence, structural_guard,
        validate_final, validate_initial,
    },
};
use super::{clock, epoch, generations, process, session, slot};

fn composer(draft: DraftKind, busy: bool) -> UiEvidence {
    UiEvidence::Composer {
        draft,
        busy,
        draft_fingerprint: [1; 32],
        context_fingerprint: [2; 32],
    }
}

fn dialog(selection: DialogSelection, permission: bool, busy: bool) -> UiEvidence {
    UiEvidence::Dialog {
        selection,
        permission,
        busy,
        dialog_fingerprint: [3; 32],
        dialog_token: [4; 32],
    }
}

fn context(action: Action, now: ClockSample) -> (Intent, Evidence) {
    let (status, ui, pending_kind) = match action {
        Action::Stop => (
            Status::Working,
            composer(DraftKind::Conversation, true),
            None,
        ),
        Action::Accept => (
            Status::Waiting,
            dialog(DialogSelection::SingleRequest, true, false),
            Some(PendingKind::PermissionPrompt),
        ),
        Action::Send => (Status::Idle, composer(DraftKind::Conversation, false), None),
        _ => (Status::Idle, composer(DraftKind::Empty, false), None),
    };
    (
        Intent {
            action,
            slot: slot(1),
            session: session(1),
            process: process(10),
            generations: generations(),
            occurred_at: now,
        },
        Evidence {
            slot: slot(1),
            session: session(1),
            process: process(10),
            generations: generations(),
            observed_at: now,
            controls_enabled: true,
            stopped: false,
            state_ready: true,
            verified: true,
            selected: true,
            foreground: true,
            presentation_current: true,
            pending_input: false,
            indeterminate_delivery: false,
            status,
            pending_kind,
            pending_at_ms: Some(5),
            run_token: Some(10),
            ui,
        },
    )
}

#[test]
fn p01_every_action_guard_and_shared_refusals() {
    let empty = composer(DraftKind::Empty, false);
    let conversation = composer(DraftKind::Conversation, false);
    let command = composer(DraftKind::Command, false);
    let ordinary = dialog(DialogSelection::Ordinary, false, false);
    let single = dialog(DialogSelection::SingleRequest, true, true);
    assert_eq!(
        structural_guard(Action::None, Status::Off, None, UiEvidence::Unknown),
        Err(R::Unsupported)
    );
    for action in [
        Action::Autopilot,
        Action::Accept,
        Action::Stop,
        Action::Fork,
        Action::Send,
        Action::ModelPicker,
        Action::Up,
        Action::Right,
        Action::Down,
        Action::Left,
    ] {
        assert_eq!(
            structural_guard(action, Status::Off, None, UiEvidence::Unknown),
            Err(R::UnknownUi)
        );
        assert_eq!(
            structural_guard(action, Status::Off, None, empty),
            Err(R::Unavailable)
        );
        assert_eq!(
            structural_guard(action, Status::Unknown, None, empty),
            Err(R::Unavailable)
        );
        let (_, evidence) = context(action, clock(0, 10));
        assert_eq!(
            structural_guard(action, evidence.status, evidence.pending_kind, evidence.ui),
            Ok(()),
            "{action:?}"
        );
    }
    for action in [Action::Up, Action::Right, Action::Down, Action::Left] {
        for status in [
            Status::Idle,
            Status::Working,
            Status::Waiting,
            Status::Complete,
            Status::Error,
        ] {
            assert_eq!(structural_guard(action, status, None, ordinary), Ok(()));
        }
    }
    for (status, ui, expected) in [
        (Status::Working, composer(DraftKind::Empty, true), Ok(())),
        (Status::Waiting, composer(DraftKind::Empty, true), Ok(())),
        (Status::Waiting, ordinary, Ok(())),
        (Status::Working, ordinary, Err(R::UnsafeContext)),
        (
            Status::Idle,
            composer(DraftKind::Empty, true),
            Err(R::UnsafeContext),
        ),
        (Status::Complete, ordinary, Err(R::UnsafeContext)),
    ] {
        assert_eq!(structural_guard(Action::Stop, status, None, ui), expected);
    }
    for (status, pending, ui, expected) in [
        (Status::Error, None, ordinary, Ok(())),
        (Status::Idle, None, ordinary, Ok(())),
        (
            Status::Working,
            Some(PendingKind::PermissionPrompt),
            single,
            Ok(()),
        ),
        (
            Status::Waiting,
            None,
            dialog(DialogSelection::Unknown, false, false),
            Err(R::UnknownUi),
        ),
        (
            Status::Waiting,
            None,
            dialog(DialogSelection::BroadApproval, false, false),
            Err(R::BroadApproval),
        ),
        (
            Status::Waiting,
            None,
            dialog(DialogSelection::Ordinary, true, false),
            Err(R::BroadApproval),
        ),
        (
            Status::Waiting,
            Some(PendingKind::PermissionPrompt),
            ordinary,
            Err(R::BroadApproval),
        ),
        (Status::Waiting, None, empty, Err(R::UnsafeContext)),
    ] {
        assert_eq!(
            structural_guard(Action::Accept, status, pending, ui),
            expected
        );
    }
    for (status, ui, expected) in [
        (Status::Idle, conversation, Ok(())),
        (Status::Complete, conversation, Ok(())),
        (Status::Idle, empty, Err(R::EmptyDraft)),
        (Status::Idle, command, Err(R::CommandDraft)),
        (
            Status::Idle,
            composer(DraftKind::Conversation, true),
            Err(R::Busy),
        ),
        (Status::Working, conversation, Err(R::Busy)),
        (Status::Idle, ordinary, Err(R::UnsafeContext)),
    ] {
        assert_eq!(structural_guard(Action::Send, status, None, ui), expected);
    }
    for action in [Action::Autopilot, Action::Fork, Action::ModelPicker] {
        for (status, ui, expected) in [
            (Status::Idle, empty, Ok(())),
            (Status::Complete, empty, Ok(())),
            (Status::Idle, conversation, Err(R::UnsafeContext)),
            (Status::Idle, command, Err(R::CommandDraft)),
            (Status::Idle, composer(DraftKind::Empty, true), Err(R::Busy)),
            (Status::Working, empty, Err(R::Busy)),
            (Status::Idle, ordinary, Err(R::UnsafeContext)),
        ] {
            assert_eq!(
                structural_guard(action, status, None, ui),
                expected,
                "{action:?}"
            );
        }
    }
}

#[test]
fn p02_initial_order_identity_generations_and_target_refusals() {
    let now = clock(100, 10);
    let (intent, evidence) = context(Action::Send, now);
    type EvidenceChange = (R, fn(&mut Evidence));
    let changes: [EvidenceChange; 14] = [
        (R::Disabled, |e| e.controls_enabled = false),
        (R::IndeterminateDelivery, |e| {
            e.indeterminate_delivery = true
        }),
        (R::PendingInput, |e| e.pending_input = true),
        (R::StaleState, |e| e.stopped = true),
        (R::ChangedBinding, |e| e.slot = slot(2)),
        (R::ChangedBinding, |e| e.session = session(2)),
        (R::ChangedBinding, |e| e.generations.binding += 1),
        (R::ChangedGeneration, |e| e.generations.connection += 1),
        (R::ChangedGeneration, |e| e.generations.profile += 1),
        (R::ChangedGeneration, |e| e.generations.selection += 1),
        (R::ChangedGeneration, |e| e.generations.presentation += 1),
        (R::ChangedGeneration, |e| e.presentation_current = false),
        (R::WrongTarget, |e| e.process = process(11)),
        (R::StaleState, |e| e.state_ready = false),
    ];
    for (reason, change) in changes {
        let mut current = evidence;
        change(&mut current);
        assert_eq!(validate_initial(&intent, &current, now), Err(reason));
        assert_eq!(
            validate_final(&intent, &evidence, &current, now),
            Err(reason)
        );
    }
    for change in [
        |e: &mut Evidence| e.verified = false,
        |e: &mut Evidence| e.selected = false,
        |e: &mut Evidence| e.foreground = false,
    ] {
        let mut current = evidence;
        change(&mut current);
        assert_eq!(
            validate_initial(&intent, &current, now),
            Err(R::WrongTarget)
        );
    }
    for process_override in [
        crate::domain::NativeProcessIdentity {
            pid: 0,
            ..intent.process
        },
        crate::domain::NativeProcessIdentity {
            creation_time_100ns: 0,
            ..intent.process
        },
    ] {
        let mut bad_intent = intent;
        let mut bad_evidence = evidence;
        bad_intent.process = process_override;
        bad_evidence.process = process_override;
        assert_eq!(
            validate_initial(&bad_intent, &bad_evidence, now),
            Err(R::WrongTarget)
        );
    }

    let mut conflicting = evidence;
    conflicting.controls_enabled = false;
    conflicting.indeterminate_delivery = true;
    conflicting.pending_input = true;
    conflicting.stopped = true;
    assert_eq!(
        validate_initial(&intent, &conflicting, clock(1_000, 10)),
        Err(R::Disabled)
    );
    conflicting.controls_enabled = true;
    assert_eq!(
        validate_initial(&intent, &conflicting, clock(1_000, 10)),
        Err(R::IndeterminateDelivery)
    );
    conflicting.indeterminate_delivery = false;
    assert_eq!(
        validate_initial(&intent, &conflicting, clock(1_000, 10)),
        Err(R::PendingInput)
    );
    conflicting.pending_input = false;
    assert_eq!(
        validate_initial(&intent, &conflicting, clock(1_000, 10)),
        Err(R::ExpiredIntent)
    );
    conflicting.slot = slot(2);
    assert_eq!(
        validate_initial(&intent, &conflicting, now),
        Err(R::StaleState)
    );
    conflicting.stopped = false;
    conflicting.generations.connection += 1;
    conflicting.verified = false;
    assert_eq!(
        validate_initial(&intent, &conflicting, now),
        Err(R::ChangedBinding)
    );
    conflicting.slot = intent.slot;
    assert_eq!(
        validate_initial(&intent, &conflicting, now),
        Err(R::ChangedGeneration)
    );
    conflicting.generations = intent.generations;
    conflicting.state_ready = false;
    assert_eq!(
        validate_initial(&intent, &conflicting, now),
        Err(R::WrongTarget)
    );
    conflicting.verified = true;
    conflicting.ui = UiEvidence::Unknown;
    assert_eq!(
        validate_initial(&intent, &conflicting, now),
        Err(R::StaleState)
    );

    let (direction, mut dir_evidence) = context(Action::Up, now);
    dir_evidence.state_ready = false;
    assert_eq!(validate_initial(&direction, &dir_evidence, now), Ok(()));
    let (stop, mut stop_evidence) = context(Action::Stop, now);
    stop_evidence.run_token = None;
    assert_eq!(
        validate_initial(&stop, &stop_evidence, now),
        Err(R::ChangedRun)
    );
    stop_evidence.ui = composer(DraftKind::Conversation, false);
    assert_eq!(
        validate_initial(&stop, &stop_evidence, now),
        Err(R::UnsafeContext)
    );
}

#[test]
fn p02_exact_time_boundaries_all_clock_errors_and_observation_order() {
    let start = clock(0, 10_000_000_000);
    let (intent, evidence) = context(Action::Send, start);
    assert_eq!(
        validate_initial(&intent, &evidence, clock(30_000_000_000, 10_000_000_000)),
        Ok(())
    );
    assert_eq!(
        validate_initial(&intent, &evidence, clock(30_000_000_001, 10_000_000_000)),
        Err(R::ExpiredIntent)
    );
    let mut recent_intent = intent;
    recent_intent.occurred_at = clock(30_000_000_001, 10_000_000_000);
    assert_eq!(
        validate_initial(&recent_intent, &evidence, recent_intent.occurred_at),
        Err(R::StaleState)
    );
    let base = clock(100, 10);
    let (intent, evidence) = context(Action::Send, base);
    let mut different_source = base;
    different_source.source = ClockSource::WindowsQpc;
    let mut different_epoch = base;
    different_epoch.stamp.epoch = epoch(2);
    let mut nil_epoch = base;
    nil_epoch.stamp.epoch = epoch(0);
    let mut zero_frequency = base;
    zero_frequency.stamp.frequency = 0;
    let mut different_frequency = base;
    different_frequency.stamp.frequency = 11;
    let regressed = clock(99, 10);
    for bad in [
        different_source,
        different_epoch,
        nil_epoch,
        zero_frequency,
        different_frequency,
        regressed,
    ] {
        assert_eq!(
            validate_initial(&intent, &evidence, bad),
            Err(R::ClockMismatch)
        );
        let mut new_intent = intent;
        new_intent.occurred_at = bad;
        let mut new_evidence = evidence;
        new_evidence.observed_at = base;
        assert_eq!(
            validate_initial(&new_intent, &new_evidence, bad),
            Err(R::ClockMismatch)
        );
    }
    let mut original = evidence;
    original.observed_at = clock(102, 10);
    let current = evidence;
    assert_eq!(
        validate_final(&intent, &original, &current, clock(102, 10)),
        Err(R::ClockMismatch)
    );
    let mut invalid_original = evidence;
    invalid_original.controls_enabled = false;
    assert_eq!(
        validate_final(&intent, &invalid_original, &evidence, base),
        Err(R::Disabled)
    );
    let mut stopped = evidence;
    stopped.stopped = true;
    stopped.observed_at = different_source;
    assert_eq!(
        validate_initial(&intent, &stopped, base),
        Err(R::StaleState)
    );
    let (nil_intent, nil_evidence) = context(Action::Send, nil_epoch);
    assert_eq!(
        validate_initial(&nil_intent, &nil_evidence, nil_epoch),
        Err(R::ClockMismatch)
    );
}

#[test]
fn p02_final_action_specific_identities_are_rechecked() {
    let now = clock(100, 10);
    let (intent, original) = context(Action::Stop, now);
    let mut current = original;
    current.run_token = Some(11);
    assert_eq!(
        validate_final(&intent, &original, &current, now),
        Err(R::ChangedRun)
    );

    let (intent, original) = context(Action::Send, now);
    let mut current = original;
    if let UiEvidence::Composer {
        draft_fingerprint, ..
    } = &mut current.ui
    {
        draft_fingerprint[0] ^= 1;
    }
    assert_eq!(
        validate_final(&intent, &original, &current, now),
        Err(R::ChangedDraft)
    );

    let (intent, original) = context(Action::Accept, now);
    let changes: [fn(&mut Evidence); 4] = [
        |e| {
            if let UiEvidence::Dialog {
                dialog_fingerprint, ..
            } = &mut e.ui
            {
                dialog_fingerprint[0] ^= 1;
            }
        },
        |e| {
            if let UiEvidence::Dialog { dialog_token, .. } = &mut e.ui {
                dialog_token[0] ^= 1;
            }
        },
        |e| e.pending_at_ms = Some(6),
        |e| e.pending_kind = Some(PendingKind::ElicitationDialog),
    ];
    for change in changes {
        let mut current = original;
        change(&mut current);
        assert_eq!(
            validate_final(&intent, &original, &current, now),
            Err(R::ChangedDialog)
        );
    }
    for action in [Action::Autopilot, Action::Fork, Action::ModelPicker] {
        let (intent, original) = context(action, now);
        let mut current = original;
        if let UiEvidence::Composer {
            context_fingerprint,
            ..
        } = &mut current.ui
        {
            context_fingerprint[0] ^= 1;
        }
        assert_eq!(
            validate_final(&intent, &original, &current, now),
            Err(R::ChangedDraft)
        );
    }
    let (intent, original) = context(Action::Up, now);
    let mut current = original;
    current.ui = dialog(DialogSelection::BroadApproval, true, true);
    current.state_ready = false;
    assert_eq!(validate_final(&intent, &original, &current, now), Ok(()));
}

fn reserve(
    reservations: &mut Reservations,
    action: Action,
    id: u128,
    when: ClockSample,
    run: u64,
    pending: u64,
    token: u8,
) -> Result<(), R> {
    let (mut intent, mut evidence) = context(action, when);
    intent.session = session(id);
    evidence.session = session(id);
    evidence.run_token = Some(run);
    evidence.pending_at_ms = Some(pending);
    if let UiEvidence::Dialog { dialog_token, .. } = &mut evidence.ui {
        *dialog_token = [token; 32];
    }
    reservations.validate_and_reserve(&intent, &evidence, &evidence, when)
}

#[test]
fn p03_stop_run_cooldown_and_clock_cut_never_clear_suppression() {
    let mut reservations = Reservations::default();
    assert_eq!(
        reserve(&mut reservations, Action::Stop, 1, clock(100, 10), 1, 0, 0),
        Ok(())
    );
    assert_eq!(
        reserve(
            &mut reservations,
            Action::Stop,
            1,
            clock(1_000, 10),
            1,
            0,
            0
        ),
        Err(R::Duplicate)
    );
    assert_eq!(
        reserve(&mut reservations, Action::Stop, 1, clock(129, 10), 2, 0, 0),
        Err(R::Duplicate)
    );
    assert_eq!(
        reserve(&mut reservations, Action::Stop, 1, clock(130, 10), 2, 0, 0),
        Ok(())
    );
    assert_eq!(
        reserve(&mut reservations, Action::Stop, 1, clock(131, 10), 2, 0, 0),
        Err(R::Duplicate)
    );
    let mut cut = clock(160, 10);
    cut.stamp.epoch = epoch(2);
    assert_eq!(
        reserve(&mut reservations, Action::Stop, 1, cut, 3, 0, 0),
        Err(R::ClockMismatch)
    );
    assert_eq!(
        reserve(&mut reservations, Action::Stop, 1, clock(129, 10), 3, 0, 0),
        Err(R::ClockMismatch)
    );
    assert_eq!(
        reserve(&mut reservations, Action::Stop, 1, clock(159, 10), 3, 0, 0),
        Err(R::Duplicate)
    );
    assert_eq!(
        reserve(&mut reservations, Action::Stop, 1, clock(160, 10), 3, 0, 0),
        Ok(())
    );
}

#[test]
fn p03_fractional_tick_cooldown_and_clock_failures_are_exact() {
    let frequency = 10_000_000_000;
    let mut reservations = Reservations::default();
    assert_eq!(
        reserve(
            &mut reservations,
            Action::Stop,
            1,
            clock(0, frequency),
            1,
            0,
            0
        ),
        Ok(())
    );
    assert_eq!(
        reserve(
            &mut reservations,
            Action::Stop,
            1,
            clock(3 * frequency - 1, frequency),
            2,
            0,
            0
        ),
        Err(R::Duplicate)
    );
    let mut source_cut = clock(3 * frequency, frequency);
    source_cut.source = ClockSource::WindowsQpc;
    let mut epoch_cut = clock(3 * frequency, frequency);
    epoch_cut.stamp.epoch = epoch(2);
    for changed in [
        source_cut,
        epoch_cut,
        clock(3 * frequency, frequency - 1),
        clock(3 * frequency, 0),
    ] {
        assert_eq!(
            reserve(&mut reservations, Action::Stop, 1, changed, 2, 0, 0),
            Err(R::ClockMismatch)
        );
    }
    assert_eq!(
        reserve(
            &mut reservations,
            Action::Stop,
            1,
            clock(3 * frequency, frequency),
            2,
            0,
            0
        ),
        Ok(())
    );
}

#[test]
fn p03_accept_request_token_cooldown_and_six_session_capacity() {
    let mut reservations = Reservations::default();
    assert_eq!(
        reserve(&mut reservations, Action::Accept, 1, clock(0, 10), 1, 50, 1),
        Ok(())
    );
    assert_eq!(
        reserve(
            &mut reservations,
            Action::Accept,
            1,
            clock(29, 10),
            1,
            50,
            1
        ),
        Err(R::Duplicate)
    );
    assert_eq!(
        reserve(
            &mut reservations,
            Action::Accept,
            1,
            clock(30, 10),
            1,
            50,
            1
        ),
        Ok(())
    );
    assert_eq!(
        reserve(
            &mut reservations,
            Action::Accept,
            1,
            clock(31, 10),
            1,
            50,
            2
        ),
        Ok(())
    );
    assert_eq!(
        reserve(
            &mut reservations,
            Action::Accept,
            1,
            clock(32, 10),
            1,
            51,
            2
        ),
        Ok(())
    );
    assert_eq!(
        reserve(
            &mut reservations,
            Action::Accept,
            1,
            clock(33, 10),
            1,
            50,
            1
        ),
        Err(R::Duplicate)
    );
    let mut new_source = clock(33, 10);
    new_source.source = ClockSource::WindowsQpc;
    assert_eq!(
        reserve(&mut reservations, Action::Accept, 1, new_source, 1, 52, 3),
        Err(R::ClockMismatch)
    );
    assert_eq!(
        reserve(
            &mut reservations,
            Action::Accept,
            1,
            clock(33, 10),
            1,
            51,
            2
        ),
        Err(R::Duplicate)
    );
    for id in 2..=6 {
        assert_eq!(
            reserve(&mut reservations, Action::Stop, id, clock(40, 10), 1, 0, 0),
            Ok(())
        );
    }
    assert_eq!(reservations.len(), 6);
    assert_eq!(
        reserve(&mut reservations, Action::Stop, 7, clock(40, 10), 1, 0, 0),
        Err(R::Unavailable)
    );
    assert_eq!(
        reserve(&mut reservations, Action::Up, 7, clock(40, 10), 1, 0, 0),
        Ok(())
    );
    assert_eq!(reservations.len(), 6);
    let (intent, mut bad) = context(Action::Stop, clock(40, 10));
    bad.controls_enabled = false;
    assert_eq!(
        reservations.validate_and_reserve(&intent, &bad, &bad, clock(40, 10)),
        Err(R::Disabled)
    );
}

#[test]
fn p03_accept_capacity_fails_closed_without_evicting_live_suppression() {
    const CAPACITY: u8 = 64;

    let mut reservations = Reservations::default();
    for token in 0..CAPACITY {
        assert_eq!(
            reserve(
                &mut reservations,
                Action::Accept,
                1,
                clock(0, 10),
                1,
                50,
                token
            ),
            Ok(())
        );
    }

    for pending in 1_000..2_000 {
        assert_eq!(
            reserve(
                &mut reservations,
                Action::Accept,
                1,
                clock(0, 10),
                1,
                pending,
                255
            ),
            Err(R::Unavailable)
        );
    }
    for token in 0..CAPACITY {
        assert_eq!(
            reserve(
                &mut reservations,
                Action::Accept,
                1,
                clock(0, 10),
                1,
                50,
                token
            ),
            Err(R::Duplicate)
        );
    }
    assert_eq!(
        reserve(&mut reservations, Action::Accept, 2, clock(0, 10), 1, 50, 0),
        Ok(())
    );
}

#[test]
fn p03_accept_capacity_reclaims_only_expired_reservations() {
    const CAPACITY: u8 = 64;

    let mut reservations = Reservations::default();
    for token in 0..CAPACITY {
        assert_eq!(
            reserve(
                &mut reservations,
                Action::Accept,
                1,
                clock(0, 10),
                1,
                50,
                token
            ),
            Ok(())
        );
    }
    assert_eq!(
        reserve(
            &mut reservations,
            Action::Accept,
            1,
            clock(30, 10),
            1,
            51,
            255
        ),
        Ok(())
    );
    assert_eq!(
        reserve(
            &mut reservations,
            Action::Accept,
            1,
            clock(30, 10),
            1,
            50,
            0
        ),
        Ok(())
    );
}
