use crate::domain::{ClockSource, RefusalReason, RuntimeEpoch, Status};

use super::{epoch, sample, snapshot};
use crate::presentation::semantic::{
    Authority, Availability, EffectRequest, LatestSnapshot, Occupancy, Overlay, OverlayKind,
    Selection, SnapshotError, Token, refusal_word, resolve,
};

#[test]
fn v01_exact_palette_words_and_selection_channels() {
    let cases = [
        (Token::Off, [0, 0, 0], [97, 97, 97], [255, 255, 255], "OFF"),
        (
            Token::Idle,
            [255, 255, 255],
            [255, 255, 255],
            [0, 0, 0],
            "IDLE",
        ),
        (
            Token::Working,
            [32, 112, 255],
            [32, 112, 255],
            [0, 0, 0],
            "WORKING",
        ),
        (
            Token::Complete,
            [0, 208, 96],
            [0, 208, 96],
            [0, 0, 0],
            "DONE",
        ),
        (
            Token::Waiting,
            [255, 170, 0],
            [255, 170, 0],
            [0, 0, 0],
            "WAITING",
        ),
        (
            Token::Error,
            [255, 48, 48],
            [255, 48, 48],
            [0, 0, 0],
            "ERROR",
        ),
        (
            Token::Unknown,
            [96, 125, 139],
            [96, 125, 139],
            [0, 0, 0],
            "UNKNOWN",
        ),
        (
            Token::Empty,
            [0, 0, 0],
            [48, 48, 48],
            [176, 176, 176],
            "EMPTY",
        ),
        (
            Token::Stale,
            [93, 64, 55],
            [93, 64, 55],
            [255, 213, 79],
            "STALE",
        ),
        (
            Token::Exited,
            [93, 64, 55],
            [93, 64, 55],
            [255, 213, 79],
            "EXITED",
        ),
        (
            Token::PidReused,
            [93, 64, 55],
            [93, 64, 55],
            [255, 213, 79],
            "PID REUSED",
        ),
        (
            Token::Unresolved,
            [93, 64, 55],
            [93, 64, 55],
            [255, 213, 79],
            "NOT FOUND",
        ),
        (
            Token::Busy,
            [106, 27, 154],
            [106, 27, 154],
            [255, 255, 255],
            "BUSY",
        ),
        (
            Token::Refused,
            [191, 54, 12],
            [191, 54, 12],
            [255, 255, 255],
            "REFUSED",
        ),
        (
            Token::Neutral,
            [96, 125, 139],
            [96, 125, 139],
            [0, 0, 0],
            "OFFLINE",
        ),
    ];
    for (token, rgb, fill, text, word) in cases {
        let style = token.style();
        assert_eq!(
            (style.rgb, style.fill, style.text, style.word),
            (rgb, fill, text, word)
        );
    }
    let confirmed = Selection::Confirmed.style().unwrap();
    assert_eq!(
        (confirmed.rgb, confirmed.word, confirmed.dashed),
        ([255, 255, 255], "SELECTED", false)
    );
    let pending = Selection::Pending.style().unwrap();
    assert_eq!(
        (pending.rgb, pending.word, pending.dashed),
        ([0, 229, 255], "PENDING", true)
    );
    assert!(Selection::None.style().is_none());
}

#[test]
fn v01_refusal_detail_words_are_exact() {
    use RefusalReason::*;
    for (reason, word) in [
        (Disabled, "DISABLED"),
        (Unsupported, "UNSUPPORTED"),
        (UnknownUi, "UNKNOWN UI"),
        (StaleState, "STALE"),
        (ClockMismatch, "CLOCK"),
        (WrongTarget, "WRONG TARGET"),
        (ChangedBinding, "REBOUND"),
        (ChangedGeneration, "OUTDATED"),
        (ChangedRun, "NEW RUN"),
        (ChangedDraft, "DRAFT EDIT"),
        (ChangedDialog, "NEW DIALOG"),
        (CommandDraft, "COMMAND"),
        (EmptyDraft, "EMPTY DRAFT"),
        (Busy, "BUSY"),
        (BroadApproval, "TOO BROAD"),
        (Duplicate, "DUPLICATE"),
        (IndeterminateDelivery, "UNCERTAIN"),
        (PendingInput, "INPUT WAIT"),
        (Unavailable, "UNAVAILABLE"),
        (UnsafeContext, "UNSAFE"),
        (ExpiredIntent, "EXPIRED"),
    ] {
        assert_eq!(refusal_word(reason), word);
    }
}

