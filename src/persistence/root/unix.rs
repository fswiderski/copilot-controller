use super::{Error, ErrorKind, Name, PublishMode, Result};
use std::ffi::{CStr, CString};
use std::fs::File as StdFile;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const DIR_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
const FILE_FLAGS: libc::c_int = libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Identity {
    dev: u64,
    ino: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ChangeIdentity {
    modified: (i64, i64),
    changed: (i64, i64),
}

pub(super) struct File {
    pub(super) file: StdFile,
    pub(super) id: Identity,
}

pub(super) struct Root {
    directory: StdFile,
    components: Vec<CString>,
    identity: Identity,
    uid: libc::uid_t,
}

pub(super) struct Tail {
    pub(super) file: StdFile,
    pub(super) id: Identity,
    parent: StdFile,
    parent_id: Identity,
    parent_components: Vec<CString>,
    name: CString,
    uid: libc::uid_t,
}

impl Tail {
    pub(super) fn open(path: &Path) -> Result<Self> {
        if path.as_os_str().as_bytes().last() == Some(&b'/') {
            return Err(Error::new(ErrorKind::InvalidPath, "tail_name"));
        }
        let mut components = path_components(path)?;
        let name = components
            .pop()
            .ok_or_else(|| Error::new(ErrorKind::InvalidPath, "tail_name"))?;
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let parent = walk_tail_ancestors(&components, uid)?;
        let parent_id = identity(&stat_fd(&parent, "tail_parent_stat")?);
        let before =
            stat_at(&parent, &name)?.ok_or_else(|| Error::new(ErrorKind::NotFound, "tail_open"))?;
        check_tail_file(&before)?;
        // SAFETY: the name is a single NUL-terminated component under parent.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                FILE_FLAGS | libc::O_RDONLY,
            )
        };
        if fd < 0 {
            return Err(Error::os(
                ErrorKind::Io,
                "tail_open",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: openat returned a new owned descriptor.
        let file = unsafe { StdFile::from_raw_fd(fd) };
        let after = stat_fd(&file, "tail_stat")?;
        check_tail_file(&after)?;
        if identity(&before) != identity(&after) {
            return Err(Error::new(ErrorKind::Changed, "tail_open_identity"));
        }
        let tail = Self {
            id: identity(&after),
            file,
            parent,
            parent_id,
            parent_components: components,
            name,
            uid,
        };
        tail.validate()?;
        Ok(tail)
    }

    pub(super) fn validate(&self) -> Result<(u64, ChangeIdentity)> {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != self.uid {
            return Err(Error::new(ErrorKind::Changed, "effective_user"));
        }
        let parent_stat = stat_fd(&self.parent, "tail_parent_stat")?;
        check_ancestor(&parent_stat, self.uid)?;
        check_ancestor_filesystem(&self.parent)?;
        check_acl(&self.parent)?;
        if identity(&parent_stat) != self.parent_id {
            return Err(Error::new(ErrorKind::Changed, "tail_parent_identity"));
        }
        let reopened = walk_tail_ancestors(&self.parent_components, self.uid)?;
        if identity(&stat_fd(&reopened, "tail_parent_path")?) != self.parent_id {
            return Err(Error::new(ErrorKind::Changed, "tail_parent_path_identity"));
        }
        let held = stat_fd(&self.file, "tail_stat")?;
        check_tail_file(&held)?;
        let named = stat_at(&self.parent, &self.name)?
            .ok_or_else(|| Error::new(ErrorKind::Changed, "tail_name_missing"))?;
        check_tail_file(&named)?;
        if identity(&held) != self.id || identity(&named) != self.id {
            return Err(Error::new(ErrorKind::Changed, "tail_identity"));
        }
        tail_snapshot(&held)
    }
}

fn tail_snapshot(stat: &libc::stat) -> Result<(u64, ChangeIdentity)> {
    let len =
        u64::try_from(stat.st_size).map_err(|_| Error::new(ErrorKind::UnsafeFile, "tail_size"))?;
    Ok((
        len,
        ChangeIdentity {
            modified: (stat.st_mtime, stat.st_mtime_nsec),
            changed: (stat.st_ctime, stat.st_ctime_nsec),
        },
    ))
}

impl Root {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let components = path_components(path)?;
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let directory = walk(&components, uid)?;
        let identity = identity(&stat_fd(&directory, "root_stat")?);
        Ok(Self {
            directory,
            components,
            identity,
            uid,
        })
    }

    pub(super) fn validate(&self) -> Result<()> {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != self.uid {
            return Err(Error::new(ErrorKind::Changed, "effective_user"));
        }
        let current = stat_fd(&self.directory, "root_stat")?;
        check_private_directory(&current, self.uid)?;
        check_acl(&self.directory)?;
        check_filesystem(&self.directory)?;
        if identity(&current) != self.identity {
            return Err(Error::new(ErrorKind::Changed, "root_identity"));
        }
        let reopened = walk(&self.components, self.uid)?;
        if identity(&stat_fd(&reopened, "root_path_stat")?) != self.identity {
            return Err(Error::new(ErrorKind::Changed, "root_path_identity"));
        }
        Ok(())
    }

    pub(super) fn open_file(&self, name: &Name, writable: bool) -> Result<Option<File>> {
        let Some(before) = self.stat_name(name)? else {
            return Ok(None);
        };
        check_private_file(&before, self.uid, self.identity.dev, false)?;
        let name_c = name_cstring(name);
        let flags = FILE_FLAGS
            | if writable {
                libc::O_RDWR
            } else {
                libc::O_RDONLY
            };
        // SAFETY: directory and the NUL-terminated name remain alive for the call.
        let fd = unsafe { libc::openat(self.directory.as_raw_fd(), name_c.as_ptr(), flags) };
        if fd < 0 {
            let error = io::Error::last_os_error();
            let kind = if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOENT)) {
                ErrorKind::Changed
            } else {
                ErrorKind::Io
            };
            return Err(Error::os(kind, "file_open", error));
        }
        // SAFETY: openat returned a new owned descriptor.
        let file = unsafe { StdFile::from_raw_fd(fd) };
        let after = stat_fd(&file, "file_stat")?;
        if identity(&before) != identity(&after) {
            return Err(Error::new(ErrorKind::Changed, "file_open_identity"));
        }
        let opened = File {
            id: identity(&after),
            file,
        };
        self.check_file(Some(name), &opened, false)?;
        Ok(Some(opened))
    }

    pub(super) fn create_file(&self, name: &Name) -> Result<File> {
        let name_c = name_cstring(name);
        let flags = FILE_FLAGS | libc::O_RDWR | libc::O_CREAT | libc::O_EXCL;
        // SAFETY: the name is valid and O_CREAT is accompanied by a mode.
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name_c.as_ptr(),
                flags,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            let error = io::Error::last_os_error();
            let kind = if error.raw_os_error() == Some(libc::EEXIST) {
                ErrorKind::AlreadyExists
            } else {
                ErrorKind::Io
            };
            return Err(Error::os(kind, "file_create", error));
        }
        // SAFETY: openat returned a new owned descriptor.
        let file = unsafe { StdFile::from_raw_fd(fd) };
        let stat = stat_fd(&file, "created_stat").map_err(Error::indeterminate)?;
        Ok(File {
            id: identity(&stat),
            file,
        })
    }

    pub(super) fn check_file(
        &self,
        name: Option<&Name>,
        file: &File,
        allow_unlinked: bool,
    ) -> Result<u64> {
        let stat = stat_fd(&file.file, "file_stat")?;
        check_private_file(&stat, self.uid, self.identity.dev, allow_unlinked)?;
        check_acl(&file.file)?;
        if identity(&stat) != file.id {
            return Err(Error::new(ErrorKind::Changed, "file_identity"));
        }
        if let Some(name) = name {
            let named = self
                .stat_name(name)?
                .ok_or_else(|| Error::new(ErrorKind::Changed, "file_name_missing"))?;
            check_private_file(&named, self.uid, self.identity.dev, false)?;
            if identity(&named) != file.id {
                return Err(Error::new(ErrorKind::Changed, "file_name_identity"));
            }
        }
        u64::try_from(stat.st_size).map_err(|_| Error::new(ErrorKind::UnsafeFile, "file_size"))
    }

    pub(super) fn sync_dir(&self) -> Result<()> {
        self.directory
            .sync_all()
            .map_err(|e| Error::os(ErrorKind::Io, "directory_sync", e))
    }

    pub(super) fn sync_named(&self, name: &Name, expected: &File) -> Result<()> {
        let writable = self
            .open_file(name, true)?
            .ok_or_else(|| Error::new(ErrorKind::Changed, "sync_name_missing"))?;
        if writable.id != expected.id {
            return Err(Error::new(ErrorKind::Changed, "sync_name_identity"));
        }
        sync_file(&writable.file)?;
        self.check_file(Some(name), &writable, false)?;
        Ok(())
    }

    pub(super) fn rename(
        &self,
        from: &Name,
        to: &Name,
        mode: PublishMode,
        source_id: Identity,
        destination_id: Option<Identity>,
    ) -> Result<()> {
        self.validate()?;
        let source = self
            .open_file(from, false)?
            .ok_or_else(|| Error::new(ErrorKind::Changed, "rename_source_missing"))?;
        if source.id != source_id {
            return Err(Error::new(ErrorKind::Changed, "rename_source_identity"));
        }
        let destination = self.open_file(to, false)?;
        if destination.as_ref().map(|file| file.id) != destination_id {
            return Err(Error::new(
                ErrorKind::Changed,
                "rename_destination_identity",
            ));
        }
        let from_c = name_cstring(from);
        let to_c = name_cstring(to);
        let dir = self.directory.as_raw_fd();
        let result = match mode {
            PublishMode::Replace => {
                // SAFETY: both names are components relative to the pinned directory.
                unsafe { libc::renameat(dir, from_c.as_ptr(), dir, to_c.as_ptr()) }
            }
            PublishMode::Create => rename_exclusive(dir, &from_c, &to_c),
        };
        if result == 0 {
            Ok(())
        } else {
            let error = io::Error::last_os_error();
            let kind = match error.raw_os_error() {
                Some(libc::EEXIST) => ErrorKind::AlreadyExists,
                Some(libc::ENOSYS | libc::EOPNOTSUPP | libc::EINVAL)
                    if mode == PublishMode::Create =>
                {
                    ErrorKind::UnsupportedFilesystem
                }
                _ => ErrorKind::Io,
            };
            Err(Error::os(kind, "atomic_rename", error))
        }
    }

    pub(super) fn unlink(&self, name: &Name, expected: Identity) -> Result<()> {
        self.validate()?;
        let file = self
            .open_file(name, false)?
            .ok_or_else(|| Error::new(ErrorKind::Changed, "unlink_name_missing"))?;
        if file.id != expected {
            return Err(Error::new(ErrorKind::Changed, "unlink_name_identity"));
        }
        let name_c = name_cstring(name);
        // SAFETY: the validated basename is relative to the pinned directory.
        if unsafe { libc::unlinkat(self.directory.as_raw_fd(), name_c.as_ptr(), 0) } != 0 {
            return Err(Error::os(
                ErrorKind::Io,
                "file_unlink",
                io::Error::last_os_error(),
            ));
        }
        Ok(())
    }

    pub(super) fn is_empty_except(&self, name: &Name) -> Result<bool> {
        let dot = c".";
        // Open a new file description: dup would share the directory offset.
        // SAFETY: the descriptor is valid and "." is a constant C string.
        let fd = unsafe { libc::openat(self.directory.as_raw_fd(), dot.as_ptr(), DIR_FLAGS) };
        if fd < 0 {
            return Err(Error::os(
                ErrorKind::Io,
                "directory_open",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: openat returned an owned descriptor.
        let directory = unsafe { StdFile::from_raw_fd(fd) };
        if identity(&stat_fd(&directory, "directory_stat")?) != self.identity {
            return Err(Error::new(ErrorKind::Changed, "directory_identity"));
        }
        let fd = directory.into_raw_fd();
        // SAFETY: fd is an owned directory descriptor. fdopendir consumes it on success.
        let dir = unsafe { libc::fdopendir(fd) };
        if dir.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: fdopendir did not consume the descriptor on failure.
            unsafe { libc::close(fd) };
            return Err(Error::os(ErrorKind::Io, "directory_stream", error));
        }
        let stream = DirectoryStream(dir);
        loop {
            set_errno_zero();
            // SAFETY: stream owns a live DIR pointer.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                return if error.raw_os_error().unwrap_or(0) == 0 {
                    Ok(true)
                } else {
                    Err(Error::os(ErrorKind::Io, "directory_read", error))
                };
            }
            // SAFETY: readdir returns a NUL-terminated d_name until the next call.
            let entry_name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if entry_name != b"." && entry_name != b".." && entry_name != name.as_str().as_bytes() {
                return Ok(false);
            }
        }
    }

    fn stat_name(&self, name: &Name) -> Result<Option<libc::stat>> {
        stat_at(&self.directory, &name_cstring(name))
    }
}

