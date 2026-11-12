use super::{Error, ErrorKind, Name, PublishMode, Result};
use std::ffi::{OsStr, OsString, c_void};
use std::fs::File as StdFile;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, ERROR_FILE_NOT_FOUND,
    ERROR_INSUFFICIENT_BUFFER, ERROR_LOCK_VIOLATION, ERROR_NO_TOKEN, ERROR_PATH_NOT_FOUND,
    ERROR_SHARING_VIOLATION, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, AddAccessAllowedAce, CopySid,
    CreateWellKnownSid, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetLengthSid,
    GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, IsValidAcl, IsValidSid,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
    SECURITY_DESCRIPTOR, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
    SetSecurityDescriptorOwner, TOKEN_QUERY, TOKEN_USER, TokenUser, WinBuiltinAdministratorsSid,
    WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, DeleteFileW, FILE_ALL_ACCESS,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK, FileBasicInfo,
    FileIdInfo, GetDriveTypeW, GetFileInformationByHandle, GetFileInformationByHandleEx,
    GetFileType, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_EXISTING, READ_CONTROL, ReplaceFileW, SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
};

const DRIVE_FIXED: u32 = 3;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Identity {
    volume: u64,
    file: [u8; 16],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ChangeIdentity {
    modified: i64,
    changed: i64,
}

pub(super) struct File {
    pub(super) file: StdFile,
    pub(super) id: Identity,
}

struct Directory {
    path: Vec<u16>,
    file: StdFile,
    id: Identity,
}

pub(super) struct Root {
    directories: Vec<Directory>,
    root_path: Vec<u16>,
    drive_path: Vec<u16>,
    owner: Sid,
    system: Sid,
    administrators: Sid,
}

pub(super) struct Tail {
    pub(super) file: StdFile,
    pub(super) id: Identity,
    path: Vec<u16>,
    directories: Vec<Directory>,
}

impl Tail {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let last = path.as_os_str().encode_wide().last();
        if matches!(last, Some(47 | 92)) {
            return Err(Error::new(ErrorKind::InvalidPath, "tail_name"));
        }
        let mut paths = absolute_paths(path)?;
        if paths.ancestors.len() < 2 {
            return Err(Error::new(ErrorKind::InvalidPath, "tail_name"));
        }
        let file_path = paths.ancestors.pop().expect("checked above");
        let mut directories = Vec::with_capacity(paths.ancestors.len());
        for path in paths.ancestors {
            let file = open_directory(&path, false)?;
            check_directory_info(&file_info(&file)?)?;
            let id = file_identity(&file)?;
            if directories
                .first()
                .is_some_and(|root: &Directory| root.id.volume != id.volume)
            {
                return Err(Error::new(ErrorKind::UnsafeRoot, "tail_ancestor_volume"));
            }
            directories.push(Directory { path, file, id });
        }
        let file = open_tail_handle(&file_path)?;
        check_tail_info(&file_info(&file)?)?;
        let id = file_identity(&file)?;
        let tail = Self {
            file,
            id,
            path: file_path,
            directories,
        };
        tail.validate()?;
        Ok(tail)
    }

    pub(super) fn validate(&self) -> Result<(u64, ChangeIdentity)> {
        for directory in &self.directories {
            check_directory_info(&file_info(&directory.file)?)?;
            if file_identity(&directory.file)? != directory.id {
                return Err(Error::new(ErrorKind::Changed, "tail_ancestor_identity"));
            }
            let named = open_directory(&directory.path, false)?;
            check_directory_info(&file_info(&named)?)?;
            if file_identity(&named)? != directory.id {
                return Err(Error::new(ErrorKind::Changed, "tail_ancestor_name"));
            }
        }
        let held = file_info(&self.file)?;
        check_tail_info(&held)?;
        let named = open_tail_handle(&self.path).map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                Error::new(ErrorKind::Changed, "tail_name_missing")
            } else {
                error
            }
        })?;
        check_tail_info(&file_info(&named)?)?;
        if file_identity(&self.file)? != self.id || file_identity(&named)? != self.id {
            return Err(Error::new(ErrorKind::Changed, "tail_identity"));
        }
        // SAFETY: the live file handle and correctly sized output buffer are
        // valid for the duration of this synchronous metadata query.
        let basic = unsafe {
            let mut basic: FILE_BASIC_INFO = zeroed();
            if GetFileInformationByHandleEx(
                self.file.as_raw_handle(),
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                size_of::<FILE_BASIC_INFO>() as u32,
            ) == 0
            {
                return Err(Error::os(
                    ErrorKind::Io,
                    "tail_change_metadata",
                    io::Error::last_os_error(),
                ));
            }
            basic
        };
        Ok((
            (u64::from(held.nFileSizeHigh) << 32) | u64::from(held.nFileSizeLow),
            ChangeIdentity {
                modified: basic.LastWriteTime,
                changed: basic.ChangeTime,
            },
        ))
    }
}

