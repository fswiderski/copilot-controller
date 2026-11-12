use std::{
    cell::RefCell,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    rc::Rc,
};

use crate::domain::Status;

use super::super::{
    event::{Event, EventKind, MAX_EVENT_BYTES},
    reducer::Reducer,
    tail::{
        FileIdentity, MAX_FORKS_PER_POLL, MAX_POLL_BYTES, SourceSnapshot, TailBoundary, TailCursor,
        TailError, TailFile, poll, validate_open_and_named,
    },
};
use super::session;

#[derive(Clone, Copy)]
enum Mutation {
    Replace,
    Shrink,
    Rewrite,
    Grow,
    GrowAtEof,
    ReadError,
    WrongReadCount,
    PrematureEof,
    SeekError,
    WrongSeek,
    RecheckError,
}

struct FakeState {
    bytes: Vec<u8>,
    identity: FileIdentity,
    modified: i128,
    changed: i128,
    mutation: Option<Mutation>,
    reads: usize,
}

impl FakeState {
    fn snapshot(&self) -> SourceSnapshot {
        SourceSnapshot {
            identity: self.identity,
            length: self.bytes.len() as u64,
            modified: self.modified,
            changed: self.changed,
        }
    }

    fn replace_contents(&mut self, bytes: Vec<u8>) {
        self.bytes = bytes;
        self.modified += 1;
        self.changed += 1;
    }
}

#[derive(Clone)]
pub(super) struct FakeBoundary(Rc<RefCell<FakeState>>);

impl FakeBoundary {
    pub(super) fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(Rc::new(RefCell::new(FakeState {
            bytes: bytes.into(),
            identity: FileIdentity { volume: 1, file: 1 },
            modified: 1,
            changed: 1,
            mutation: None,
            reads: 0,
        })))
    }
}

pub(super) struct FakeFile {
    state: Rc<RefCell<FakeState>>,
    offset: usize,
    checks: usize,
}

impl TailBoundary for FakeBoundary {
    type File = FakeFile;

    fn open(&self, _: &Path) -> Result<Self::File, TailError> {
        Ok(FakeFile {
            state: self.0.clone(),
            offset: 0,
            checks: 0,
        })
    }
}

impl TailFile for FakeFile {
    fn recheck(&mut self) -> Result<SourceSnapshot, TailError> {
        self.checks += 1;
        let state = self.state.borrow();
        if self.checks > 1 && matches!(state.mutation, Some(Mutation::RecheckError)) {
            return Err(TailError::SourceChanged);
        }
        Ok(state.snapshot())
    }
}

impl Seek for FakeFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if matches!(self.state.borrow().mutation, Some(Mutation::SeekError)) {
            return Err(io::Error::other("private seek path"));
        }
        let SeekFrom::Start(offset) = position else {
            return Err(io::Error::other("unsupported fake seek"));
        };
        if matches!(self.state.borrow().mutation, Some(Mutation::WrongSeek)) {
            return Ok(offset.saturating_add(1));
        }
        self.offset = usize::try_from(offset).map_err(|_| io::Error::other("seek overflow"))?;
        Ok(offset)
    }
}

impl Read for FakeFile {
    fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
        let mut state = self.state.borrow_mut();
        state.reads += 1;
        if matches!(state.mutation, Some(Mutation::ReadError)) {
            return Err(io::Error::other("private read path"));
        }
        if matches!(state.mutation, Some(Mutation::WrongReadCount)) {
            return Ok(target.len() + 1);
        }
        if matches!(state.mutation, Some(Mutation::PrematureEof)) {
            return Ok(0);
        }
        let count = target
            .len()
            .min(state.bytes.len().saturating_sub(self.offset));
        if count == 0 {
            if matches!(state.mutation, Some(Mutation::GrowAtEof)) {
                state
                    .bytes
                    .extend_from_slice(b"{\"type\":\"abort\",\"timestamp\":2}\n");
                state.modified += 1;
                state.changed += 1;
                state.mutation = None;
            }
            return Ok(0);
        }
        target[..count].copy_from_slice(&state.bytes[self.offset..self.offset + count]);
        self.offset += count;
        match state.mutation.take() {
            Some(Mutation::Replace) => {
                state.identity.file += 1;
            }
            Some(Mutation::Shrink) => {
                state.bytes.clear();
                state.modified += 1;
                state.changed += 1;
            }
            Some(Mutation::Rewrite) => {
                state.modified += 1;
                state.changed += 1;
            }
            Some(Mutation::Grow) => {
                state
                    .bytes
                    .extend_from_slice(b"{\"type\":\"abort\",\"timestamp\":2}\n");
                state.modified += 1;
                state.changed += 1;
            }
            Some(other) => state.mutation = Some(other),
            None => {}
        }
        Ok(count)
    }
}