fn stat_at(directory: &StdFile, name: &CStr) -> Result<Option<libc::stat>> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: stat points to writable memory and name is NUL terminated.
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        // SAFETY: successful fstatat initialized the structure.
        Ok(Some(unsafe { stat.assume_init() }))
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(Error::os(ErrorKind::Io, "name_stat", error))
        }
    }
}

struct DirectoryStream(*mut libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this wrapper uniquely owns the DIR pointer.
        unsafe { libc::closedir(self.0) };
    }
}

fn name_cstring(name: &Name) -> CString {
    CString::new(name.as_str()).expect("Name rejects NUL")
}

fn path_components(path: &Path) -> Result<Vec<CString>> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.first() != Some(&b'/') || bytes.contains(&0) {
        return Err(Error::new(ErrorKind::InvalidPath, "absolute_root"));
    }
    let mut components = Vec::new();
    let rest = &bytes[1..];
    if rest.is_empty() {
        return Ok(components);
    }
    let mut parts = rest.split(|byte| *byte == b'/').peekable();
    while let Some(part) = parts.next() {
        if part.is_empty() && parts.peek().is_none() {
            break;
        }
        if part.is_empty()
            || part == b"."
            || part == b".."
            || part.contains(&b':')
            || part.contains(&b'\\')
        {
            return Err(Error::new(ErrorKind::InvalidPath, "root_component"));
        }
        components.push(
            CString::new(part).map_err(|_| Error::new(ErrorKind::InvalidPath, "root_component"))?,
        );
    }
    Ok(components)
}