impl Root {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let paths = absolute_paths(path)?;
        check_fixed_drive(&paths.drive)?;
        let owner = effective_user()?;
        let system = well_known_sid(WinLocalSystemSid)?;
        let administrators = well_known_sid(WinBuiltinAdministratorsSid)?;
        let mut directories = Vec::with_capacity(paths.ancestors.len());
        for path in &paths.ancestors {
            let file = open_directory(path, false)?;
            let info = file_info(&file)?;
            check_directory_info(&info)?;
            let id = file_identity(&file)?;
            if directories
                .first()
                .is_some_and(|root: &Directory| root.id.volume != id.volume)
            {
                return Err(Error::new(ErrorKind::UnsafeRoot, "ancestor_volume"));
            }
            directories.push(Directory {
                path: path.clone(),
                file,
                id,
            });
        }
        let root = directories
            .last()
            .ok_or_else(|| Error::new(ErrorKind::InvalidPath, "absolute_root"))?;
        check_acl(&root.file, &owner, &system, &administrators)?;
        Ok(Self {
            directories,
            root_path: paths.root,
            drive_path: paths.drive,
            owner,
            system,
            administrators,
        })
    }

    pub(super) fn validate(&self) -> Result<()> {
        check_fixed_drive(&self.drive_path)?;
        if !self.owner.equals(&effective_user()?) {
            return Err(Error::new(ErrorKind::Changed, "effective_user"));
        }
        for directory in &self.directories {
            check_directory_info(&file_info(&directory.file)?)?;
            if file_identity(&directory.file)? != directory.id {
                return Err(Error::new(ErrorKind::Changed, "ancestor_identity"));
            }
            let named = open_directory(&directory.path, false)?;
            check_directory_info(&file_info(&named)?)?;
            if file_identity(&named)? != directory.id {
                return Err(Error::new(ErrorKind::Changed, "ancestor_name_identity"));
            }
        }
        let root = self
            .directories
            .last()
            .expect("root has an ancestor handle");
        check_acl(&root.file, &self.owner, &self.system, &self.administrators)
    }

    pub(super) fn open_file(&self, name: &Name, writable: bool) -> Result<Option<File>> {
        let path = self.child_path(name);
        let access = GENERIC_READ | READ_CONTROL | if writable { GENERIC_WRITE } else { 0 };
        let share =
            FILE_SHARE_READ | FILE_SHARE_WRITE | if writable { 0 } else { FILE_SHARE_DELETE };
        let file = match open_handle(&path, access, share, OPEN_EXISTING, null()) {
            Ok(file) => file,
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(x) if x == ERROR_FILE_NOT_FOUND as i32 || x == ERROR_PATH_NOT_FOUND as i32
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(Error::os(ErrorKind::Io, "file_open", error)),
        };
        self.check_file_handle(&file, false)?;
        let id = file_identity(&file)?;
        Ok(Some(File { file, id }))
    }

    pub(super) fn create_file(&self, name: &Name) -> Result<File> {
        let path = self.child_path(name);
        let mut security = PrivateSecurity::new(&self.owner)?;
        let file = match open_handle(
            &path,
            GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            CREATE_NEW,
            security.attributes(),
        ) {
            Ok(file) => file,
            Err(error) => {
                let kind = if matches!(
                    error.raw_os_error(),
                    Some(x) if x == ERROR_FILE_EXISTS as i32 || x == ERROR_ALREADY_EXISTS as i32
                ) {
                    ErrorKind::AlreadyExists
                } else {
                    ErrorKind::Io
                };
                return Err(Error::os(kind, "file_create", error));
            }
        };
        let id = file_identity(&file).map_err(Error::indeterminate)?;
        Ok(File { file, id })
    }

    pub(super) fn check_file(
        &self,
        name: Option<&Name>,
        file: &File,
        allow_unlinked: bool,
    ) -> Result<u64> {
        let info = self.check_file_handle(&file.file, allow_unlinked)?;
        if file_identity(&file.file)? != file.id {
            return Err(Error::new(ErrorKind::Changed, "file_identity"));
        }
        if let Some(name) = name {
            let named = self
                .open_file(name, false)?
                .ok_or_else(|| Error::new(ErrorKind::Changed, "file_name_missing"))?;
            if named.id != file.id {
                return Err(Error::new(ErrorKind::Changed, "file_name_identity"));
            }
        }
        Ok((u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow))
    }

    fn check_file_handle(
        &self,
        file: &StdFile,
        allow_unlinked: bool,
    ) -> Result<BY_HANDLE_FILE_INFORMATION> {
        let info = file_info(file)?;
        let root = self
            .directories
            .last()
            .expect("root has an ancestor handle");
        if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
            || (info.nNumberOfLinks != 1 && !(allow_unlinked && info.nNumberOfLinks == 0))
            || file_identity(file)?.volume != root.id.volume
        {
            return Err(Error::new(ErrorKind::UnsafeFile, "file_metadata"));
        }
        check_acl(file, &self.owner, &self.system, &self.administrators)?;
        Ok(info)
    }

    pub(super) fn sync_dir(&self) -> Result<()> {
        // Win32 does not document a user-mode directory flush that establishes
        // the needed namespace durability. Do not equate an undocumented
        // successful directory FlushFileBuffers call with that guarantee.
        Err(Error::new(
            ErrorKind::UnsupportedPlatform,
            "windows_directory_durability_unavailable",
        ))
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
        let from_path = self.child_path(from);
        let to_path = self.child_path(to);
        let delays = [10, 20, 40, 80, 160, 160, 160];
        for attempt in 0..8 {
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
            drop(source);
            drop(destination);
            // SAFETY: both paths are NUL-terminated children of pinned ancestors.
            let succeeded = unsafe {
                match mode {
                    PublishMode::Create => {
                        MoveFileExW(from_path.as_ptr(), to_path.as_ptr(), MOVEFILE_WRITE_THROUGH)
                    }
                    PublishMode::Replace => ReplaceFileW(
                        to_path.as_ptr(),
                        from_path.as_ptr(),
                        null(),
                        0,
                        null(),
                        null(),
                    ),
                }
            };
            if succeeded != 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            let code = error.raw_os_error();
            if mode == PublishMode::Replace
                && attempt < delays.len()
                && matches!(
                    code,
                    Some(x) if x == ERROR_SHARING_VIOLATION as i32 || x == ERROR_LOCK_VIOLATION as i32
                )
            {
                std::thread::sleep(Duration::from_millis(delays[attempt]));
                continue;
            }
            let kind = if matches!(
                code,
                Some(x) if x == ERROR_FILE_EXISTS as i32 || x == ERROR_ALREADY_EXISTS as i32
            ) {
                ErrorKind::AlreadyExists
            } else {
                ErrorKind::Io
            };
            return Err(Error::os(kind, "atomic_rename", error));
        }
        unreachable!("the last rename attempt returns")
    }

    pub(super) fn unlink(&self, name: &Name, expected: Identity) -> Result<()> {
        self.validate()?;
        let file = self
            .open_file(name, false)?
            .ok_or_else(|| Error::new(ErrorKind::Changed, "unlink_name_missing"))?;
        if file.id != expected {
            return Err(Error::new(ErrorKind::Changed, "unlink_name_identity"));
        }
        drop(file);
        let path = self.child_path(name);
        // SAFETY: the path names one validated component under pinned ancestors.
        if unsafe { DeleteFileW(path.as_ptr()) } == 0 {
            return Err(Error::os(
                ErrorKind::Io,
                "file_unlink",
                io::Error::last_os_error(),
            ));
        }
        Ok(())
    }

    pub(super) fn is_empty_except(&self, name: &Name) -> Result<bool> {
        let path = PathBuf::from(OsString::from_wide(without_nul(&self.root_path)));
        let expected: Vec<u16> = OsStr::new(name.as_str()).encode_wide().collect();
        for entry in
            std::fs::read_dir(path).map_err(|e| Error::os(ErrorKind::Io, "directory_read", e))?
        {
            let entry = entry.map_err(|e| Error::os(ErrorKind::Io, "directory_read", e))?;
            if entry.file_name().encode_wide().collect::<Vec<_>>() != expected {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn child_path(&self, name: &Name) -> Vec<u16> {
        let mut path = without_nul(&self.root_path).to_vec();
        if path.last() != Some(&(b'\\' as u16)) {
            path.push(b'\\' as u16);
        }
        path.extend(OsStr::new(name.as_str()).encode_wide());
        path.push(0);
        path
    }
}

struct ParsedPaths {
    drive: Vec<u16>,
    root: Vec<u16>,
    ancestors: Vec<Vec<u16>>,
}

fn absolute_paths(path: &Path) -> Result<ParsedPaths> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.len() < 3
        || !((b'A' as u16..=b'Z' as u16).contains(&wide[0])
            || (b'a' as u16..=b'z' as u16).contains(&wide[0]))
        || wide[1] != b':' as u16
        || !matches!(wide[2], 47 | 92)
        || wide.contains(&0)
    {
        return Err(Error::new(ErrorKind::InvalidPath, "absolute_drive_root"));
    }
    let drive_letter = (wide[0] as u8 as char).to_ascii_uppercase();
    let drive: Vec<u16> = format!("{drive_letter}:\\\0").encode_utf16().collect();
    let mut current: Vec<u16> = format!(r"\\?\{drive_letter}:\").encode_utf16().collect();
    let mut ancestors = vec![nul_terminated(&current)];
    let rest = &wide[3..];
    if !rest.is_empty() {
        let mut parts = rest
            .split(|unit| *unit == b'\\' as u16 || *unit == b'/' as u16)
            .peekable();
        while let Some(part) = parts.next() {
            if part.is_empty() && parts.peek().is_none() {
                break;
            }
            if part.is_empty() {
                return Err(Error::new(ErrorKind::InvalidPath, "root_component"));
            }
            let component = String::from_utf16(part)
                .map_err(|_| Error::new(ErrorKind::InvalidPath, "root_component"))?;
            Name::new(&component)
                .map_err(|_| Error::new(ErrorKind::InvalidPath, "root_component"))?;
            if current.last() != Some(&(b'\\' as u16)) {
                current.push(b'\\' as u16);
            }
            current.extend_from_slice(part);
            ancestors.push(nul_terminated(&current));
        }
    }
    if current.len() >= 32_000 {
        return Err(Error::new(ErrorKind::InvalidPath, "root_length"));
    }
    Ok(ParsedPaths {
        drive,
        root: nul_terminated(&current),
        ancestors,
    })
}

fn nul_terminated(path: &[u16]) -> Vec<u16> {
    let mut result = path.to_vec();
    result.push(0);
    result
}

fn without_nul(path: &[u16]) -> &[u16] {
    &path[..path.len() - 1]
}

fn check_fixed_drive(path: &[u16]) -> Result<()> {
    // SAFETY: path is a NUL-terminated drive root.
    if unsafe { GetDriveTypeW(path.as_ptr()) } == DRIVE_FIXED {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::UnsupportedFilesystem,
            "drive_not_fixed",
        ))
    }
}

