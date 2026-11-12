//! Explicitly selected private roots and descriptor-pinned state files.
//!
//! The caller supplies an existing absolute directory; this module never chooses
//! a home or temporary directory.  The lock protects only cooperating users and
//! is not a security boundary against arbitrary code running as the same user.
//!
//! Unix builds require `libc`. Windows builds require `windows-sys` with the
//! Foundation, Security, Security_Authorization, Storage_FileSystem, System_IO,
//! and System_Threading features. Windows read-only operations and locking an
//! existing anchor are implemented, but mutations and lock-file creation fail
//! closed with `windows_directory_durability_unavailable`: crash/power-loss
//! persistence of namespace creation, replacement, and evidence removal is not
//! established by file flushes or the available unprivileged replace operation.
//! Fixed local disks do not remove this limitation. Unsupported targets fail
//! closed.

use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use unix as native;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as native;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
use unsupported as native;

/// A separate ceiling prevents callers from accidentally requesting unbounded
/// allocations while comparing recovery evidence.
pub const MAX_BOUNDED_READ: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    InvalidPath,
    InvalidName,
    UnsafeRoot,
    UnsafeFile,
    UnsafeAcl,
    UnsupportedFilesystem,
    UnsupportedPlatform,
    NotFound,
    AlreadyExists,
    Busy,
    Changed,
    EvidenceMismatch,
    TooLarge,
    Io,
    /// An operation may have created or published bytes. Do not retry the
    /// logical mutation or discard any remaining evidence automatically.
    Indeterminate,
    /// Cleanup after synchronized publication or explicit discard could not be
    /// confirmed. Do not retry the logical mutation.
    CleanupUncertain,
}

/// Errors contain no path, filename, or file contents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    kind: ErrorKind,
    operation: &'static str,
    native_code: Option<i32>,
    cause: Option<ErrorKind>,
}

pub type Result<T> = std::result::Result<T, Error>;
pub type RootError = Error;
pub type RootErrorKind = ErrorKind;

#[cfg(all(test, target_os = "windows"))]
pub(crate) fn create_test_private_directory(path: &Path) -> Result<()> {
    native::create_test_private_directory(path)
}

impl Error {
    fn new(kind: ErrorKind, operation: &'static str) -> Self {
        Self {
            kind,
            operation,
            native_code: None,
            cause: None,
        }
    }

    fn os(kind: ErrorKind, operation: &'static str, error: std::io::Error) -> Self {
        Self {
            kind,
            operation,
            native_code: error.raw_os_error(),
            cause: None,
        }
    }

    fn indeterminate(error: Self) -> Self {
        if error.kind == ErrorKind::Indeterminate {
            return error;
        }
        Self {
            kind: ErrorKind::Indeterminate,
            operation: error.operation,
            native_code: error.native_code,
            cause: Some(error.kind),
        }
    }

    fn cleanup_uncertain(error: Self) -> Self {
        Self {
            kind: ErrorKind::CleanupUncertain,
            operation: error.operation,
            native_code: error.native_code,
            cause: Some(error.kind),
        }
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn operation(&self) -> &'static str {
        self.operation
    }

    pub fn native_code(&self) -> Option<i32> {
        self.native_code
    }

