use std::{
    ffi::CString,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
};

#[cfg(target_os = "macos")]
use std::ffi::{c_int, c_void};

use super::{
    FileIdentity, SourceSnapshot, TailBoundary, TailError, TailFile, validate_open_and_named,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeBoundary;

pub struct NativeFile {
    file: File,
    path: PathBuf,
    directories: Vec<File>,
}

impl Read for NativeFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buffer)
    }
}

impl Seek for NativeFile {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(position)
    }
}

impl TailBoundary for NativeBoundary {
    type File = NativeFile;

    fn open(&self, path: &Path) -> Result<Self::File, TailError> {
        let (directories, file) = walk(path)?;
        Ok(NativeFile {
            file,
            path: path.to_owned(),
            directories,
        })
    }
}

impl TailFile for NativeFile {
    fn recheck(&mut self) -> Result<SourceSnapshot, TailError> {
        let (fresh_directories, fresh_file) =
            walk(&self.path).map_err(|_| TailError::SourceChanged)?;
        if self.directories.len() != fresh_directories.len() {
            return Err(TailError::SourceChanged);
        }
        for (old, fresh) in self.directories.iter().zip(&fresh_directories) {
            validate_directory(old).map_err(|_| TailError::SourceChanged)?;
            let old = old.metadata().map_err(|_| TailError::SourceChanged)?;
            let fresh = fresh.metadata().map_err(|_| TailError::SourceChanged)?;
            if identity(&old) != identity(&fresh) {
                return Err(TailError::SourceChanged);
            }
        }
        let opened = file_snapshot(&self.file).map_err(|_| TailError::SourceChanged)?;
        let named = file_snapshot(&fresh_file).map_err(|_| TailError::SourceChanged)?;
        validate_open_and_named(opened, named)
    }
}

fn components(path: &Path) -> Result<Vec<CString>, TailError> {
    let raw = path.as_os_str().as_bytes();
    if raw.first() != Some(&b'/') || raw.len() < 2 {
        return Err(TailError::InvalidPath);
    }
    let mut result = Vec::new();
    for component in raw[1..].split(|byte| *byte == b'/') {
        if component.is_empty() || component == b"." || component == b".." {
            return Err(TailError::InvalidPath);
        }
        result.push(CString::new(component).map_err(|_| TailError::InvalidPath)?);
    }
    Ok(result)
}

fn walk(path: &Path) -> Result<(Vec<File>, File), TailError> {
    let components = components(path)?;
    let root = CString::new("/").map_err(|_| TailError::InvalidPath)?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let root_fd = unsafe { libc::open(root.as_ptr(), flags) };
    if root_fd < 0 {
        return Err(classify_open_error());
    }
    let root_file = unsafe { File::from_raw_fd(root_fd) };
    validate_directory(&root_file)?;
    let mut directories = vec![root_file];
    for component in &components[..components.len() - 1] {
        let parent = directories.last().ok_or(TailError::UnsafePath)?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                component.as_ptr(),
                flags | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(classify_open_error());
        }
        let directory = unsafe { File::from_raw_fd(fd) };
        validate_directory(&directory)?;
        directories.push(directory);
    }
    let parent = directories.last().ok_or(TailError::UnsafePath)?;
    let name = components.last().ok_or(TailError::InvalidPath)?;
    let before = named_regular(parent, name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(classify_open_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let opened = file_snapshot(&file)?;
    let after = named_regular(parent, name)?;
    if before != opened.identity || after != opened.identity {
        return Err(TailError::SourceChanged);
    }
    Ok((directories, file))
}

fn named_regular(parent: &File, name: &CString) -> Result<FileIdentity, TailError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        return Err(classify_open_error());
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_nlink != 1 {
        return Err(TailError::UnsafePath);
    }
    // Darwin's dev_t is signed; Linux uses an unsigned dev_t.
    #[allow(clippy::unnecessary_cast)]
    let volume = stat.st_dev as u64;
    Ok(FileIdentity {
        volume,
        file: stat.st_ino,
    })
}

fn classify_open_error() -> TailError {
    match std::io::Error::last_os_error().raw_os_error() {
        Some(code)
            if code == libc::ELOOP
                || code == libc::ENOTDIR
                || code == libc::EISDIR
                || code == libc::ENXIO =>
        {
            TailError::UnsafePath
        }
        _ => TailError::Io,
    }
}