fn open_directory(path: &[u16], writable: bool) -> Result<StdFile> {
    let access = FILE_READ_ATTRIBUTES
        | READ_CONTROL
        | SYNCHRONIZE
        | if writable { GENERIC_WRITE } else { 0 };
    open_handle(
        path,
        access,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        OPEN_EXISTING,
        null(),
    )
    .map_err(|e| Error::os(ErrorKind::UnsafeRoot, "ancestor_open", e))
}

fn open_handle(
    path: &[u16],
    access: u32,
    share: u32,
    creation: u32,
    security: *const SECURITY_ATTRIBUTES,
) -> io::Result<StdFile> {
    // SAFETY: path is NUL terminated; security, when non-null, is valid for this call.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            share,
            security,
            creation,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: CreateFileW returned a newly owned handle.
        Ok(unsafe { StdFile::from_raw_handle(handle as _) })
    }
}

fn handle(file: &StdFile) -> HANDLE {
    file.as_raw_handle() as HANDLE
}

fn file_info(file: &StdFile) -> Result<BY_HANDLE_FILE_INFORMATION> {
    // SAFETY: the handle is live and points to an open object.
    if unsafe { GetFileType(handle(file)) } != FILE_TYPE_DISK {
        return Err(Error::new(ErrorKind::UnsafeFile, "file_type"));
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: info points to writable memory and the handle is valid.
    if unsafe { GetFileInformationByHandle(handle(file), &mut info) } == 0 {
        return Err(Error::os(
            ErrorKind::Io,
            "file_info",
            io::Error::last_os_error(),
        ));
    }
    Ok(info)
}

fn file_identity(file: &StdFile) -> Result<Identity> {
    let mut info = FILE_ID_INFO::default();
    // SAFETY: info has the exact layout and size expected for FileIdInfo.
    if unsafe {
        GetFileInformationByHandleEx(
            handle(file),
            FileIdInfo,
            &mut info as *mut _ as *mut c_void,
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(Error::os(
            ErrorKind::UnsupportedFilesystem,
            "file_id_query",
            io::Error::last_os_error(),
        ));
    }
    if info.FileId.Identifier == [0; 16] {
        return Err(Error::new(
            ErrorKind::UnsupportedFilesystem,
            "file_id_unavailable",
        ));
    }
    Ok(Identity {
        volume: info.VolumeSerialNumber,
        file: info.FileId.Identifier,
    })
}

fn check_directory_info(info: &BY_HANDLE_FILE_INFORMATION) -> Result<()> {
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(Error::new(
            ErrorKind::UnsafeRoot,
            "ancestor_reparse_or_type",
        ));
    }
    Ok(())
}

fn check_tail_info(info: &BY_HANDLE_FILE_INFORMATION) -> Result<()> {
    if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || info.nNumberOfLinks != 1
    {
        return Err(Error::new(ErrorKind::UnsafeFile, "tail_file_metadata"));
    }
    Ok(())
}

fn open_tail_handle(path: &[u16]) -> Result<StdFile> {
    open_handle(
        path,
        GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        OPEN_EXISTING,
        null(),
    )
    .map_err(|error| {
        let kind = if matches!(
            error.raw_os_error(),
            Some(x) if x == ERROR_FILE_NOT_FOUND as i32 || x == ERROR_PATH_NOT_FOUND as i32
        ) {
            ErrorKind::NotFound
        } else {
            ErrorKind::Io
        };
        Error::os(kind, "tail_open", error)
    })
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: this wrapper uniquely owns the returned token handle.
        unsafe { CloseHandle(self.0) };
    }
}

