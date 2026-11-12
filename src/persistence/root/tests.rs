use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

pub(super) struct Fixture {
    pub(super) path: PathBuf,
    pub(super) root: PathBuf,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let configured = std::env::var_os("COPILOT_PRIVATE_ROOT_FIXTURES").expect(
            "set COPILOT_PRIVATE_ROOT_FIXTURES to an explicitly approved absolute directory",
        );
        let base = PathBuf::from(configured);
        assert!(base.is_absolute(), "fixture directory must be absolute");
        #[cfg(unix)]
        assert_eq!(
            fs::canonicalize(&base).expect("fixture directory must exist"),
            base,
            "fixture directory must have no symlinked ancestors"
        );
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        let path = base.join(format!(
            "pr-{:x}-{nonce:x}-{:x}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        make_private_directory(&path);
        let root = path.join("root");
        make_private_directory(&root);
        Self { path, root }
    }

    #[cfg(unix)]
    fn open_root(&self) -> PrivateRoot {
        let root = PrivateRoot::open(&self.root)
            .unwrap_or_else(|error| panic!("explicit native fixture was rejected: {error}"));
        root.check_mutation_support()
            .unwrap_or_else(|error| panic!("native mutation support is unavailable: {error}"));
        root
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Only this test's newly and exclusively created child is removed.
        fs::remove_dir_all(&self.path).expect("remove only the test's own fixture");
    }
}

fn make_private_directory(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .expect("create unique fixture directory");
    }
    #[cfg(windows)]
    native::create_test_private_directory(path).expect("create unique private fixture directory");
    #[cfg(not(any(unix, windows)))]
    fs::create_dir(path).expect("create unique fixture directory");
}

fn name(value: &str) -> Name {
    Name::new(value).unwrap()
}

#[test]
fn portable_names_are_components_not_aliases() {
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        r"a\b",
        "a:b",
        "a\0b",
        "a?",
        "x.",
        "x ",
        "CON",
        "con.txt",
        "LPT2",
        "com¹.log",
    ] {
        assert_eq!(Name::new(bad).unwrap_err().kind(), ErrorKind::InvalidName);
    }
    for good in [
        "core-state.pending.json",
        "runtime-owner.lock",
        "device-binding.json",
        "déjà-vu.json",
    ] {
        assert_eq!(Name::new(good).unwrap().as_str(), good);
    }
}

#[cfg(unix)]
#[test]
fn root_path_rejects_relative_and_traversal_spellings() {
    for path in ["relative", "/a/../b", "/a/./b", "/a//b", "/a:b/c"] {
        let error = PrivateRoot::open(path).err().unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidPath, "{path}");
    }
}

#[test]
fn synthetic_partial_write_preserves_indeterminate_evidence() {
    use std::io::{self, Write};

    struct PartialWriter {
        file: fs::File,
        remaining: usize,
    }

    impl Write for PartialWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::other("injected partial write"));
            }
            let count = self.file.write(&bytes[..bytes.len().min(self.remaining)])?;
            self.remaining -= count;
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }
    }

    let fixture = Fixture::new();
    let path = fixture.root.join("partial");
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).unwrap();
    let mut writer = PartialWriter { file, remaining: 3 };
    let error = write_contents(&mut writer, b"prepared")
        .map_err(Error::indeterminate)
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Indeterminate);
    assert_eq!(error.cause_kind(), Some(ErrorKind::Io));
    drop(writer);
    assert_eq!(fs::read(path).unwrap(), b"pre");
}

#[cfg(unix)]
#[test]
#[ignore = "requires an explicitly configured, policy-approved native filesystem"]
fn private_files_exclusive_create_and_unsafe_types() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::os::unix::net::UnixListener;

    let fixture = Fixture::new();
    let root = fixture.open_root();
    let safe = name("safe.json");
    root.create_exclusive(&safe, b"complete").unwrap();
    let metadata = fs::metadata(fixture.root.join("safe.json")).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o077, 0);
    // SAFETY: geteuid has no preconditions.
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(metadata.nlink(), 1);
    assert_eq!(root.read(&safe, 32).unwrap().unwrap(), b"complete");
    assert_eq!(
        root.create_exclusive(&safe, b"overwrite")
            .unwrap_err()
            .kind(),
        ErrorKind::AlreadyExists
    );
    assert_eq!(root.read(&safe, 32).unwrap().unwrap(), b"complete");
    assert_eq!(root.read(&safe, 3).unwrap_err().kind(), ErrorKind::TooLarge);

    symlink("safe.json", fixture.root.join("symbolic")).unwrap();
    fs::hard_link(fixture.root.join("safe.json"), fixture.root.join("hard")).unwrap();
    fs::write(fixture.root.join("permissive"), b"not-private").unwrap();
    fs::set_permissions(
        fixture.root.join("permissive"),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    fs::create_dir(fixture.root.join("directory")).unwrap();
    let fifo = CString::new(fixture.root.join("fifo").as_os_str().as_bytes()).unwrap();
    // SAFETY: the C string is a path inside this uniquely owned fixture.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let _socket = UnixListener::bind(fixture.root.join("socket")).unwrap();
    for bad in [
        "symbolic",
        "safe.json",
        "hard",
        "permissive",
        "directory",
        "fifo",
        "socket",
    ] {
        let error = match root.open_read(&name(bad)) {
            Err(error) => error,
            Ok(_) => panic!("{bad} was incorrectly accepted"),
        };
        assert_eq!(error.kind(), ErrorKind::UnsafeFile, "{bad}");
    }
}