    pub fn cause_kind(&self) -> Option<ErrorKind> {
        self.cause
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "private-root {:?}: {}", self.kind, self.operation)?;
        if let Some(code) = self.native_code {
            write!(f, " (native {code})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

/// A portable single path component. Windows aliases and reserved device names
/// are rejected even on Unix so that a name has the same meaning on all hosts.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Name(String);

impl Name {
    pub fn new(value: impl AsRef<str>) -> Result<Self> {
        let value = value.as_ref();
        let reserved = value.split('.').next().is_some_and(is_windows_device_name);
        if value.is_empty()
            || value == "."
            || value == ".."
            || value.len() > 200
            || value.ends_with(['.', ' '])
            || reserved
            || value.chars().any(|c| {
                c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
            })
        {
            return Err(Error::new(ErrorKind::InvalidName, "basename"));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn replacement(&self) -> Result<Self> {
        Self::new(format!("{}.replacement", self.0))
    }
}

fn is_windows_device_name(stem: &str) -> bool {
    let stem = stem.to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

/// A pinned root. Clones refer to the same pinned directory, not a new path.
#[derive(Clone)]
pub struct PrivateRoot {
    inner: Arc<native::Root>,
}

impl PrivateRoot {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(native::Root::open(path.as_ref())?),
        })
    }

    /// Rewalk the originally supplied path and verify the pinned identity.
    pub fn validate(&self) -> Result<()> {
        self.inner.validate()
    }

    /// Probe whether durable mutations are supported without creating a file.
    pub fn check_mutation_support(&self) -> Result<()> {
        self.validate()?;
        self.inner.sync_dir()?;
        self.validate()
    }

    /// Open a snapshot reader. It may keep reading its original inode after a
    /// later atomic replacement; `verify_current` instead requires the name to
    /// still designate this exact file.
    pub fn open_read(&self, name: &Name) -> Result<Option<ReadFile>> {
        self.validate()?;
        let Some(file) = self.inner.open_file(name, false)? else {
            self.validate()?;
            return Ok(None);
        };
        self.inner.check_file(Some(name), &file, false)?;
        self.validate()?;
        Ok(Some(ReadFile {
            root: self.clone(),
            name: name.clone(),
            file,
        }))
    }

    /// Read only if the name still designates the opened file afterwards.
    pub fn read(&self, name: &Name, limit: usize) -> Result<Option<Vec<u8>>> {
        let Some(mut file) = self.open_read(name)? else {
            return Ok(None);
        };
        let bytes = file.read_bounded(limit)?;
        file.verify_current()?;
        Ok(Some(bytes))
    }

    /// Create a new artifact without ever truncating or overwriting another
    /// name. A failure after creation is indeterminate and leaves the artifact.
    pub fn create_exclusive(&self, name: &Name, bytes: &[u8]) -> Result<()> {
        self.validate()?;
        self.inner.sync_dir()?;
        let mut file = self.inner.create_file(name)?;
        let result = (|| {
            self.inner.check_file(Some(name), &file, false)?;
            self.validate()?;
            self.inner.sync_dir()?;
            write_contents(&mut file.file, bytes)?;
            native::sync_file(&file.file)?;
            self.inner.check_file(Some(name), &file, false)?;
            self.validate()?;
            self.inner.sync_dir()?;
            self.inner.check_file(Some(name), &file, false)?;
            self.validate()
        })();
        if let Err(error) = result {
            let _ = native::sync_file(&file.file);
            let _ = self.inner.sync_dir();
            return Err(Error::indeterminate(error));
        }
        Ok(())
    }

    /// Try once; the returned guard owns the OS lock until dropped.
    pub fn try_lock(&self, name: &Name) -> Result<LockGuard> {
        self.validate()?;
        let file = match self.inner.open_file(name, true)? {
            Some(file) => file,
            None => {
                self.inner.sync_dir()?;
                match self.inner.create_file(name) {
                    Ok(file) => {
                        let result = (|| {
                            self.inner.check_file(Some(name), &file, false)?;
                            native::sync_file(&file.file)?;
                            self.inner.sync_dir()
                        })();
                        if let Err(error) = result {
                            let _ = self.inner.sync_dir();
                            return Err(Error::indeterminate(error));
                        }
                        file
                    }
                    Err(error) if error.kind == ErrorKind::AlreadyExists => self
                        .inner
                        .open_file(name, true)?
                        .ok_or_else(|| Error::new(ErrorKind::Changed, "lock_open"))?,
                    Err(error) => return Err(error),
                }
            }
        };
        if self.inner.check_file(Some(name), &file, false)? != 0 {
            return Err(Error::new(ErrorKind::Changed, "lock_nonempty"));
        }
        native::try_lock(&file.file)?;
        let guard = LockGuard {
            root: self.clone(),
            name: name.clone(),
            file,
        };
        guard.validate()?;
        Ok(guard)
    }

    /// Try a nonblocking lock on an existing anchor without creating it.
    pub fn try_lock_existing(&self, name: &Name) -> Result<LockGuard> {
        self.validate()?;
        let file = self
            .inner
            .open_file(name, true)?
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "lock_open"))?;
        if self.inner.check_file(Some(name), &file, false)? != 0 {
            return Err(Error::new(ErrorKind::Changed, "lock_nonempty"));
        }
        native::try_lock(&file.file)?;
        let guard = LockGuard {
            root: self.clone(),
            name: name.clone(),
            file,
        };
        guard.validate()?;
        Ok(guard)
    }

    /// The state-store policy: at most 20 attempts, with 10 ms between them.
    pub fn lock_bounded(&self, name: &Name) -> Result<LockGuard> {
        for attempt in 0..20 {
            match self.try_lock(name) {
                Err(error) if error.kind == ErrorKind::Busy && attempt < 19 => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                result => return result,
            }
        }
        unreachable!("the last lock attempt returns")
    }
}

fn write_contents(writer: &mut impl Write, bytes: &[u8]) -> Result<()> {
    writer
        .write_all(bytes)
        .map_err(|error| Error::os(ErrorKind::Io, "write_file", error))
}

pub struct ReadFile {
    root: PrivateRoot,
    name: Name,
    file: native::File,
}

/// Opaque identity for one opened state file. It is process-local and must not
/// be persisted as a cross-boot identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileIdentity(native::Identity);

/// Opaque native identity for a read-only event file. It is not a process or
/// authority identity and must not be persisted as a cross-boot guarantee.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TailIdentity(native::Identity);

/// Handle-derived last-write and metadata-change timestamps. Compare for
/// equality only; this is not a content digest or an unforgeable generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TailChangeIdentity(native::ChangeIdentity);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TailMetadata {
    pub identity: TailIdentity,
    pub len: u64,
    pub change: TailChangeIdentity,
}

