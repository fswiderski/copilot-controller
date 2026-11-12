use std::{
    cell::Cell,
    collections::BTreeMap,
    io::{self, Read},
    path::{Path, PathBuf},
    rc::Rc,
};

use super::{event, sample};
use crate::presentation::{
    assignment::{
        AssignmentError, CandidateBlocker, EXPORT_FILENAME, ExportError, ExportOutcome,
        MAX_RECORDED_BYTES, PrivateExportRoot, export_candidate, recorded_assignment,
        recorded_assignment_for_profile,
    },
    input::{Intent, PhysicalEvent, PhysicalEventKind},
};

fn document(device: &str, intent: serde_json::Value, events: &[PhysicalEvent]) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "device": device,
        "intent": intent,
        "adjacency_confirmed": false,
        "events": events,
    }))
    .unwrap()
}

fn valid_press() -> Vec<u8> {
    document(
        "ajazz",
        serde_json::json!({"select_slot": 1}),
        &[
            event("button_0", PhysicalEventKind::Press, 0),
            event("button_0", PhysicalEventKind::Release, 50),
        ],
    )
}

#[test]
fn i03_valid_assignment_is_candidate_only_and_never_contains_recorded_data() {
    let candidate = recorded_assignment(valid_press().as_slice()).unwrap();
    assert_eq!(candidate.profile(), "akp03-original");
    assert_eq!(
        candidate.binding().intent,
        Intent::SelectSlot(crate::domain::Slot::new(1).unwrap())
    );
    assert!(
        candidate
            .blocking_reasons()
            .contains(&CandidateBlocker::CompleteScopeAndNativeInputEvidenceRequired)
    );
    assert!(
        candidate
            .blocking_reasons()
            .contains(&CandidateBlocker::ControlNotNormalizedByProfileAdapter)
    );
    let json = serde_json::to_string(&candidate).unwrap();
    for forbidden in [
        "events",
        "ticks",
        "stamp",
        "epoch",
        "firmware",
        "observation_ref",
        "process",
        "actions_granted",
    ] {
        assert!(!json.contains(forbidden), "{forbidden}");
    }
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["activation"], "candidate_only");
    assert_eq!(value["command_ready"], false);
    assert_eq!(value["binding"]["control"], "button_0");

    let turn = document(
        "micro",
        serde_json::json!("move_cursor"),
        &[event("encoder_turn", PhysicalEventKind::Turn(-1), 0)],
    );
    let candidate = recorded_assignment(turn.as_slice()).unwrap();
    assert_eq!(candidate.profile(), "micro-2-pro");
    assert_eq!(candidate.binding().intent, Intent::MoveCursor);
    assert!(
        !candidate
            .blocking_reasons()
            .contains(&CandidateBlocker::ControlNotNormalizedByProfileAdapter)
    );

    let rev2 = recorded_assignment_for_profile(valid_press().as_slice(), "akp03e-rev2").unwrap();
    assert_eq!(rev2.profile(), "akp03e-rev2");
    assert!(
        !rev2
            .blocking_reasons()
            .contains(&CandidateBlocker::ControlNotNormalizedByProfileAdapter)
    );
    assert!(matches!(
        recorded_assignment_for_profile(valid_press().as_slice(), "micro-2-pro"),
        Err(AssignmentError::ProfileMismatch)
    ));
}

#[test]
fn i03_exact_one_gesture_no_overlaps_duplicates_or_unsafe_roles() {
    let press = event("button_0", PhysicalEventKind::Press, 0);
    let release = event("button_0", PhysicalEventKind::Release, 50);
    for events in [
        vec![],
        vec![press.clone()],
        vec![release.clone()],
        vec![press.clone(), press.clone(), release.clone()],
        vec![
            press.clone(),
            event("button_1", PhysicalEventKind::Release, 1),
        ],
        vec![release.clone(), press.clone()],
        vec![event("button_0", PhysicalEventKind::Turn(2), 0)],
        vec![
            press.clone(),
            event("button_1", PhysicalEventKind::Press, 1),
            release.clone(),
        ],
    ] {
        let bytes = document("ajazz", serde_json::json!({"select_slot":1}), &events);
        assert!(recorded_assignment(bytes.as_slice()).is_err());
    }
    let nonunit = document(
        "micro",
        serde_json::json!("move_cursor"),
        &[event("encoder_turn", PhysicalEventKind::Turn(0), 0)],
    );
    assert!(recorded_assignment(nonunit.as_slice()).is_err());
    let wrong_kind = document(
        "ajazz",
        serde_json::json!({"select_slot":1}),
        &[event("button_0", PhysicalEventKind::Turn(1), 0)],
    );
    assert!(matches!(
        recorded_assignment(wrong_kind.as_slice()),
        Err(AssignmentError::WrongControlKind)
    ));
    let micro_stop = document(
        "micro",
        serde_json::json!({"command":"stop"}),
        &[
            event("ACT08", PhysicalEventKind::Press, 0),
            event("ACT08", PhysicalEventKind::Release, 1),
        ],
    );
    assert!(matches!(
        recorded_assignment(micro_stop.as_slice()),
        Err(AssignmentError::Role(_))
    ));
}