#[cfg(unix)]
#[test]
#[ignore = "requires an explicitly configured, policy-approved native filesystem"]
fn read_only_tail_allows_public_file_but_not_unsafe_links_or_replacement() {
    use std::io::Write;
    use std::os::unix::fs::{PermissionsExt, symlink};

    let fixture = Fixture::new();
    let _root = fixture.open_root();
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o755)).unwrap();
    let path = fixture.root.join("events.jsonl");
    fs::write(&path, b"one\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        PrivateRoot::open(&fixture.root).err().unwrap().kind(),
        ErrorKind::UnsafeRoot
    );
    let mut tail = TailFile::open(&path).unwrap();
    let original = tail.metadata().unwrap();
    assert_eq!(tail.read_at(0, 3).unwrap(), b"one");
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"two\n")
        .unwrap();
    assert_eq!(tail.read_at(4, 20).unwrap(), b"two\n");
    assert_eq!(tail.metadata().unwrap().identity, original.identity);
    fs::rename(&path, fixture.root.join("old-events")).unwrap();
    fs::write(&path, b"new\n").unwrap();
    assert!(tail.metadata().is_err());
    fs::remove_file(&path).unwrap();
    symlink("old-events", &path).unwrap();
    assert_eq!(
        TailFile::open(&path).err().unwrap().kind(),
        ErrorKind::UnsafeFile
    );
    fs::remove_file(&path).unwrap();
    fs::hard_link(fixture.root.join("old-events"), &path).unwrap();
    assert_eq!(
        TailFile::open(&path).err().unwrap().kind(),
        ErrorKind::UnsafeFile
    );
}

#[test]
#[ignore = "requires an explicitly configured, policy-approved native filesystem"]
fn tail_same_size_mixed_read_is_rejected() {
    let fixture = Fixture::new();
    let path = fixture.root.join("events.jsonl");
    fs::write(&path, b"old\nold\n").unwrap();
    let mut tail = TailFile::open(&path).unwrap();
    let before = tail.metadata().unwrap();
    let error = tail
        .read_at_using(0, 8, |file, limit| {
            let mut bytes = vec![0; limit];
            file.read_exact(&mut bytes[..4])?;
            fs::write(&path, b"new\nnew\n")?;
            fs::File::options()
                .write(true)
                .open(&path)?
                .set_modified(UNIX_EPOCH + Duration::from_secs(123))?;
            file.read_exact(&mut bytes[4..])?;
            assert_eq!(bytes, b"old\nnew\n");
            Ok(bytes)
        })
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Changed);
    assert_eq!(error.operation(), "tail_modified");
    let after = tail.metadata().unwrap();
    assert_eq!(before.identity, after.identity);
    assert_eq!(before.len, after.len);
    assert_ne!(before.change, after.change);
}