pub(super) fn path() -> &'static Path {
    #[cfg(windows)]
    return Path::new(r"C:\synthetic\events.jsonl");
    #[cfg(not(windows))]
    Path::new("/synthetic/events.jsonl")
}

fn bytes(line: &str) -> Vec<u8> {
    format!("{line}\n").into_bytes()
}

#[test]
fn e03_split_bounded_blank_lines_and_acknowledgment_survive_empty_poll() {
    let data = concat!(
        "{\"type\":\"user.message\",\"timestamp\":1,\"data\":{\"content\":\"private body\"}}\n",
        " \t\r\n",
        "{\"type\":\"session.task_complete\",\"timestamp\":2}\n"
    )
    .as_bytes()
    .to_vec();
    let boundary = FakeBoundary::new(data.clone());
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    let first = poll(&boundary, path(), 12, &mut cursor, &mut reducer).unwrap();
    assert!(first.reset);
    assert_eq!(
        (first.records, first.bytes_read, cursor.offset()),
        (0, 12, 12)
    );
    assert!(!first.caught_up);
    assert_eq!(cursor.partial_bytes(), 12);
    assert!(!format!("{cursor:?}").contains("private"));
    let mut total_records = 0;
    while !cursor.caught_up() {
        let report = poll(&boundary, path(), 13, &mut cursor, &mut reducer).unwrap();
        assert!(report.bytes_read <= 13);
        total_records += report.records;
    }
    assert_eq!(total_records, 2);
    assert_eq!(cursor.offset(), data.len() as u64);
    assert_eq!(reducer.status(), Status::Complete);
    let timestamp = reducer.meaningful_event_ms();
    reducer.acknowledge_completion();
    let empty = poll(&boundary, path(), 100, &mut cursor, &mut reducer).unwrap();
    assert!(!empty.reset);
    assert!(empty.caught_up);
    assert_eq!((empty.records, empty.bytes_read), (0, 0));
    assert_eq!(
        (reducer.status(), reducer.meaningful_event_ms()),
        (Status::Idle, timestamp)
    );
    boundary.0.borrow_mut().changed += 1;
    let unchanged = poll(&boundary, path(), 100, &mut cursor, &mut reducer).unwrap();
    assert_eq!((unchanged.records, unchanged.bytes_read), (0, 0));
    assert!(!unchanged.reset);
    assert_eq!(reducer.status(), Status::Idle);
}

#[test]
fn e03_partial_record_debug_never_exposes_ingress_text() {
    let data =
        br#"{"type":"assistant.message","timestamp":1,"data":{"content":"private-synthetic-body"#;
    let boundary = FakeBoundary::new(data.to_vec());
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    let report = poll(&boundary, path(), data.len(), &mut cursor, &mut reducer).unwrap();
    assert!(!report.caught_up);
    assert_eq!(cursor.partial_bytes(), data.len());
    assert!(!format!("{cursor:?}").contains("private-synthetic-body"));
}