#[test]
fn i03_time_generation_and_strict_nested_schema_are_enforced() {
    let press = event("button_0", PhysicalEventKind::Press, 0);
    for (release, expected) in [
        (event("button_0", PhysicalEventKind::Release, 3000), true),
        (event("button_0", PhysicalEventKind::Release, 3001), false),
    ] {
        let bytes = document(
            "ajazz",
            serde_json::json!({"select_slot":1}),
            &[press.clone(), release],
        );
        assert_eq!(recorded_assignment(bytes.as_slice()).is_ok(), expected);
    }
    let mut changed = event("button_0", PhysicalEventKind::Release, 1);
    changed.connection_generation += 1;
    let bytes = document(
        "ajazz",
        serde_json::json!({"select_slot":1}),
        &[press.clone(), changed],
    );
    assert!(matches!(
        recorded_assignment(bytes.as_slice()),
        Err(AssignmentError::ChangedGeneration)
    ));
    let mut changed = event("button_0", PhysicalEventKind::Release, 1);
    changed.at.stamp.frequency = 0;
    let bytes = document(
        "ajazz",
        serde_json::json!({"select_slot":1}),
        &[press, changed],
    );
    assert!(matches!(
        recorded_assignment(bytes.as_slice()),
        Err(AssignmentError::ClockMismatch)
    ));
    let mut value: serde_json::Value = serde_json::from_slice(&valid_press()).unwrap();
    value["events"][0]["at"]["unknown"] = serde_json::json!(true);
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(matches!(
        recorded_assignment(bytes.as_slice()),
        Err(AssignmentError::InvalidJson(_))
    ));
    let valid = String::from_utf8(valid_press()).unwrap();
    for invalid in [
        valid.replacen(
            "\"adjacency_confirmed\":false",
            "\"adjacency_confirmed\":false,\"adjacency_confirmed\":true",
            1,
        ),
        valid.replacen(
            "\"control\":\"button_0\"",
            "\"control\":\"button_0\",\"control\":\"button_1\"",
            1,
        ),
        format!("{valid} garbage"),
    ] {
        assert!(matches!(
            recorded_assignment(invalid.as_bytes()),
            Err(AssignmentError::InvalidJson(_))
        ));
    }
    let mut value: serde_json::Value = serde_json::from_slice(&valid_press()).unwrap();
    value["intent"] = serde_json::json!({"select_slot":1,"command":"stop"});
    assert!(recorded_assignment(serde_json::to_vec(&value).unwrap().as_slice()).is_err());
}

struct InfiniteReader(Rc<Cell<usize>>);

impl Read for InfiniteReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        buffer.fill(b' ');
        self.0.set(self.0.get() + buffer.len());
        Ok(buffer.len())
    }
}

#[test]
fn i03_size_limit_counts_whitespace_and_never_reads_beyond_one_extra_byte() {
    let mut bytes = valid_press();
    bytes.resize(MAX_RECORDED_BYTES, b' ');
    assert!(recorded_assignment(bytes.as_slice()).is_ok());
    bytes.push(b' ');
    assert!(matches!(
        recorded_assignment(bytes.as_slice()),
        Err(AssignmentError::TooLarge)
    ));
    let count = Rc::new(Cell::new(0));
    assert!(matches!(
        recorded_assignment(InfiniteReader(count.clone())),
        Err(AssignmentError::TooLarge)
    ));
    assert_eq!(count.get(), MAX_RECORDED_BYTES + 1);
}