/// A read-only, no-follow event-file handle. Unlike private state, its file
/// permissions and filesystem are not required to be private or local.
pub struct TailFile {
    inner: native::Tail,
    observed: TailMetadata,
}

impl TailFile {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let inner = native::Tail::open(path.as_ref())?;
        let (len, change) = inner.validate()?;
        Ok(Self {
            observed: TailMetadata {
                identity: TailIdentity(inner.id),
                len,
                change: TailChangeIdentity(change),
            },
            inner,
        })
    }

    /// Pollers must retain this snapshot with their offset. A changed `change`
    /// at a previously observed EOF requires restarting reduction from zero,
    /// even when the file identity and length have not changed.
    pub fn metadata(&self) -> Result<TailMetadata> {
        let (len, change) = self.inner.validate()?;
        Ok(TailMetadata {
            identity: TailIdentity(self.inner.id),
            len,
            change: TailChangeIdentity(change),
        })
    }

    /// Read at most `limit` bytes. Reject replacement, truncation, or a
    /// same-size modification during or since the last successful read/open.
    /// Reopen after a source-change error; interpreting records and resetting
    /// reducer state belongs to the higher-level tailer.
    pub fn read_at(&mut self, offset: u64, limit: usize) -> Result<Vec<u8>> {
        self.read_at_using(offset, limit, |file, limit| {
            let mut bytes = Vec::new();
            file.take(limit as u64).read_to_end(&mut bytes)?;
            Ok(bytes)
        })
    }

    fn read_at_using(
        &mut self,
        offset: u64,
        limit: usize,
        read: impl FnOnce(&mut std::fs::File, usize) -> std::io::Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        if limit > MAX_BOUNDED_READ {
            return Err(Error::new(ErrorKind::TooLarge, "tail_bound"));
        }
        let before = self.metadata()?;
        let inner = &mut self.inner;
        read_tail_at(&mut self.observed, before, offset, || {
            inner
                .file
                .seek(SeekFrom::Start(offset))
                .map_err(|e| Error::os(ErrorKind::Io, "tail_seek", e))?;
            let bytes = read(&mut inner.file, limit)
                .map_err(|e| Error::os(ErrorKind::Io, "tail_read", e))?;
            let (len, change) = inner.validate()?;
            Ok((
                bytes,
                TailMetadata {
                    identity: TailIdentity(inner.id),
                    len,
                    change: TailChangeIdentity(change),
                },
            ))
        })
    }
}