#[derive(Clone)]
struct Sid {
    words: Vec<u32>,
}

impl Sid {
    fn as_ptr(&self) -> PSID {
        self.words.as_ptr() as PSID
    }

    fn equals(&self, other: &Sid) -> bool {
        // SAFETY: both SIDs are validated and remain alive for the call.
        unsafe { EqualSid(self.as_ptr(), other.as_ptr()) != 0 }
    }
}

fn effective_user() -> Result<Sid> {
    let mut thread_token = null_mut();
    // SAFETY: the pseudo handle and output pointer are valid.
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut thread_token) } != 0 {
        let _owned = OwnedHandle(thread_token);
        return Err(Error::new(
            ErrorKind::UnsupportedPlatform,
            "thread_impersonation",
        ));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
        return Err(Error::os(ErrorKind::UnsafeAcl, "thread_token", error));
    }
    let mut token = null_mut();
    // SAFETY: the pseudo process handle and output pointer are valid.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(Error::os(
            ErrorKind::UnsafeAcl,
            "process_token",
            io::Error::last_os_error(),
        ));
    }
    let token = OwnedHandle(token);
    let mut size = 0;
    // SAFETY: a null buffer with zero size queries the required allocation.
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut size) };
    let query_error = io::Error::last_os_error();
    if query_error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        || size < size_of::<TOKEN_USER>() as u32
    {
        return Err(Error::os(
            ErrorKind::UnsafeAcl,
            "token_user_size",
            query_error,
        ));
    }
    let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
    // SAFETY: buffer is aligned and at least size bytes long.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr() as *mut c_void,
            size,
            &mut size,
        )
    } == 0
    {
        return Err(Error::os(
            ErrorKind::UnsafeAcl,
            "token_user",
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: GetTokenInformation initialized a TOKEN_USER at the buffer start.
    let source = unsafe { (*(buffer.as_ptr() as *const TOKEN_USER)).User.Sid };
    // SAFETY: the SID is supplied by the operating system.
    if source.is_null() || unsafe { IsValidSid(source) } == 0 {
        return Err(Error::new(ErrorKind::UnsafeAcl, "token_sid"));
    }
    // SAFETY: the SID was validated.
    let sid_len = unsafe { GetLengthSid(source) } as usize;
    let mut words = vec![0u32; sid_len.div_ceil(size_of::<u32>())];
    // SAFETY: the destination is aligned and has at least sid_len bytes.
    if unsafe { CopySid(sid_len as u32, words.as_mut_ptr() as PSID, source) } == 0 {
        return Err(Error::os(
            ErrorKind::UnsafeAcl,
            "token_sid_copy",
            io::Error::last_os_error(),
        ));
    }
    Ok(Sid { words })
}

fn well_known_sid(kind: i32) -> Result<Sid> {
    let mut words = vec![0u32; 17];
    let mut size = (words.len() * size_of::<u32>()) as u32;
    // SAFETY: the buffer is aligned and large enough for a maximum-sized SID.
    if unsafe { CreateWellKnownSid(kind, null_mut(), words.as_mut_ptr() as PSID, &mut size) } == 0 {
        return Err(Error::os(
            ErrorKind::UnsafeAcl,
            "well_known_sid",
            io::Error::last_os_error(),
        ));
    }
    Ok(Sid { words })
}

struct LocalMemory(PSECURITY_DESCRIPTOR);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        // SAFETY: GetSecurityInfo allocated the descriptor with LocalAlloc.
        unsafe { LocalFree(self.0 as _) };
    }
}

