//! Cooperative runtime ownership uses one caller-selected authority root,
//! independent of any configuration root.

use std::fmt;

use uuid::Uuid;

use super::root::{ErrorKind as RootErrorKind, LockGuard, Name, PrivateRoot};

const OWNER_LOCK: &str = "runtime-owner.lock";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnershipErrorKind {
    UnsafeRoot,
    Unavailable,
    Busy,
    OwnershipChanged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnershipError {
    kind: OwnershipErrorKind,
}

impl OwnershipError {
    fn new(kind: OwnershipErrorKind) -> Self {
        Self { kind }
    }

    pub const fn kind(&self) -> OwnershipErrorKind {
        self.kind
    }
}

impl fmt::Display for OwnershipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "runtime ownership refused: {:?}", self.kind)
    }
}

impl std::error::Error for OwnershipError {}

/// A process-local epoch backed by a held native lock in a shared authority root.
/// The caller must pass the same approved authority root across configuration roots.
pub struct RuntimeOwnerLease {
    lock: LockGuard,
    epoch: Uuid,
}

impl RuntimeOwnerLease {
    pub fn acquire(authority_root: &PrivateRoot) -> Result<Self, OwnershipError> {
        authority_root.validate().map_err(|error| {
            OwnershipError::new(if error.kind() == RootErrorKind::UnsafeRoot {
                OwnershipErrorKind::UnsafeRoot
            } else {
                OwnershipErrorKind::Unavailable
            })
        })?;
        let name = Name::new(OWNER_LOCK)
            .map_err(|_| OwnershipError::new(OwnershipErrorKind::Unavailable))?;
        let lock = authority_root.try_lock_existing(&name).map_err(|error| {
            let kind = match error.kind() {
                RootErrorKind::Busy => OwnershipErrorKind::Busy,
                RootErrorKind::Changed | RootErrorKind::EvidenceMismatch => {
                    OwnershipErrorKind::OwnershipChanged
                }
                RootErrorKind::UnsafeRoot => OwnershipErrorKind::OwnershipChanged,
                _ => OwnershipErrorKind::Unavailable,
            };
            OwnershipError::new(kind)
        })?;
        lock.validate().map_err(|error| {
            OwnershipError::new(if error.kind() == RootErrorKind::Io {
                OwnershipErrorKind::Unavailable
            } else {
                OwnershipErrorKind::OwnershipChanged
            })
        })?;
        Ok(Self {
            lock,
            epoch: Uuid::new_v4(),
        })
    }

    /// Revalidates the empty anchor and held-file/name association before
    /// returning the epoch. No owner data is written to the anchor.
    pub fn epoch(&self) -> Result<Uuid, OwnershipError> {
        self.lock.validate().map_err(|error| {
            OwnershipError::new(if error.kind() == RootErrorKind::Io {
                OwnershipErrorKind::Unavailable
            } else {
                OwnershipErrorKind::OwnershipChanged
            })
        })?;
        Ok(self.epoch)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        io::{BufRead, BufReader},
        path::PathBuf,
        process::{Command, Stdio},
        sync::mpsc,
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let base = PathBuf::from(
                std::env::var_os("COPILOT_PRIVATE_ROOT_FIXTURES")
                    .expect("set COPILOT_PRIVATE_ROOT_FIXTURES to an approved directory"),
            );
            let path = base.join(format!(
                "owner-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            }
            #[cfg(windows)]
            super::super::root::create_test_private_directory(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    #[ignore = "requires explicitly approved native root and anchor initialization"]
    fn one_authority_anchor_excludes_other_config_roots_without_takeover() {
        let fixture = Fixture::new();
        let root = PrivateRoot::open(&fixture.0).unwrap();
        root.check_mutation_support().unwrap();
        let name = Name::new(OWNER_LOCK).unwrap();
        assert!(matches!(
            RuntimeOwnerLease::acquire(&root),
            Err(error) if error.kind() == OwnershipErrorKind::Unavailable
        ));
        assert!(root.open_read(&name).unwrap().is_none());
        root.create_exclusive(&name, &[]).unwrap();

        let owner = RuntimeOwnerLease::acquire(&root).unwrap();
        let epoch = owner.epoch().unwrap();
        assert!(matches!(
            RuntimeOwnerLease::acquire(&root),
            Err(error) if error.kind() == OwnershipErrorKind::Busy
        ));
        drop(owner);

        let replacement_owner = RuntimeOwnerLease::acquire(&root).unwrap();
        assert_ne!(replacement_owner.epoch().unwrap(), epoch);
        assert_eq!(fs::metadata(fixture.0.join(OWNER_LOCK)).unwrap().len(), 0);
    }

    #[test]
    #[ignore = "requires explicitly approved native root and anchor initialization"]
    fn process_death_releases_lock_without_takeover() {
        const CHILD_PATH: &str = "COPILOT_CONTROLLER_OWNER_LOCK_CHILD";
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let root = PrivateRoot::open(path).unwrap();
            let _lease = RuntimeOwnerLease::acquire(&root).unwrap();
            println!("OWNER_LOCK_HELD");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            loop {
                thread::sleep(Duration::from_secs(1));
            }
        }

        let fixture = Fixture::new();
        let root = PrivateRoot::open(&fixture.0).unwrap();
        root.check_mutation_support().unwrap();
        root.create_exclusive(&Name::new(OWNER_LOCK).unwrap(), &[])
            .unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "persistence::ownership::tests::process_death_releases_lock_without_takeover",
                "--include-ignored",
                "--nocapture",
            ])
            .env(CHILD_PATH, &fixture.0)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut marker_seen = false;
        while Instant::now() < deadline {
            match receiver.recv_timeout(deadline - Instant::now()) {
                Ok(line) if line == "OWNER_LOCK_HELD" => {
                    marker_seen = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let busy = matches!(
            RuntimeOwnerLease::acquire(&root),
            Err(error) if error.kind() == OwnershipErrorKind::Busy
        );
        let _ = child.kill();
        child.wait().unwrap();
        reader.join().unwrap();
        assert!(
            marker_seen,
            "child failed to report acquiring the owner lock"
        );
        assert!(busy, "a live owner lock was taken over");
        assert!(RuntimeOwnerLease::acquire(&root).is_ok());
    }
}