fn read_tail_at(
    observed: &mut TailMetadata,
    before: TailMetadata,
    offset: u64,
    read: impl FnOnce() -> Result<(Vec<u8>, TailMetadata)>,
) -> Result<Vec<u8>> {
    if before.len == observed.len && before.change != observed.change {
        return Err(Error::new(ErrorKind::Changed, "tail_modified"));
    }
    if offset > before.len {
        return Err(Error::new(ErrorKind::Changed, "tail_truncated"));
    }
    let (bytes, after) = read()?;
    if after.len < before.len || offset.saturating_add(bytes.len() as u64) > after.len {
        return Err(Error::new(ErrorKind::Changed, "tail_truncated"));
    }
    if after.len == before.len && after.change != before.change {
        return Err(Error::new(ErrorKind::Changed, "tail_modified"));
    }
    *observed = after;
    Ok(bytes)
}

impl ReadFile {
    pub fn identity(&self) -> FileIdentity {
        FileIdentity(self.file.id)
    }

    pub fn verify_current(&self) -> Result<()> {
        self.root.validate()?;
        self.root
            .inner
            .check_file(Some(&self.name), &self.file, false)?;
        self.root.validate()
    }

    /// Read the opened file, including an old unlinked snapshot. Changed
    /// contents, unsafe permissions, or a changed root fail closed.
    pub fn read_bounded(&mut self, limit: usize) -> Result<Vec<u8>> {
        if limit > MAX_BOUNDED_READ {
            return Err(Error::new(ErrorKind::TooLarge, "read_bound"));
        }
        self.root.validate()?;
        let len = self.root.inner.check_file(None, &self.file, true)?;
        if len > limit as u64 {
            return Err(Error::new(ErrorKind::TooLarge, "read_file"));
        }
        let modified = self
            .file
            .file
            .metadata()
            .and_then(|m| m.modified())
            .map_err(|e| Error::os(ErrorKind::Io, "read_metadata", e))?;
        self.file
            .file
            .seek(SeekFrom::Start(0))
            .map_err(|e| Error::os(ErrorKind::Io, "seek_file", e))?;
        let mut bytes = Vec::with_capacity(len as usize);
        (&mut self.file.file)
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::os(ErrorKind::Io, "read_file", e))?;
        if bytes.len() > limit {
            return Err(Error::new(ErrorKind::TooLarge, "read_file"));
        }
        let after_len = self.root.inner.check_file(None, &self.file, true)?;
        let after_modified = self
            .file
            .file
            .metadata()
            .and_then(|m| m.modified())
            .map_err(|e| Error::os(ErrorKind::Io, "read_metadata", e))?;
        self.root.validate()?;
        if len != after_len || len != bytes.len() as u64 || modified != after_modified {
            return Err(Error::new(ErrorKind::Changed, "read_changed"));
        }
        Ok(bytes)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishMode {
    /// Atomically install only if the destination does not exist.
    Create,
    /// Atomically replace an already existing safe destination.
    Replace,
}

pub struct LockGuard {
    root: PrivateRoot,
    name: Name,
    file: native::File,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublishFailpoint {
    BeforeRename,
    AfterRename,
}

impl LockGuard {
    /// Verify the root, the held file, its name association, and zero length.
    pub fn validate(&self) -> Result<()> {
        self.root.validate()?;
        if self
            .root
            .inner
            .check_file(Some(&self.name), &self.file, false)?
            != 0
        {
            return Err(Error::new(ErrorKind::Changed, "lock_nonempty"));
        }
        self.root.validate()
    }

    pub fn is_empty_except_lock(&self) -> Result<bool> {
        self.validate()?;
        let empty = self.root.inner.is_empty_except(&self.name)?;
        self.validate()?;
        Ok(empty)
    }

    pub fn create_exclusive(&self, name: &Name, bytes: &[u8]) -> Result<()> {
        self.validate()?;
        self.root.create_exclusive(name, bytes)?;
        self.validate().map_err(Error::indeterminate)
    }

    /// Synchronize a recognized current file during explicit recovery.
    pub fn sync_file(&self, name: &Name) -> Result<()> {
        self.validate()?;
        let file = self
            .root
            .open_read(name)?
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "sync_file_missing"))?;
        if file.file.id == self.file.id {
            return Err(Error::new(ErrorKind::InvalidName, "sync_lock"));
        }
        self.root.inner.sync_named(name, &file.file)?;
        file.verify_current()?;
        self.validate()
    }