#[test]
#[ignore = "requires an explicitly configured, policy-approved native filesystem"]
fn tail_same_length_rewrite_at_eof_changes_metadata_and_is_rejected() {
    let fixture = Fixture::new();
    let path = fixture.root.join("events.jsonl");
    fs::write(&path, b"old\n").unwrap();
    let mut tail = TailFile::open(&path).unwrap();
    assert_eq!(tail.read_at(0, 4).unwrap(), b"old\n");
    let before = tail.metadata().unwrap();
    fs::write(&path, b"new\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(456))
        .unwrap();
    let after = tail.metadata().unwrap();
    assert_eq!(before.identity, after.identity);
    assert_eq!(before.len, after.len);
    assert_ne!(before.change, after.change);
    assert_eq!(tail.read_at(4, 4).unwrap_err().kind(), ErrorKind::Changed);
    let mut restarted = TailFile::open(&path).unwrap();
    assert_eq!(restarted.read_at(0, 4).unwrap(), b"new\n");
    assert!(restarted.read_at(4, 4).unwrap().is_empty());
}

#[test]
#[ignore = "requires an explicitly configured, policy-approved native filesystem"]
fn tail_change_identity_detects_rewrite_with_restored_mtime() {
    let fixture = Fixture::new();
    let path = fixture.root.join("events.jsonl");
    fs::write(&path, b"old\n").unwrap();
    let original_time = UNIX_EPOCH + Duration::from_secs(789);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(original_time)
        .unwrap();
    let mut tail = TailFile::open(&path).unwrap();
    assert_eq!(tail.read_at(0, 4).unwrap(), b"old\n");
    let before = tail.metadata().unwrap();
    // Let even coarse native change timestamps advance; restore mtime so only
    // the handle-derived change timestamp can reveal this same-length rewrite.
    std::thread::sleep(Duration::from_millis(1100));
    fs::write(&path, b"new\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(original_time)
        .unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        original_time
    );
    let after = tail.metadata().unwrap();
    assert_eq!(before.identity, after.identity);
    assert_eq!(before.len, after.len);
    assert_ne!(before.change, after.change);
    assert_eq!(tail.read_at(4, 4).unwrap_err().kind(), ErrorKind::Changed);
}

#[cfg(windows)]
#[test]
#[ignore = "requires an explicitly configured fixed-disk Windows fixture"]
fn windows_mutations_fail_before_namespace_changes() {
    let fixture = Fixture::new();
    let root = PrivateRoot::open(&fixture.root).unwrap();
    for result in [
        root.check_mutation_support(),
        root.create_exclusive(&name("new"), b"no"),
        root.try_lock(&name("new-lock")).map(drop),
    ] {
        let error = result.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UnsupportedPlatform);
        assert_eq!(
            error.operation(),
            "windows_directory_durability_unavailable"
        );
    }
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);

    // An existing lock can be used, but must not authorize durable publication
    // or recovery cleanup. Only the test fixture bypasses the mutation gate.
    drop(root.inner.create_file(&name("existing-lock")).unwrap());
    let lock = root.try_lock(&name("existing-lock")).unwrap();
    for result in [
        lock.publish_prepared(
            &name("prepared"),
            &name("state"),
            PublishMode::Create,
            b"",
            8,
        ),
        lock.discard_prepared(&name("prepared"), b"", 8),
    ] {
        assert_eq!(result.unwrap_err().kind(), ErrorKind::UnsupportedPlatform);
    }
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
#[ignore = "requires an explicitly configured, policy-approved native filesystem"]
fn unsafe_roots_ancestor_and_root_substitution_are_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let fixture = Fixture::new();
    let root = fixture.open_root();
    let alias = fixture.path.join("alias");
    symlink("root", &alias).unwrap();
    assert_eq!(
        PrivateRoot::open(&alias).err().unwrap().kind(),
        ErrorKind::UnsafeRoot
    );
    make_private_directory(&fixture.path.join("actual-ancestor"));
    make_private_directory(&fixture.path.join("actual-ancestor/nested-root"));
    symlink("actual-ancestor", fixture.path.join("ancestor-alias")).unwrap();
    assert_eq!(
        PrivateRoot::open(fixture.path.join("ancestor-alias/nested-root"))
            .err()
            .unwrap()
            .kind(),
        ErrorKind::UnsafeRoot
    );

    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(root.validate().unwrap_err().kind(), ErrorKind::UnsafeRoot);
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o700)).unwrap();

    fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o770)).unwrap();
    assert_eq!(root.validate().unwrap_err().kind(), ErrorKind::UnsafeRoot);
    fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o700)).unwrap();
    root.validate().unwrap();

    fs::rename(&fixture.root, fixture.path.join("moved-root")).unwrap();
    make_private_directory(&fixture.root);
    assert_eq!(root.validate().unwrap_err().kind(), ErrorKind::Changed);
    assert!(root.create_exclusive(&name("escaped"), b"no").is_err());
    assert!(!fixture.root.join("escaped").exists());
    assert!(!fixture.path.join("moved-root/escaped").exists());
}