#[test]
fn v01_snapshot_holder_rejects_nil_wrong_epoch_and_nonincreasing_generation() {
    let nil = RuntimeEpoch::parse_str("00000000-0000-0000-0000-000000000000").unwrap();
    assert!(matches!(
        LatestSnapshot::new(nil),
        Err(SnapshotError::NilExpectedEpoch)
    ));
    let mut holder = LatestSnapshot::new(epoch()).unwrap();
    let mut value = snapshot();
    value.generation = 0;
    holder.publish(value.clone()).unwrap();
    assert_eq!(
        holder.publish(value.clone()),
        Err(SnapshotError::NonIncreasingGeneration)
    );
    value.generation = 2;
    holder.publish(value.clone()).unwrap();
    value.generation = 1;
    assert_eq!(
        holder.publish(value.clone()),
        Err(SnapshotError::NonIncreasingGeneration)
    );
    value.generation = u64::MAX;
    value.epoch = nil;
    assert_eq!(holder.publish(value), Err(SnapshotError::WrongEpoch));
    assert_eq!(holder.latest().unwrap().generation, 2);
}

#[test]
fn v01_priority_clears_context_and_never_mutates_provider() {
    let refused = Some(Overlay {
        kind: OverlayKind::Refused(RefusalReason::Disabled),
        generation: 1,
        started: sample(0),
    });
    for (occupancy, availability, overlay, expected) in [
        (Occupancy::Empty, Availability::Busy, refused, Token::Empty),
        (Occupancy::Bound, Availability::Stale, refused, Token::Stale),
        (
            Occupancy::Bound,
            Availability::Exited,
            refused,
            Token::Exited,
        ),
        (
            Occupancy::Bound,
            Availability::PidReused,
            refused,
            Token::PidReused,
        ),
        (
            Occupancy::Bound,
            Availability::Unresolved,
            refused,
            Token::Unresolved,
        ),
        (
            Occupancy::Bound,
            Availability::Busy,
            refused,
            Token::Refused,
        ),
        (Occupancy::Bound, Availability::Busy, None, Token::Busy),
        (Occupancy::Bound, Availability::Live, None, Token::Error),
    ] {
        let mut source = snapshot();
        source.slots[0].occupancy = occupancy;
        source.slots[0].availability = availability;
        source.slots[0].overlay = overlay;
        source.slots[0].status = Status::Error;
        source.slots[0].selection = Selection::Confirmed;
        source.slots[0].cursor = true;
        source.slots[0].authority = Authority::ActionsGranted;
        let original = source.clone();
        let slot = resolve(&source, sample(1))[0];
        assert_eq!(slot.token, expected);
        if expected.clears_context() {
            assert_eq!(slot.selection, Selection::None);
            assert!(!slot.cursor);
            assert_eq!(slot.authority, Authority::Disarmed);
            assert_eq!(slot.overlay, None);
            assert_eq!(slot.effect, EffectRequest::Steady);
        }
        assert_eq!(source, original);
    }
    let mut source = snapshot();
    source.connected = false;
    source.slots[0].occupancy = Occupancy::Empty;
    assert_eq!(resolve(&source, sample(0))[0].token, Token::Neutral);
    source.connected = true;
    let mut wrong_clock = sample(0);
    wrong_clock.stamp.epoch =
        RuntimeEpoch::parse_str("22222222-2222-4222-8222-222222222222").unwrap();
    assert_eq!(resolve(&source, wrong_clock)[0].token, Token::Neutral);
    wrong_clock = sample(0);
    wrong_clock.stamp.frequency = 0;
    assert_eq!(resolve(&source, wrong_clock)[0].token, Token::Neutral);
}