fn check_acl(file: &StdFile, owner: &Sid, system: &Sid, administrators: &Sid) -> Result<()> {
    let mut file_owner: PSID = null_mut();
    let mut acl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: all output pointers are writable and the handle has READ_CONTROL.
    let result = unsafe {
        GetSecurityInfo(
            handle(file),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut file_owner,
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(Error::os(
            ErrorKind::UnsafeAcl,
            "security_query",
            io::Error::from_raw_os_error(result as i32),
        ));
    }
    let _memory = LocalMemory(descriptor);
    // SAFETY: pointers are owned by the returned security descriptor.
    if file_owner.is_null()
        || acl.is_null()
        || unsafe { IsValidSid(file_owner) } == 0
        || unsafe { EqualSid(file_owner, owner.as_ptr()) } == 0
        || unsafe { IsValidAcl(acl) } == 0
    {
        return Err(Error::new(ErrorKind::UnsafeAcl, "security_owner_or_dacl"));
    }
    // SAFETY: IsValidAcl confirmed the ACL header and ACE count are valid.
    let count = unsafe { (*acl).AceCount };
    for index in 0..u32::from(count) {
        let mut ace: *mut c_void = null_mut();
        // SAFETY: index is bounded by the validated ACL's ACE count.
        if unsafe { GetAce(acl, index, &mut ace) } == 0 || ace.is_null() {
            return Err(Error::new(ErrorKind::UnsafeAcl, "security_ace"));
        }
        // SAFETY: GetAce returned a pointer to a valid ACE header.
        let header = unsafe { &*(ace as *const ACE_HEADER) };
        match header.AceType {
            ACCESS_DENIED_ACE_TYPE => continue,
            ACCESS_ALLOWED_ACE_TYPE => {}
            _ => return Err(Error::new(ErrorKind::UnsafeAcl, "security_ace_type")),
        }
        let sid_offset = size_of::<ACE_HEADER>() + size_of::<u32>();
        if usize::from(header.AceSize) < sid_offset + 8 {
            return Err(Error::new(ErrorKind::UnsafeAcl, "security_ace_size"));
        }
        // SAFETY: the size check includes the fixed SID header.
        let sid = unsafe { (ace as *mut u8).add(sid_offset) };
        // SAFETY: the second byte is the SID subauthority count.
        let sid_size = 8 + usize::from(unsafe { *sid.add(1) }) * 4;
        if sid_offset + sid_size > usize::from(header.AceSize) {
            return Err(Error::new(ErrorKind::UnsafeAcl, "security_sid_size"));
        }
        let sid = sid as PSID;
        // SAFETY: the complete SID fits inside the validated ACE.
        if unsafe { IsValidSid(sid) } == 0
            || (unsafe { EqualSid(sid, owner.as_ptr()) } == 0
                && unsafe { EqualSid(sid, system.as_ptr()) } == 0
                && unsafe { EqualSid(sid, administrators.as_ptr()) } == 0)
        {
            return Err(Error::new(ErrorKind::UnsafeAcl, "security_grants_other"));
        }
    }
    Ok(())
}

struct PrivateSecurity<'a> {
    // The descriptor refers to the other allocations until CreateFileW returns.
    _owner: &'a Sid,
    acl: Vec<u32>,
    descriptor: SECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl<'a> PrivateSecurity<'a> {
    fn new(owner: &'a Sid) -> Result<Self> {
        // SAFETY: the SID was validated when it was copied.
        let sid_len = unsafe { GetLengthSid(owner.as_ptr()) } as usize;
        let size = size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid_len;
        let mut acl = vec![0u32; size.div_ceil(size_of::<u32>())];
        let acl_ptr = acl.as_mut_ptr() as *mut ACL;
        // SAFETY: ACL storage is aligned, large enough, and lives through creation.
        if unsafe { InitializeAcl(acl_ptr, size as u32, ACL_REVISION) } == 0
            || unsafe {
                AddAccessAllowedAce(acl_ptr, ACL_REVISION, FILE_ALL_ACCESS, owner.as_ptr())
            } == 0
        {
            return Err(Error::os(
                ErrorKind::UnsafeAcl,
                "create_acl",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: a zeroed SECURITY_DESCRIPTOR is initialized before use.
        let mut descriptor: SECURITY_DESCRIPTOR = unsafe { zeroed() };
        let pointer = &mut descriptor as *mut _ as PSECURITY_DESCRIPTOR;
        // SAFETY: the descriptor, SID, and ACL remain valid during these calls.
        if unsafe { InitializeSecurityDescriptor(pointer, SECURITY_DESCRIPTOR_REVISION) } == 0
            || unsafe { SetSecurityDescriptorOwner(pointer, owner.as_ptr(), 0) } == 0
            || unsafe { SetSecurityDescriptorDacl(pointer, 1, acl_ptr, 0) } == 0
            || unsafe {
                SetSecurityDescriptorControl(pointer, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
            } == 0
        {
            return Err(Error::os(
                ErrorKind::UnsafeAcl,
                "create_security_descriptor",
                io::Error::last_os_error(),
            ));
        }
        Ok(Self {
            _owner: owner,
            acl,
            descriptor,
            attributes: SECURITY_ATTRIBUTES::default(),
        })
    }

    fn attributes(&mut self) -> *const SECURITY_ATTRIBUTES {
        self.attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: &mut self.descriptor as *mut _ as *mut c_void,
            bInheritHandle: 0,
        };
        // Keep the ACL allocation alive while Windows reads the descriptor.
        debug_assert!(!self.acl.is_empty());
        &self.attributes
    }
}

pub(super) fn sync_file(file: &StdFile) -> Result<()> {
    file.sync_all()
        .map_err(|e| Error::os(ErrorKind::Io, "file_sync", e))
}

pub(super) fn try_lock(file: &StdFile) -> Result<()> {
    let mut overlapped = OVERLAPPED::default();
    // SAFETY: the handle is synchronous and overlapped remains alive until return.
    if unsafe {
        LockFileEx(
            handle(file),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    } != 0
    {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    let kind = if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        ErrorKind::Busy
    } else {
        ErrorKind::Io
    };
    Err(Error::os(kind, "file_lock", error))
}

#[cfg(test)]
pub(super) fn create_test_private_directory(path: &Path) -> Result<()> {
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

    let owner = effective_user()?;
    let mut security = PrivateSecurity::new(&owner)?;
    let mut path: Vec<u16> = path.as_os_str().encode_wide().collect();
    path.push(0);
    // SAFETY: this test-only call creates a single explicitly named fixture.
    if unsafe { CreateDirectoryW(path.as_ptr(), security.attributes()) } == 0 {
        return Err(Error::os(
            ErrorKind::Io,
            "fixture_directory_create",
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