#[test]
#[ignore = "requires an explicitly configured, policy-approved native filesystem"]
#[cfg(unix)]
fn publication_preserves_old_readers_and_recovery_evidence() {
    let fixture = Fixture::new();
    let root = fixture.open_root();
    let lock = root.try_lock(&name("state.lock")).unwrap();
    assert!(lock.is_empty_except_lock().unwrap());
    lock.create_exclusive(&name("state.json"), b"old").unwrap();
    let mut old_reader = root.open_read(&name("state.json")).unwrap().unwrap();
    lock.create_exclusive(&name("pending.json"), b"new")
        .unwrap();
    assert_eq!(
        lock.publish_prepared(
            &name("pending.json"),
            &name("state.json"),
            PublishMode::Replace,
            b"unrecognized",
            64,
        )
        .unwrap_err()
        .kind(),
        ErrorKind::EvidenceMismatch
    );
    assert!(
        root.read(&name("pending.json.replacement"), 64)
            .unwrap()
            .is_none()
    );
    lock.publish_prepared(
        &name("pending.json"),
        &name("state.json"),
        PublishMode::Replace,
        b"new",
        64,
    )
    .unwrap();
    assert_eq!(old_reader.read_bounded(64).unwrap(), b"old");
    assert_eq!(
        old_reader.verify_current().unwrap_err().kind(),
        ErrorKind::UnsafeFile
    );
    assert_eq!(root.read(&name("state.json"), 64).unwrap().unwrap(), b"new");
    assert!(root.read(&name("pending.json"), 64).unwrap().is_none());
    assert!(
        root.read(&name("pending.json.replacement"), 64)
            .unwrap()
            .is_none()
    );

    lock.create_exclusive(&name("pending.json"), b"next")
        .unwrap();
    lock.create_exclusive(&name("pending.json.replacement"), b"other")
        .unwrap();
    assert_eq!(
        lock.publish_prepared(
            &name("pending.json"),
            &name("state.json"),
            PublishMode::Replace,
            b"next",
            64
        )
        .unwrap_err()
        .kind(),
        ErrorKind::EvidenceMismatch
    );
    assert_eq!(
        lock.discard_prepared(&name("pending.json"), b"next", 64)
            .unwrap_err()
            .kind(),
        ErrorKind::EvidenceMismatch
    );
    assert_eq!(
        root.read(&name("pending.json"), 64).unwrap().unwrap(),
        b"next"
    );
    assert_eq!(
        root.read(&name("pending.json.replacement"), 64)
            .unwrap()
            .unwrap(),
        b"other"
    );
}