#[test]
fn v01_overlay_exact_expiry_generation_and_clock_cuts() {
    let mut source = snapshot();
    source.slots[0].overlay = Some(Overlay {
        kind: OverlayKind::Refused(RefusalReason::Busy),
        generation: 1,
        started: sample(100),
    });
    assert_eq!(resolve(&source, sample(2099))[0].token, Token::Refused);
    assert_eq!(resolve(&source, sample(2100))[0].token, Token::Idle);
    assert_eq!(resolve(&source, sample(2101))[0].token, Token::Idle);
    source.slots[0].overlay.as_mut().unwrap().generation = 2;
    assert_eq!(resolve(&source, sample(101))[0].token, Token::Idle);
    source.slots[0].overlay.as_mut().unwrap().generation = 1;
    assert_eq!(resolve(&source, sample(99))[0].token, Token::Idle);
    let mut changed_source = sample(101);
    changed_source.source = ClockSource::WindowsQpc;
    assert_eq!(resolve(&source, changed_source)[0].token, Token::Idle);
    let mut changed_frequency = sample(101);
    changed_frequency.stamp.frequency = 2000;
    assert_eq!(resolve(&source, changed_frequency)[0].token, Token::Idle);

    source.slots[0]
        .overlay
        .as_mut()
        .unwrap()
        .started
        .stamp
        .frequency = 1_000_000;
    let mut precise = sample(0);
    precise.stamp.frequency = 1_000_000;
    precise.stamp.ticks = 100 + 1_999_999;
    assert_eq!(resolve(&source, precise)[0].token, Token::Refused);
    precise.stamp.ticks += 1;
    assert_eq!(resolve(&source, precise)[0].token, Token::Idle);
}

#[test]
fn v01_ambiguous_marker_kinds_are_independent_and_include_stale_duplicates() {
    let mut source = snapshot();
    source.slots[0].selection = Selection::Confirmed;
    source.slots[1].selection = Selection::Confirmed;
    source.slots[1].availability = Availability::Stale;
    source.slots[2].selection = Selection::Pending;
    for slot in &mut source.slots[..3] {
        slot.cursor = true;
        slot.authority = Authority::ActionsGranted;
    }
    let visible = resolve(&source, sample(0));
    assert_eq!(visible[0].selection, Selection::None);
    assert!(!visible[0].cursor);
    assert_eq!(visible[0].authority, Authority::Disarmed);
    assert_eq!(visible[1].selection, Selection::None);
    assert_eq!(visible[2].selection, Selection::Pending);
    assert!(visible[2].cursor);
    assert_eq!(visible[2].authority, Authority::ActionsGranted);

    source.slots[0].selection = Selection::Pending;
    source.slots[1].selection = Selection::Pending;
    source.slots[2].selection = Selection::Confirmed;
    let visible = resolve(&source, sample(0));
    assert_eq!(visible[0].selection, Selection::None);
    assert_eq!(visible[2].selection, Selection::Confirmed);
    source.slots[1].selection = Selection::None;
    let visible = resolve(&source, sample(0));
    assert_eq!(visible[0].selection, Selection::Pending);
    assert_eq!(visible[2].selection, Selection::Confirmed);
}

#[test]
fn v01_attention_and_pulse_requests_follow_visible_semantics() {
    let mut source = snapshot();
    source.slots[0].selection = Selection::Confirmed;
    source.slots[2].overlay = Some(Overlay {
        kind: OverlayKind::Attention,
        generation: 1,
        started: sample(0),
    });
    let visible = resolve(&source, sample(1));
    assert_eq!(
        visible.map(|slot| slot.effect),
        [
            EffectRequest::SlowPulse,
            EffectRequest::SlowPulse,
            EffectRequest::Attention,
            EffectRequest::Attention,
            EffectRequest::Attention,
            EffectRequest::Steady,
        ]
    );
}