#[derive(Default)]
struct FakeRoot {
    files: BTreeMap<PathBuf, Vec<u8>>,
    calls: usize,
    fail_after_partial: bool,
}

impl PrivateExportRoot for FakeRoot {
    fn create_exclusive_and_sync(
        &mut self,
        root: &Path,
        basename: &str,
        bytes: &[u8],
    ) -> io::Result<()> {
        self.calls += 1;
        assert_eq!(basename, EXPORT_FILENAME);
        let name = root.join(basename);
        if self.files.contains_key(&name) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        if self.fail_after_partial {
            self.files.insert(name, bytes[..8].to_vec());
            return Err(io::Error::other("simulated partial write"));
        }
        self.files.insert(name, bytes.to_vec());
        Ok(())
    }
}

fn absolute_root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\private-fixture")
    } else {
        PathBuf::from("/private-fixture")
    }
}

#[test]
fn i03_export_pairs_confirmation_and_delegates_only_sanitized_exclusive_bytes() {
    let candidate = recorded_assignment(valid_press().as_slice()).unwrap();
    let root = absolute_root();
    let mut backend = FakeRoot::default();
    assert_eq!(
        export_candidate(&candidate, None, None, None).unwrap(),
        ExportOutcome::NotRequested
    );
    assert!(matches!(
        export_candidate(&candidate, Some(&root), None, Some(&mut backend)),
        Err(ExportError::RootAndConfirmationRequired)
    ));
    assert!(matches!(
        export_candidate(&candidate, None, Some("ASSIGN"), Some(&mut backend)),
        Err(ExportError::RootAndConfirmationRequired)
    ));
    assert!(matches!(
        export_candidate(&candidate, Some(&root), Some("ASSIGN "), Some(&mut backend)),
        Err(ExportError::InvalidConfirmation)
    ));
    assert!(matches!(
        export_candidate(
            &candidate,
            Some(Path::new("relative")),
            Some("ASSIGN"),
            Some(&mut backend)
        ),
        Err(ExportError::RootMustBeAbsolute)
    ));
    assert!(matches!(
        export_candidate(
            &candidate,
            Some(&root.join("..").join("escape")),
            Some("ASSIGN"),
            Some(&mut backend)
        ),
        Err(ExportError::RootMustBeAbsolute)
    ));
    assert!(matches!(
        export_candidate(&candidate, Some(&root), Some("ASSIGN"), None),
        Err(ExportError::NativePrivateRootUnavailable)
    ));
    assert_eq!(backend.calls, 0);
    assert_eq!(
        export_candidate(&candidate, Some(&root), Some("ASSIGN"), Some(&mut backend)).unwrap(),
        ExportOutcome::Delegated
    );
    let written = backend
        .files
        .get(&root.join(EXPORT_FILENAME))
        .unwrap()
        .clone();
    assert_eq!(written.last(), Some(&b'\n'));
    let text = std::str::from_utf8(&written).unwrap();
    for forbidden in [
        "events",
        "ticks",
        "stamp",
        "epoch",
        "observation_ref",
        "session_id",
        "pid",
        "authority",
    ] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
    assert!(matches!(
        export_candidate(&candidate, Some(&root), Some("ASSIGN"), Some(&mut backend)),
        Err(ExportError::PrivateRoot(error)) if error.kind() == io::ErrorKind::AlreadyExists
    ));
    assert_eq!(backend.files[&root.join(EXPORT_FILENAME)], written);
    let mut partial = FakeRoot {
        fail_after_partial: true,
        ..FakeRoot::default()
    };
    assert!(matches!(
        export_candidate(&candidate, Some(&root), Some("ASSIGN"), Some(&mut partial)),
        Err(ExportError::PrivateRoot(_))
    ));
    assert_eq!(partial.files[&root.join(EXPORT_FILENAME)].len(), 8);
}

#[test]
fn physical_event_json_uses_externally_tagged_unit_and_valued_kinds() {
    let event = event("encoder_turn", PhysicalEventKind::Turn(-1), 0);
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["kind"], serde_json::json!({"turn": -1}));
    assert_eq!(value["at"], serde_json::to_value(sample(0)).unwrap());
}