fn walk(components: &[CString], uid: libc::uid_t) -> Result<StdFile> {
    // SAFETY: "/" is a constant C string and the flags require a directory.
    let fd = unsafe { libc::open(c"/".as_ptr(), DIR_FLAGS) };
    if fd < 0 {
        return Err(Error::os(
            ErrorKind::UnsafeRoot,
            "root_walk",
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: open returned a new owned descriptor.
    let mut current = unsafe { StdFile::from_raw_fd(fd) };
    for component in components {
        let stat = stat_fd(&current, "ancestor_stat")?;
        check_ancestor(&stat, uid)?;
        check_ancestor_filesystem(&current)?;
        check_acl(&current)?;
        // SAFETY: component and current descriptor remain valid during openat.
        let next = unsafe { libc::openat(current.as_raw_fd(), component.as_ptr(), DIR_FLAGS) };
        if next < 0 {
            return Err(Error::os(
                ErrorKind::UnsafeRoot,
                "ancestor_open",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: openat returned a new owned descriptor.
        current = unsafe { StdFile::from_raw_fd(next) };
    }
    check_private_directory(&stat_fd(&current, "root_stat")?, uid)?;
    check_acl(&current)?;
    check_filesystem(&current)?;
    Ok(current)
}

fn walk_tail_ancestors(components: &[CString], uid: libc::uid_t) -> Result<StdFile> {
    // SAFETY: "/" is a constant C string and the flags require a directory.
    let fd = unsafe { libc::open(c"/".as_ptr(), DIR_FLAGS) };
    if fd < 0 {
        return Err(Error::os(
            ErrorKind::UnsafeRoot,
            "tail_walk",
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: open returned a new owned descriptor.
    let mut current = unsafe { StdFile::from_raw_fd(fd) };
    for component in components {
        check_ancestor(&stat_fd(&current, "tail_ancestor_stat")?, uid)?;
        check_ancestor_filesystem(&current)?;
        check_acl(&current)?;
        // SAFETY: component and current descriptor remain valid during openat.
        let next = unsafe { libc::openat(current.as_raw_fd(), component.as_ptr(), DIR_FLAGS) };
        if next < 0 {
            return Err(Error::os(
                ErrorKind::UnsafeRoot,
                "tail_ancestor_open",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: openat returned a new owned descriptor.
        current = unsafe { StdFile::from_raw_fd(next) };
    }
    check_ancestor(&stat_fd(&current, "tail_ancestor_stat")?, uid)?;
    check_ancestor_filesystem(&current)?;
    check_acl(&current)?;
    Ok(current)
}

fn stat_fd(file: &StdFile, operation: &'static str) -> Result<libc::stat> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: stat points to writable memory and file owns a live descriptor.
    if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(Error::os(
            ErrorKind::Io,
            operation,
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: successful fstat initialized the structure.
    Ok(unsafe { stat.assume_init() })
}

fn identity(stat: &libc::stat) -> Identity {
    Identity {
        dev: device(stat),
        ino: stat.st_ino,
    }
}

#[cfg(target_os = "linux")]
fn device(stat: &libc::stat) -> u64 {
    stat.st_dev
}

#[cfg(target_os = "macos")]
fn device(stat: &libc::stat) -> u64 {
    stat.st_dev as u64
}

fn check_ancestor(stat: &libc::stat, uid: libc::uid_t) -> Result<()> {
    let mode = stat.st_mode;
    if mode & libc::S_IFMT != libc::S_IFDIR
        || (stat.st_uid != 0 && stat.st_uid != uid)
        || (mode & 0o022 != 0 && !(stat.st_uid == 0 && mode & libc::S_ISVTX != 0))
    {
        return Err(Error::new(ErrorKind::UnsafeRoot, "ancestor_permissions"));
    }
    Ok(())
}

fn check_private_directory(stat: &libc::stat, uid: libc::uid_t) -> Result<()> {
    let mode = stat.st_mode;
    if mode & libc::S_IFMT != libc::S_IFDIR || stat.st_uid != uid || mode & 0o7077 != 0 {
        return Err(Error::new(ErrorKind::UnsafeRoot, "root_permissions"));
    }
    Ok(())
}

fn check_private_file(
    stat: &libc::stat,
    uid: libc::uid_t,
    root_device: u64,
    allow_unlinked: bool,
) -> Result<()> {
    let mode = stat.st_mode;
    if mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_uid != uid
        || mode & 0o7077 != 0
        || device(stat) != root_device
        || (stat.st_nlink != 1 && !(allow_unlinked && stat.st_nlink == 0))
    {
        return Err(Error::new(ErrorKind::UnsafeFile, "file_metadata"));
    }
    Ok(())
}

fn check_tail_file(stat: &libc::stat) -> Result<()> {
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_nlink != 1 {
        return Err(Error::new(ErrorKind::UnsafeFile, "tail_file_metadata"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn check_acl(file: &StdFile) -> Result<()> {
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> *mut libc::c_void;
        fn acl_free(object: *mut libc::c_void) -> libc::c_int;
    }
    const ACL_TYPE_EXTENDED: libc::c_int = 0x100;
    // SAFETY: file owns an open descriptor and the ACL type is defined by Darwin.
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if !acl.is_null() {
        // SAFETY: acl_get_fd_np returned an allocated ACL object.
        unsafe { acl_free(acl) };
        return Err(Error::new(ErrorKind::UnsafeAcl, "extended_acl"));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(())
    } else {
        Err(Error::os(ErrorKind::UnsafeAcl, "acl_query", error))
    }
}

#[cfg(target_os = "linux")]
fn check_acl(_file: &StdFile) -> Result<()> {
    // POSIX ACL effective group permissions are bounded by the mode's mask.
    Ok(())
}

#[cfg(target_os = "macos")]
fn check_ancestor_filesystem(file: &StdFile) -> Result<()> {
    let mut stat = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: stat points to writable memory and file owns a live descriptor.
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(Error::os(
            ErrorKind::UnsafeRoot,
            "ancestor_filesystem_query",
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: successful fstatfs initialized the structure.
    let stat = unsafe { stat.assume_init() };
    if stat.f_flags & libc::MNT_IGNORE_OWNERSHIP as u32 != 0 {
        return Err(Error::new(
            ErrorKind::UnsafeRoot,
            "ancestor_ownership_disabled",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn check_ancestor_filesystem(_file: &StdFile) -> Result<()> {
    Ok(())
}

fn check_filesystem(file: &StdFile) -> Result<()> {
    let mut stat = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: stat points to writable memory and file owns a live descriptor.
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(Error::os(
            ErrorKind::UnsupportedFilesystem,
            "filesystem_query",
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: successful fstatfs initialized the structure.
    let stat = unsafe { stat.assume_init() };
    filesystem_policy(&stat)
}

#[cfg(target_os = "macos")]
fn filesystem_policy(stat: &libc::statfs) -> Result<()> {
    let name = &stat.f_fstypename;
    let apfs = name.len() >= 5
        && name[0] == b'a' as libc::c_char
        && name[1] == b'p' as libc::c_char
        && name[2] == b'f' as libc::c_char
        && name[3] == b's' as libc::c_char
        && name[4] == 0;
    if stat.f_flags & libc::MNT_LOCAL as u32 == 0 || !apfs {
        return Err(Error::new(
            ErrorKind::UnsupportedFilesystem,
            "filesystem_not_local_apfs",
        ));
    }
    if stat.f_flags & libc::MNT_IGNORE_OWNERSHIP as u32 != 0 {
        return Err(Error::new(
            ErrorKind::UnsupportedFilesystem,
            "filesystem_ownership_disabled",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn filesystem_policy(stat: &libc::statfs) -> Result<()> {
    const EXT_SUPER_MAGIC: u64 = 0xef53;
    const XFS_SUPER_MAGIC: u64 = 0x5846_5342;
    const BTRFS_SUPER_MAGIC: u64 = 0x9123_683e;
    match stat.f_type as u64 {
        EXT_SUPER_MAGIC | XFS_SUPER_MAGIC | BTRFS_SUPER_MAGIC => Ok(()),
        _ => Err(Error::new(
            ErrorKind::UnsupportedFilesystem,
            "filesystem_not_local_ext_xfs_btrfs",
        )),
    }
}

pub(super) fn sync_file(file: &StdFile) -> Result<()> {
    file.sync_all()
        .map_err(|e| Error::os(ErrorKind::Io, "file_sync", e))?;
    #[cfg(target_os = "macos")]
    {
        // SAFETY: F_FULLFSYNC takes no third argument and file is open.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } != 0 {
            return Err(Error::os(
                ErrorKind::Io,
                "file_full_sync",
                io::Error::last_os_error(),
            ));
        }
    }
    Ok(())
}

pub(super) fn try_lock(file: &StdFile) -> Result<()> {
    // SAFETY: flock operates on an owned file descriptor.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    let kind = if error.raw_os_error() == Some(libc::EWOULDBLOCK)
        || error.raw_os_error() == Some(libc::EAGAIN)
    {
        ErrorKind::Busy
    } else {
        ErrorKind::Io
    };
    Err(Error::os(kind, "file_lock", error))
}

#[cfg(target_os = "linux")]
fn rename_exclusive(dir: RawFd, from: &CStr, to: &CStr) -> libc::c_int {
    // SAFETY: the arguments satisfy renameat2 and the names live through the call.
    unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            dir,
            from.as_ptr(),
            dir,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        ) as libc::c_int
    }
}

#[cfg(target_os = "macos")]
fn rename_exclusive(dir: RawFd, from: &CStr, to: &CStr) -> libc::c_int {
    // SAFETY: both names are valid components relative to the same directory.
    unsafe { libc::renameatx_np(dir, from.as_ptr(), dir, to.as_ptr(), libc::RENAME_EXCL) }
}

#[cfg(target_os = "linux")]
fn set_errno_zero() {
    // SAFETY: errno is thread-local and the returned pointer is valid.
    unsafe { *libc::__errno_location() = 0 };
}

#[cfg(target_os = "macos")]
fn set_errno_zero() {
    // SAFETY: errno is thread-local and the returned pointer is valid.
    unsafe { *libc::__error() = 0 };
}

#[cfg(test)]
#[path = "unix_tests.rs"]
mod tests;