    /// Publish previously validated prepared bytes without modifying an old
    /// open reader. The `.replacement` copy and prepared file are never
    /// removed on an earlier write, validation, comparison, or rename failure.
    /// The caller must validate the schema, store identity, and revision.
    pub fn publish_prepared(
        &self,
        prepared: &Name,
        destination: &Name,
        mode: PublishMode,
        expected_prepared: &[u8],
        limit: usize,
    ) -> Result<()> {
        self.publish_prepared_inner(prepared, destination, mode, expected_prepared, limit, None)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn publish_prepared_with_failpoint(
        &self,
        prepared: &Name,
        destination: &Name,
        mode: PublishMode,
        expected_prepared: &[u8],
        limit: usize,
        failpoint: PublishFailpoint,
    ) -> Result<()> {
        self.publish_prepared_inner(
            prepared,
            destination,
            mode,
            expected_prepared,
            limit,
            Some(failpoint),
        )
    }

    fn publish_prepared_inner(
        &self,
        prepared: &Name,
        destination: &Name,
        mode: PublishMode,
        expected_prepared: &[u8],
        limit: usize,
        failpoint: Option<PublishFailpoint>,
    ) -> Result<()> {
        self.validate()?;
        self.root.check_mutation_support()?;
        if prepared == destination {
            return Err(Error::new(ErrorKind::InvalidName, "publish_names"));
        }
        let replacement = prepared.replacement()?;
        if &replacement == destination
            || replacement == self.name
            || prepared == &self.name
            || destination == &self.name
        {
            return Err(Error::new(ErrorKind::InvalidName, "publish_names"));
        }
        let mut source = self
            .root
            .open_read(prepared)?
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "prepared_open"))?;
        let bytes = source.read_bounded(limit)?;
        source.verify_current()?;
        if bytes != expected_prepared {
            return Err(Error::new(
                ErrorKind::EvidenceMismatch,
                "prepared_unrecognized",
            ));
        }
        if source.file.id == self.file.id {
            return Err(Error::new(ErrorKind::Changed, "publish_lock_alias"));
        }
        let destination_before = self.root.open_read(destination)?;
        match (mode, &destination_before) {
            (PublishMode::Create, Some(_)) => {
                return Err(Error::new(ErrorKind::AlreadyExists, "publish_destination"));
            }
            (PublishMode::Replace, None) => {
                return Err(Error::new(ErrorKind::NotFound, "publish_destination"));
            }
            _ => {}
        }
        if destination_before
            .as_ref()
            .is_some_and(|file| file.file.id == source.file.id || file.file.id == self.file.id)
        {
            return Err(Error::new(ErrorKind::Changed, "publish_alias"));
        }
        drop(destination_before);