fn validate_directory(directory: &File) -> Result<(), TailError> {
    let metadata = directory.metadata().map_err(|_| TailError::Io)?;
    if !metadata.is_dir() {
        return Err(TailError::UnsafePath);
    }
    let mode = metadata.mode();
    if mode & 0o022 != 0 && !(metadata.uid() == 0 && mode & 0o1000 != 0) {
        return Err(TailError::UnsafePath);
    }
    reject_extended_acl(directory)?;
    Ok(())
}

fn file_snapshot(file: &File) -> Result<SourceSnapshot, TailError> {
    let metadata = file.metadata().map_err(|_| TailError::Io)?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(TailError::UnsafePath);
    }
    reject_extended_acl(file)?;
    Ok(SourceSnapshot {
        identity: identity(&metadata),
        length: metadata.len(),
        modified: i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()),
        changed: i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec()),
    })
}

fn identity(metadata: &Metadata) -> FileIdentity {
    FileIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
    }
}

#[cfg(target_os = "macos")]
fn reject_extended_acl(file: &File) -> Result<(), TailError> {
    const ACL_TYPE_EXTENDED: c_int = 0x100;
    const ACL_FIRST_ENTRY: c_int = 0;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, acl_type: c_int) -> *mut c_void;
        fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_free(object: *mut c_void) -> c_int;
    }
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        return if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            Ok(())
        } else {
            Err(TailError::UnsafePath)
        };
    }
    let mut entry = std::ptr::null_mut();
    let result = unsafe { acl_get_entry(acl, ACL_FIRST_ENTRY, &mut entry) };
    let error = std::io::Error::last_os_error().raw_os_error();
    let freed = unsafe { acl_free(acl) };
    if acl_entry_is_absent(result, error) && freed == 0 {
        Ok(())
    } else {
        Err(TailError::UnsafePath)
    }
}

#[cfg(target_os = "macos")]
fn acl_entry_is_absent(result: c_int, error: Option<c_int>) -> bool {
    // Darwin returns -1/EINVAL for ACL_FIRST_ENTRY on an empty valid ACL.
    result == -1 && error == Some(libc::EINVAL)
}

#[cfg(all(test, target_os = "macos"))]
mod acl_tests {
    use super::*;
    use std::{fs::OpenOptions, process::Command};

    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn file_with_extended_acl_is_rejected() {
        let path = std::env::current_dir()
            .unwrap()
            .join(format!("acl-test-{}", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let fixture = Fixture(path);
        assert_eq!(reject_extended_acl(&file), Ok(()));
        let user = Command::new("/usr/bin/id").arg("-un").output().unwrap();
        assert!(user.status.success());
        let user = std::str::from_utf8(&user.stdout).unwrap().trim();
        assert!(
            Command::new("/bin/chmod")
                .arg("+a")
                .arg(format!("user:{user} allow read"))
                .arg(&fixture.0)
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(reject_extended_acl(&file), Err(TailError::UnsafePath));
    }

    #[test]
    fn empty_acl_is_allowed_but_entries_and_other_errors_are_not() {
        unsafe extern "C" {
            fn acl_init(count: c_int) -> *mut c_void;
            fn acl_create_entry(acl: *mut *mut c_void, entry: *mut *mut c_void) -> c_int;
            fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
            fn acl_free(object: *mut c_void) -> c_int;
        }
        let mut acl = unsafe { acl_init(1) };
        assert!(!acl.is_null());
        let mut entry = std::ptr::null_mut();
        let result = unsafe { acl_get_entry(acl, 0, &mut entry) };
        let error = std::io::Error::last_os_error().raw_os_error();
        assert!(acl_entry_is_absent(result, error));
        assert_eq!(unsafe { acl_create_entry(&mut acl, &mut entry) }, 0);
        let result = unsafe { acl_get_entry(acl, 0, &mut entry) };
        assert!(!acl_entry_is_absent(result, None));
        assert!(!acl_entry_is_absent(-1, Some(libc::EIO)));
        assert_eq!(unsafe { acl_free(acl) }, 0);
    }
}

#[cfg(target_os = "linux")]
fn reject_extended_acl(_: &File) -> Result<(), TailError> {
    Ok(())
}