#[test]
fn e03_eof_append_rewrite_truncation_and_replacement_restart() {
    let complete = bytes(r#"{"type":"session.task_complete","timestamp":1}"#);
    let boundary = FakeBoundary::new(complete.clone());
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    poll(&boundary, path(), 1_000, &mut cursor, &mut reducer).unwrap();
    assert_eq!(reducer.status(), Status::Complete);

    let mut appended = complete;
    appended.extend(bytes(r#"{"type":"user.message","timestamp":2}"#));
    boundary.0.borrow_mut().replace_contents(appended);
    let report = poll(&boundary, path(), 1_000, &mut cursor, &mut reducer).unwrap();
    assert!(report.reset);
    assert_eq!(report.records, 2);
    assert_eq!(reducer.status(), Status::Working);

    let old_len = boundary.0.borrow().bytes.len();
    let mut rewrite = bytes(r#"{"type":"abort","timestamp":3}"#);
    rewrite.resize(old_len - 1, b' ');
    rewrite.push(b'\n');
    boundary.0.borrow_mut().replace_contents(rewrite);
    let report = poll(&boundary, path(), 1_000, &mut cursor, &mut reducer).unwrap();
    assert!(report.reset);
    assert_eq!(reducer.status(), Status::Idle);
    assert_eq!(reducer.meaningful_event_ms(), 3);

    boundary
        .0
        .borrow_mut()
        .replace_contents(bytes(r#"{"type":"session.error","timestamp":4}"#));
    let report = poll(&boundary, path(), 1_000, &mut cursor, &mut reducer).unwrap();
    assert!(report.reset);
    assert_eq!(reducer.status(), Status::Error);
    {
        let mut source = boundary.0.borrow_mut();
        source.identity.file += 1;
        source.replace_contents(bytes(r#"{"type":"user.message","timestamp":1}"#));
    }
    let report = poll(&boundary, path(), 1_000, &mut cursor, &mut reducer).unwrap();
    assert!(report.reset);
    assert_eq!(
        (reducer.status(), reducer.meaningful_event_ms()),
        (Status::Working, 1)
    );
}

#[test]
fn e03_incomplete_and_newline_inclusive_record_bounds() {
    let mut exact = br#"{"type":"abort","timestamp":1}"#.to_vec();
    exact.resize(MAX_EVENT_BYTES - 1, b' ');
    exact.push(b'\n');
    let boundary = FakeBoundary::new(exact.clone());
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    let report = poll(
        &boundary,
        path(),
        MAX_EVENT_BYTES,
        &mut cursor,
        &mut reducer,
    )
    .unwrap();
    assert_eq!(report.records, 1);
    assert!(report.caught_up);

    let boundary = FakeBoundary::new(exact[..MAX_EVENT_BYTES - 1].to_vec());
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    let partial = poll(
        &boundary,
        path(),
        MAX_EVENT_BYTES,
        &mut cursor,
        &mut reducer,
    )
    .unwrap();
    assert!(!partial.caught_up);
    assert_eq!(cursor.partial_bytes(), MAX_EVENT_BYTES - 1);
    boundary.0.borrow_mut().replace_contents(exact);
    assert!(
        poll(
            &boundary,
            path(),
            MAX_EVENT_BYTES,
            &mut cursor,
            &mut reducer
        )
        .unwrap()
        .caught_up
    );

    let boundary = FakeBoundary::new(vec![b' '; MAX_EVENT_BYTES]);
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    assert_eq!(
        poll(
            &boundary,
            path(),
            MAX_EVENT_BYTES,
            &mut cursor,
            &mut reducer
        ),
        Err(TailError::OversizedRecord)
    );
    assert_eq!(cursor.offset(), 0);
    assert_eq!(reducer.status(), Status::Unknown);
    let mut too_long = vec![b' '; MAX_EVENT_BYTES];
    too_long.push(b'\n');
    boundary.0.borrow_mut().replace_contents(too_long);
    assert_eq!(
        poll(
            &boundary,
            path(),
            MAX_EVENT_BYTES + 1,
            &mut cursor,
            &mut reducer
        ),
        Err(TailError::OversizedRecord)
    );
}

#[test]
fn e03_corruption_never_commits_or_skips_history() {
    let mut data = bytes(r#"{"type":"user.message","timestamp":1}"#);
    data.extend_from_slice(b"{private invalid record}\n");
    data.extend(bytes(r#"{"type":"session.task_complete","timestamp":2}"#));
    let boundary = FakeBoundary::new(data);
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    reducer.apply_event(Event {
        timestamp_ms: 20,
        kind: EventKind::TaskComplete,
    });
    for _ in 0..2 {
        let result = poll(&boundary, path(), 1_000, &mut cursor, &mut reducer);
        assert_eq!(result, Err(TailError::InvalidRecord));
        assert!(!format!("{result:?}").contains("private"));
        assert_eq!(reducer.status(), Status::Unknown);
        assert_eq!(
            (cursor.offset(), cursor.partial_bytes(), cursor.caught_up()),
            (0, 0, false)
        );
    }
    let mut repaired = bytes(r#"{"type":"user.message","timestamp":1}"#);
    repaired.extend(bytes(r#"{"type":"session.task_complete","timestamp":2}"#));
    boundary.0.borrow_mut().replace_contents(repaired);
    let report = poll(&boundary, path(), 1_000, &mut cursor, &mut reducer).unwrap();
    assert!(report.reset);
    assert_eq!(report.records, 2);
    assert_eq!(reducer.status(), Status::Complete);
}

#[test]
fn e03_fault_after_a_committed_prefix_restarts_from_the_beginning() {
    let first = bytes(r#"{"type":"user.message","timestamp":1}"#);
    let mut data = first.clone();
    data.extend_from_slice(b"{invalid}\n");
    data.extend(bytes(r#"{"type":"session.task_complete","timestamp":2}"#));
    let boundary = FakeBoundary::new(data);
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    let prefix = poll(&boundary, path(), first.len(), &mut cursor, &mut reducer).unwrap();
    assert_eq!(prefix.records, 1);
    assert_eq!(cursor.offset(), first.len() as u64);
    assert_eq!(reducer.status(), Status::Working);
    assert_eq!(
        poll(&boundary, path(), 100, &mut cursor, &mut reducer),
        Err(TailError::InvalidRecord)
    );
    assert_eq!(cursor.offset(), 0);
    assert_eq!(reducer.status(), Status::Unknown);
    let mut repaired = first;
    repaired.extend(bytes(r#"{"type":"session.task_complete","timestamp":2}"#));
    boundary.0.borrow_mut().replace_contents(repaired);
    let complete = poll(&boundary, path(), 1_000, &mut cursor, &mut reducer).unwrap();
    assert_eq!(complete.records, 2);
    assert_eq!(reducer.status(), Status::Complete);
    assert_eq!(
        poll(&boundary, path(), 0, &mut cursor, &mut reducer),
        Err(TailError::InvalidBudget)
    );
    assert_eq!(cursor.offset(), 0);
    assert_eq!(reducer.status(), Status::Unknown);
}

#[test]
fn e03_source_faults_budget_and_concurrent_growth() {
    for (mutation, expected) in [
        (Mutation::Replace, TailError::SourceChanged),
        (Mutation::Shrink, TailError::SourceChanged),
        (Mutation::Rewrite, TailError::SourceChanged),
        (Mutation::ReadError, TailError::Io),
        (Mutation::WrongReadCount, TailError::Io),
        (Mutation::PrematureEof, TailError::Io),
        (Mutation::SeekError, TailError::Io),
        (Mutation::WrongSeek, TailError::Io),
        (Mutation::RecheckError, TailError::SourceChanged),
    ] {
        let boundary = FakeBoundary::new(bytes(r#"{"type":"user.message","timestamp":1}"#));
        boundary.0.borrow_mut().mutation = Some(mutation);
        let mut cursor = TailCursor::default();
        let mut reducer = Reducer::new();
        assert_eq!(
            poll(&boundary, path(), 100, &mut cursor, &mut reducer),
            Err(expected)
        );
        assert_eq!(cursor.offset(), 0);
        assert!(!cursor.caught_up());
        assert_eq!(reducer.status(), Status::Unknown);
    }
    let boundary = FakeBoundary::new(bytes(r#"{"type":"user.message","timestamp":1}"#));
    for budget in [0, MAX_POLL_BYTES + 1] {
        let mut cursor = TailCursor::default();
        let mut reducer = Reducer::new();
        assert_eq!(
            poll(&boundary, path(), budget, &mut cursor, &mut reducer),
            Err(TailError::InvalidBudget)
        );
        assert_eq!(reducer.status(), Status::Unknown);
    }
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    assert_eq!(
        poll(
            &boundary,
            Path::new("relative"),
            1,
            &mut cursor,
            &mut reducer
        ),
        Err(TailError::InvalidPath)
    );
    assert_eq!(reducer.status(), Status::Unknown);

    let original = bytes(r#"{"type":"user.message","timestamp":1}"#);
    let boundary = FakeBoundary::new(original.clone());
    boundary.0.borrow_mut().mutation = Some(Mutation::Grow);
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    let first = poll(&boundary, path(), original.len(), &mut cursor, &mut reducer).unwrap();
    assert!(!first.caught_up);
    assert_eq!(reducer.status(), Status::Working);
    let second = poll(&boundary, path(), 100, &mut cursor, &mut reducer).unwrap();
    assert!(!second.reset);
    assert!(second.caught_up);
    assert_eq!(reducer.status(), Status::Idle);

    let boundary = FakeBoundary::new(original);
    boundary.0.borrow_mut().mutation = Some(Mutation::GrowAtEof);
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    let first = poll(&boundary, path(), 100, &mut cursor, &mut reducer).unwrap();
    assert!(!first.caught_up);
    assert_eq!(reducer.status(), Status::Working);
    let second = poll(&boundary, path(), 100, &mut cursor, &mut reducer).unwrap();
    assert!(!second.reset);
    assert!(second.caught_up);
    assert_eq!(reducer.status(), Status::Idle);
}

#[test]
fn e03_native_recheck_allows_growth_between_handle_snapshots() {
    let opened = SourceSnapshot {
        identity: FileIdentity { volume: 1, file: 2 },
        length: 10,
        modified: 3,
        changed: 4,
    };
    let grown = SourceSnapshot {
        length: 11,
        modified: 5,
        changed: 6,
        ..opened
    };
    assert_eq!(validate_open_and_named(opened, grown), Ok(grown));

    for rewritten in [
        SourceSnapshot {
            modified: 5,
            ..opened
        },
        SourceSnapshot {
            changed: 5,
            ..opened
        },
    ] {
        assert_eq!(
            validate_open_and_named(opened, rewritten),
            Err(TailError::SourceChanged)
        );
    }
    let replaced = SourceSnapshot {
        identity: FileIdentity { volume: 1, file: 3 },
        ..grown
    };
    assert_eq!(
        validate_open_and_named(opened, replaced),
        Err(TailError::SourceChanged)
    );
}

#[test]
fn e03_fork_observation_cap_never_skips_later_history() {
    let mut data = Vec::new();
    for id in 1..=MAX_FORKS_PER_POLL + 1 {
        let message = format!(
            "Forked this session into \"hidden\" ({}).",
            session(id as u128 + 100)
        );
        let record = serde_json::json!({
            "type":"session.info", "timestamp":id, "data":{"infoType":"fork", "message":message}
        });
        data.extend(serde_json::to_vec(&record).unwrap());
        data.push(b'\n');
    }
    data.extend(bytes(r#"{"type":"session.task_complete","timestamp":100}"#));
    let boundary = FakeBoundary::new(data);
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    let first = poll(&boundary, path(), MAX_POLL_BYTES, &mut cursor, &mut reducer).unwrap();
    assert_eq!(first.fork_observations.len(), MAX_FORKS_PER_POLL);
    assert_eq!(first.records, MAX_FORKS_PER_POLL);
    assert!(cursor.offset() <= first.bytes_read as u64);
    assert!(!first.caught_up);
    assert_eq!(reducer.status(), Status::Idle);
    let second = poll(&boundary, path(), MAX_POLL_BYTES, &mut cursor, &mut reducer).unwrap();
    assert_eq!(second.fork_observations.len(), 1);
    assert_eq!(second.records, 2);
    assert!(second.caught_up);
    assert!(
        first.fork_observations.last().unwrap().next_offset
            < second.fork_observations[0].next_offset
    );
    assert_eq!(reducer.status(), Status::Complete);
    assert!(!format!("{first:?}{second:?}").contains("hidden"));
}

#[test]
fn e03_ordinary_input_after_fork_threshold_is_read_in_chunks() {
    let mut data = Vec::new();
    for id in 1..=MAX_FORKS_PER_POLL + 1 {
        if id == MAX_FORKS_PER_POLL {
            data.extend(std::iter::repeat_n(b'\n', 30_000));
        }
        let message = format!(
            "Forked this session into \"hidden\" ({}).",
            session(id as u128 + 100)
        );
        let record = serde_json::json!({
            "type":"session.info", "timestamp":id, "data":{"infoType":"fork", "message":message}
        });
        data.extend(serde_json::to_vec(&record).unwrap());
        data.push(b'\n');
    }
    data.extend(bytes(r#"{"type":"session.task_complete","timestamp":100}"#));
    let boundary = FakeBoundary::new(data.clone());
    let mut cursor = TailCursor::default();
    let mut reducer = Reducer::new();
    let first = poll(&boundary, path(), MAX_POLL_BYTES, &mut cursor, &mut reducer).unwrap();
    assert_eq!(first.fork_observations.len(), MAX_FORKS_PER_POLL);
    assert!(boundary.0.borrow().reads < 20);
    assert!(cursor.offset() < first.bytes_read as u64);
    assert!(!first.caught_up);
    let second = poll(&boundary, path(), MAX_POLL_BYTES, &mut cursor, &mut reducer).unwrap();
    assert_eq!(second.fork_observations.len(), 1);
    assert_eq!(second.records, 2);
    assert_eq!(cursor.offset(), data.len() as u64);
    assert!(second.caught_up);
    assert_eq!(reducer.status(), Status::Complete);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native {
    use std::{
        fs,
        os::unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, PermissionsExt, symlink},
        },
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;
    use crate::session_engine::tail::NativeBoundary;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(root: &Path) -> Self {
            assert!(
                root.is_absolute(),
                "fixture root must be explicitly absolute"
            );
            for _ in 0..1_024 {
                let path = root.join(format!(
                    "engine-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(_) => panic!("cannot create private fixture"),
                }
            }
            panic!("cannot allocate unique fixture");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if fs::remove_dir_all(&self.0).is_err() && !std::thread::panicking() {
                panic!("cannot remove private fixture");
            }
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn storage_group_writable_ancestor_is_refused() {
        let base = std::env::var_os("SESSION_ENGINE_SAFE_FIXTURE_ROOT")
            .expect("explicit engine fixture root");
        let fixture = Fixture::new(Path::new(&base));
        let group_writable = fixture.0.join("group-writable");
        fs::DirBuilder::new()
            .mode(0o770)
            .create(&group_writable)
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&group_writable, fs::Permissions::from_mode(0o770)).unwrap();
        let file = group_writable.join("events");
        fs::write(&file, bytes(r#"{"type":"abort","timestamp":1}"#)).unwrap();
        assert!(matches!(
            NativeBoundary.open(&file),
            Err(TailError::UnsafePath)
        ));
    }

    #[test]
    #[ignore = "requires explicit SESSION_ENGINE_SAFE_FIXTURE_ROOT with safe ancestors"]
    fn safe_native_read_rejects_links_types_and_path_replacement() {
        let root = std::env::var_os("SESSION_ENGINE_SAFE_FIXTURE_ROOT")
            .expect("supply an explicit safe native fixture root");
        let fixture = Fixture::new(Path::new(&root));
        let file = fixture.0.join("events");
        fs::write(&file, bytes(r#"{"type":"user.message","timestamp":1}"#)).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        let mut cursor = TailCursor::default();
        let mut reducer = Reducer::new();
        let report = poll(&NativeBoundary, &file, 1_000, &mut cursor, &mut reducer).unwrap();
        assert!(report.caught_up);
        assert_eq!(reducer.status(), Status::Working);

        let symbolic = fixture.0.join("symbolic");
        symlink(&file, &symbolic).unwrap();
        assert!(matches!(
            NativeBoundary.open(&symbolic),
            Err(TailError::UnsafePath)
        ));
        let linked = fixture.0.join("hardlink");
        fs::hard_link(&file, &linked).unwrap();
        assert!(matches!(
            NativeBoundary.open(&file),
            Err(TailError::UnsafePath)
        ));
        assert!(matches!(
            NativeBoundary.open(&linked),
            Err(TailError::UnsafePath)
        ));
        fs::remove_file(linked).unwrap();

        let target_directory = fixture.0.join("real");
        fs::create_dir(&target_directory).unwrap();
        let real_file = target_directory.join("events");
        fs::write(&real_file, b"").unwrap();
        let directory_link = fixture.0.join("linked-dir");
        symlink(&target_directory, &directory_link).unwrap();
        assert!(matches!(
            NativeBoundary.open(&directory_link.join("events")),
            Err(TailError::UnsafePath)
        ));
        let unsafe_dir = fixture.0.join("writable");
        fs::create_dir(&unsafe_dir).unwrap();
        fs::set_permissions(&unsafe_dir, fs::Permissions::from_mode(0o777)).unwrap();
        fs::write(unsafe_dir.join("events"), b"").unwrap();
        assert!(matches!(
            NativeBoundary.open(&unsafe_dir.join("events")),
            Err(TailError::UnsafePath)
        ));
        fs::set_permissions(&unsafe_dir, fs::Permissions::from_mode(0o700)).unwrap();

        let fifo = fixture.0.join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            NativeBoundary.open(&fifo),
            Err(TailError::UnsafePath)
        ));
        assert!(matches!(
            NativeBoundary.open(&target_directory),
            Err(TailError::UnsafePath)
        ));
        assert!(matches!(
            NativeBoundary.open(&fixture.0.join("real/../events")),
            Err(TailError::InvalidPath)
        ));

        let nested = fixture.0.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("events"), b"").unwrap();
        let mut nested_open = NativeBoundary.open(&nested.join("events")).unwrap();
        fs::rename(&nested, fixture.0.join("old-nested")).unwrap();
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("events"), b"").unwrap();
        assert_eq!(nested_open.recheck(), Err(TailError::SourceChanged));

        let mut opened = NativeBoundary.open(&file).unwrap();
        let old = fixture.0.join("old");
        fs::rename(&file, &old).unwrap();
        fs::write(&file, bytes(r#"{"type":"abort","timestamp":1}"#)).unwrap();
        assert_eq!(opened.recheck(), Err(TailError::SourceChanged));
    }
}

#[cfg(windows)]
mod windows_native {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;
    use crate::session_engine::tail::NativeBoundary;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::var_os("SESSION_ENGINE_SAFE_FIXTURE_ROOT")
                .expect("supply an explicit Windows fixture root");
            let root = Path::new(&root);
            assert!(root.is_absolute());
            for _ in 0..1_024 {
                let child = root.join(format!(
                    "engine-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&child) {
                    Ok(()) => return Self(child),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(_) => panic!("cannot create private fixture"),
                }
            }
            panic!("cannot allocate unique fixture");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if fs::remove_dir_all(&self.0).is_err() && !std::thread::panicking() {
                panic!("cannot remove private fixture");
            }
        }
    }

    #[test]
    fn reserved_windows_devices_are_rejected_without_opening_them() {
        for path in [
            r"C:\synthetic\NUL",
            r"C:\synthetic\COM1.txt",
            r"C:\synthetic\LPT²",
            r"C:\synthetic\CON .txt",
            r"C:\synthetic\file:stream",
            r"\\server\pipe\name",
            r"\\server\share\events",
        ] {
            assert!(matches!(
                NativeBoundary.open(Path::new(path)),
                Err(TailError::InvalidPath)
            ));
        }
    }

    #[test]
    #[ignore = "requires explicit SESSION_ENGINE_SAFE_FIXTURE_ROOT on Windows"]
    fn native_windows_regular_file_links_and_replacement() {
        let fixture = Fixture::new();
        let file = fixture.0.join("events");
        fs::write(&file, bytes(r#"{"type":"user.message","timestamp":1}"#)).unwrap();
        let mut cursor = TailCursor::default();
        let mut reducer = Reducer::new();
        assert!(
            poll(&NativeBoundary, &file, 1_000, &mut cursor, &mut reducer)
                .unwrap()
                .caught_up
        );
        assert_eq!(reducer.status(), Status::Working);
        let linked = fixture.0.join("hardlink");
        fs::hard_link(&file, &linked).unwrap();
        assert!(matches!(
            NativeBoundary.open(&file),
            Err(TailError::UnsafePath)
        ));
        fs::remove_file(linked).unwrap();
        let mut opened = NativeBoundary.open(&file).unwrap();
        fs::rename(&file, fixture.0.join("old")).unwrap();
        fs::write(&file, bytes(r#"{"type":"abort","timestamp":1}"#)).unwrap();
        assert_eq!(opened.recheck(), Err(TailError::SourceChanged));
    }
}