        let candidate_existed = self.root.open_read(&replacement)?.is_some();
        if !candidate_existed {
            self.create_exclusive(&replacement, &bytes)?;
        }
        let mut publication_attempted = false;
        let work = (|| {
            let mut candidate = self
                .root
                .open_read(&replacement)?
                .ok_or_else(|| Error::new(ErrorKind::Changed, "replacement_open"))?;
            if candidate.read_bounded(limit)? != bytes {
                return Err(Error::new(
                    ErrorKind::EvidenceMismatch,
                    "replacement_compare",
                ));
            }
            if candidate.file.id == source.file.id || candidate.file.id == self.file.id {
                return Err(Error::new(ErrorKind::Changed, "replacement_alias"));
            }
            candidate.verify_current()?;
            source.verify_current()?;
            let destination_file = self.root.open_read(destination)?;
            match (mode, &destination_file) {
                (PublishMode::Create, Some(_)) => {
                    return Err(Error::new(ErrorKind::AlreadyExists, "publish_destination"));
                }
                (PublishMode::Replace, None) => {
                    return Err(Error::new(ErrorKind::NotFound, "publish_destination"));
                }
                _ => {}
            }
            if destination_file.as_ref().is_some_and(|file| {
                file.file.id == source.file.id
                    || file.file.id == candidate.file.id
                    || file.file.id == self.file.id
            }) {
                return Err(Error::new(ErrorKind::Changed, "publish_alias"));
            }
            self.root.inner.sync_named(&replacement, &candidate.file)?;
            self.validate()?;
            if source.read_bounded(limit)? != bytes || candidate.read_bounded(limit)? != bytes {
                return Err(Error::new(ErrorKind::EvidenceMismatch, "prepared_changed"));
            }
            source.verify_current()?;
            candidate.verify_current()?;
            if let Some(file) = &destination_file {
                file.verify_current()?;
            }
            let candidate_id = candidate.file.id;
            let destination_id = destination_file.as_ref().map(|file| file.file.id);
            drop(candidate);
            drop(destination_file);
            if failpoint == Some(PublishFailpoint::BeforeRename) {
                return Err(Error::new(ErrorKind::Io, "injected_before_rename"));
            }
            publication_attempted = true;
            self.root
                .inner
                .rename(
                    &replacement,
                    destination,
                    mode,
                    candidate_id,
                    destination_id,
                )
                .map_err(Error::indeterminate)?;
            let published = self
                .root
                .open_read(destination)?
                .ok_or_else(|| Error::new(ErrorKind::Changed, "publish_missing"))?;
            if published.file.id != candidate_id {
                return Err(Error::new(ErrorKind::Changed, "publish_identity"));
            }
            published.verify_current()?;
            self.validate()?;
            self.root.inner.sync_dir()?;
            if failpoint == Some(PublishFailpoint::AfterRename) {
                return Err(Error::new(ErrorKind::Io, "injected_after_rename"));
            }
            source.verify_current()?;
            self.validate()?;
            let source_id = source.file.id;
            drop(source);
            Ok(source_id)
        })();
        let source_id = if candidate_existed && !publication_attempted {
            // Before rename, a mismatching preexisting candidate is simply
            // unrecognized evidence; all evidence remains untouched.
            work?
        } else {
            work.map_err(Error::indeterminate)?
        };
        // The committed bytes and directory entry were synchronized and the
        // root and lock were revalidated before beginning evidence cleanup.
        self.root
            .inner
            .unlink(prepared, source_id)
            .map_err(Error::cleanup_uncertain)?;
        self.root
            .inner
            .sync_dir()
            .map_err(Error::cleanup_uncertain)?;
        self.validate().map_err(Error::cleanup_uncertain)
    }

    /// Explicit recovery discard of caller-validated bytes. An existing
    /// replacement must match those bounded bytes before either artifact may be
    /// removed. The caller must validate the schema, store identity, revision,
    /// and whether discarding is allowed.
    pub fn discard_prepared(
        &self,
        prepared: &Name,
        expected_prepared: &[u8],
        limit: usize,
    ) -> Result<()> {
        self.validate()?;
        self.root.check_mutation_support()?;
        if prepared == &self.name {
            return Err(Error::new(ErrorKind::InvalidName, "discard_names"));
        }
        let replacement = prepared.replacement()?;
        if replacement == self.name {
            return Err(Error::new(ErrorKind::InvalidName, "discard_names"));
        }
        let mut source = self
            .root
            .open_read(prepared)?
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "prepared_open"))?;
        let bytes = source.read_bounded(limit)?;
        source.verify_current()?;
        if bytes != expected_prepared {
            return Err(Error::new(
                ErrorKind::EvidenceMismatch,
                "prepared_unrecognized",
            ));
        }
        if source.file.id == self.file.id {
            return Err(Error::new(ErrorKind::Changed, "discard_lock_alias"));
        }
        let candidate = self.root.open_read(&replacement)?;
        if let Some(mut candidate) = candidate {
            if candidate.read_bounded(limit)? != bytes {
                return Err(Error::new(
                    ErrorKind::EvidenceMismatch,
                    "replacement_compare",
                ));
            }
            if candidate.file.id == source.file.id || candidate.file.id == self.file.id {
                return Err(Error::new(ErrorKind::Changed, "discard_alias"));
            }
            candidate.verify_current()?;
            source.verify_current()?;
            self.validate()?;
            let candidate_id = candidate.file.id;
            drop(candidate);
            self.root
                .inner
                .unlink(&replacement, candidate_id)
                .map_err(Error::indeterminate)?;
            self.root.inner.sync_dir().map_err(Error::indeterminate)?;
        }
        source.verify_current()?;
        self.validate()?;
        let source_id = source.file.id;
        drop(source);
        self.root
            .inner
            .unlink(prepared, source_id)
            .map_err(Error::cleanup_uncertain)?;
        self.root
            .inner
            .sync_dir()
            .map_err(Error::cleanup_uncertain)?;
        self.validate().map_err(Error::cleanup_uncertain)
    }
}

#[cfg(test)]
mod tests;