#[test]
#[ignore = "requires an explicitly configured, policy-approved native filesystem"]
#[cfg(unix)]
fn create_publication_is_exclusive_and_matching_discard_is_explicit() {
    let fixture = Fixture::new();
    let root = fixture.open_root();
    let lock = root.try_lock(&name("state.lock")).unwrap();
    lock.create_exclusive(&name("pending"), b"first").unwrap();
    lock.publish_prepared(
        &name("pending"),
        &name("state"),
        PublishMode::Create,
        b"first",
        64,
    )
    .unwrap();
    lock.sync_file(&name("state")).unwrap();
    lock.create_exclusive(&name("pending"), b"second").unwrap();
    assert_eq!(
        lock.publish_prepared(
            &name("pending"),
            &name("state"),
            PublishMode::Create,
            b"second",
            64,
        )
        .unwrap_err()
        .kind(),
        ErrorKind::AlreadyExists
    );
    assert_eq!(root.read(&name("state"), 64).unwrap().unwrap(), b"first");
    lock.create_exclusive(&name("pending.replacement"), b"second")
        .unwrap();
    assert_eq!(
        lock.discard_prepared(&name("pending"), b"unrecognized", 64)
            .unwrap_err()
            .kind(),
        ErrorKind::EvidenceMismatch
    );
    assert_eq!(root.read(&name("pending"), 64).unwrap().unwrap(), b"second");
    lock.discard_prepared(&name("pending"), b"second", 64)
        .unwrap();
    assert!(root.read(&name("pending"), 64).unwrap().is_none());
    assert!(
        root.read(&name("pending.replacement"), 64)
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
#[ignore = "requires an explicitly configured, policy-approved native filesystem"]
fn replaced_or_nonempty_lock_loses_association() {
    let fixture = Fixture::new();
    let root = fixture.open_root();
    root.create_exclusive(&name("nonempty.lock"), b"x").unwrap();
    assert_eq!(
        root.try_lock(&name("nonempty.lock")).err().unwrap().kind(),
        ErrorKind::Changed
    );
    let lock = root.try_lock(&name("state.lock")).unwrap();
    fs::rename(
        fixture.root.join("state.lock"),
        fixture.root.join("old-lock"),
    )
    .unwrap();
    fs::write(fixture.root.join("state.lock"), b"").unwrap();
    assert!(lock.validate().is_err());
    assert!(lock.create_exclusive(&name("must-not-exist"), b"").is_err());
    assert!(!fixture.root.join("must-not-exist").exists());
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires an explicitly configured, policy-approved native filesystem"]
fn extended_acl_is_rejected() {
    use std::process::Command;

    let fixture = Fixture::new();
    let root = fixture.open_root();
    let status = Command::new("/bin/chmod")
        .args(["+a", "everyone allow write"])
        .arg(&fixture.root)
        .status()
        .expect("invoke the system ACL utility");
    assert!(
        status.success(),
        "fixture filesystem must support extended ACLs"
    );
    assert_eq!(root.validate().unwrap_err().kind(), ErrorKind::UnsafeAcl);
    let status = Command::new("/bin/chmod")
        .arg("-N")
        .arg(&fixture.root)
        .status()
        .expect("remove only the test root ACL");
    assert!(status.success());

    let status = Command::new("/bin/chmod")
        .args(["+a", "everyone allow write"])
        .arg(&fixture.path)
        .status()
        .expect("invoke the system ACL utility");
    assert!(
        status.success(),
        "fixture filesystem must support extended ACLs"
    );
    assert_eq!(root.validate().unwrap_err().kind(), ErrorKind::UnsafeAcl);
    let status = Command::new("/bin/chmod")
        .arg("-N")
        .arg(&fixture.path)
        .status()
        .expect("remove only the test fixture ACL");
    assert!(status.success());

    root.create_exclusive(&name("acl-file"), b"data").unwrap();
    let status = Command::new("/bin/chmod")
        .args(["+a", "everyone allow read"])
        .arg(fixture.root.join("acl-file"))
        .status()
        .expect("invoke the system ACL utility");
    assert!(
        status.success(),
        "fixture filesystem must support extended ACLs"
    );
    let error = match root.open_read(&name("acl-file")) {
        Err(error) => error,
        Ok(_) => panic!("extended ACL was accepted"),
    };
    assert_eq!(error.kind(), ErrorKind::UnsafeAcl);
}

#[test]
fn lock_child() {
    use std::io::{BufRead, Write};

    let Some(path) = std::env::var_os("COPILOT_PRIVATE_ROOT_LOCK_CHILD") else {
        return;
    };
    let root = PrivateRoot::open(path).unwrap();
    let busy = root.try_lock(&name("state.lock")).err().unwrap();
    assert_eq!(busy.kind(), ErrorKind::Busy);
    let busy = root.lock_bounded(&name("state.lock")).err().unwrap();
    assert_eq!(busy.kind(), ErrorKind::Busy);
    println!("LOCK_CHILD_BUSY");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap();
    let _guard = root.lock_bounded(&name("state.lock")).unwrap();
    println!("LOCK_CHILD_ACQUIRED");
    std::io::stdout().flush().unwrap();
    line.clear();
    std::io::stdin().lock().read_line(&mut line).unwrap();
}

#[cfg(unix)]
pub(super) fn exercise_process_lock(
    filter: &str,
    variable: &str,
    path: &Path,
    release_parent: impl FnOnce(),
    check_contended: impl FnOnce(),
) {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc::{self, Receiver};
    use std::time::{Duration, Instant};

    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    fn wait_for(receiver: &Receiver<String>, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = receiver
                .recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("child did not report {expected} before deadline"));
            if line.contains(expected) {
                return;
            }
        }
    }

    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(["--nocapture", filter])
            .env(variable, path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    wait_for(&receiver, "LOCK_CHILD_BUSY");
    release_parent();
    writeln!(child.0.stdin.as_mut().unwrap(), "released").unwrap();
    child.0.stdin.as_mut().unwrap().flush().unwrap();
    wait_for(&receiver, "LOCK_CHILD_ACQUIRED");
    check_contended();
    writeln!(child.0.stdin.as_mut().unwrap(), "finish").unwrap();
    child.0.stdin.as_mut().unwrap().flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "lock child did not exit before deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());
    reader.join().unwrap();
}

#[test]
#[ignore = "requires an explicitly configured, policy-approved native filesystem"]
#[cfg(unix)]
fn lock_contention_uses_bounded_cross_process_handshakes() {
    let fixture = Fixture::new();
    let root = fixture.open_root();
    let first = root.try_lock(&name("state.lock")).unwrap();
    exercise_process_lock(
        "tests::lock_child",
        "COPILOT_PRIVATE_ROOT_LOCK_CHILD",
        &fixture.root,
        || drop(first),
        || {
            assert_eq!(
                root.try_lock(&name("state.lock")).err().unwrap().kind(),
                ErrorKind::Busy
            );
        },
    );
    root.try_lock(&name("state.lock"))
        .unwrap()
        .validate()
        .unwrap();
}
