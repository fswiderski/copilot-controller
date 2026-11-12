use std::{
    ffi::OsStr,
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
    ptr,
};

use windows_sys::Win32::{
    Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FILE_TYPE_DISK, FileBasicInfo, GetDriveTypeW, GetFileInformationByHandle,
        GetFileInformationByHandleEx, GetFileType, OPEN_EXISTING,
    },
};

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
            let old = information(old, true).map_err(|_| TailError::SourceChanged)?;
            let fresh = information(fresh, true).map_err(|_| TailError::SourceChanged)?;
            if old.identity != fresh.identity {
                return Err(TailError::SourceChanged);
            }
        }
        let opened = information(&self.file, false).map_err(|_| TailError::SourceChanged)?;
        let named = information(&fresh_file, false).map_err(|_| TailError::SourceChanged)?;
        validate_open_and_named(opened, named)
    }
}

fn walk(path: &Path) -> Result<(Vec<File>, File), TailError> {
    let mut parts = path.components();
    let Some(Component::Prefix(prefix)) = parts.next() else {
        return Err(TailError::InvalidPath);
    };
    if !matches!(prefix.kind(), Prefix::Disk(_)) {
        return Err(TailError::InvalidPath);
    }
    if !matches!(parts.next(), Some(Component::RootDir)) {
        return Err(TailError::InvalidPath);
    }
    let mut names = Vec::new();
    for part in parts {
        let Component::Normal(name) = part else {
            return Err(TailError::InvalidPath);
        };
        if invalid_component(name) {
            return Err(TailError::InvalidPath);
        }
        names.push(name);
    }
    if names.is_empty() {
        return Err(TailError::InvalidPath);
    }

    let mut current = PathBuf::from(prefix.as_os_str());
    current.push(r"\");
    let mut root: Vec<u16> = current.as_os_str().encode_wide().collect();
    root.push(0);
    const DRIVE_FIXED: u32 = 3;
    if unsafe { GetDriveTypeW(root.as_ptr()) } != DRIVE_FIXED {
        return Err(TailError::UnsafePath);
    }
    let root = open_component(&current, true)?;
    let mut directories = vec![root];
    for name in &names[..names.len() - 1] {
        current.push(name);
        directories.push(open_component(&current, true)?);
    }
    current.push(names[names.len() - 1]);
    let file = open_component(&current, false)?;
    Ok((directories, file))
}

fn invalid_component(name: &OsStr) -> bool {
    let wide: Vec<u16> = name.encode_wide().collect();
    wide.is_empty()
        || wide.iter().any(|character| matches!(*character, 0 | 0x3a))
        || matches!(wide.last(), Some(0x20 | 0x2e))
        || reserved_name(name)
}

fn reserved_name(name: &OsStr) -> bool {
    let text = name.to_string_lossy();
    let stem = text
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let port = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"));
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) || port.is_some_and(|number| {
        matches!(
            number,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
    })
}

fn open_component(path: &Path, directory: bool) -> Result<File, TailError> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(TailError::InvalidPath);
    }
    wide.push(0);
    let access = if directory {
        FILE_READ_ATTRIBUTES
    } else {
        GENERIC_READ
    };
    let sharing =
        FILE_SHARE_READ | FILE_SHARE_WRITE | if directory { 0 } else { FILE_SHARE_DELETE };
    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | if directory {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            0
        };
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            sharing,
            ptr::null(),
            OPEN_EXISTING,
            flags,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(TailError::Io);
    }
    let file = unsafe { File::from_raw_handle(handle.cast()) };
    information(&file, directory)?;
    Ok(file)
}

fn information(file: &File, directory: bool) -> Result<SourceSnapshot, TailError> {
    let handle = file.as_raw_handle().cast();
    if unsafe { GetFileType(handle) } != FILE_TYPE_DISK {
        return Err(TailError::UnsafePath);
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(TailError::Io);
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
        || !directory && info.nNumberOfLinks != 1
    {
        return Err(TailError::UnsafePath);
    }
    let mut basic = FILE_BASIC_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(TailError::Io);
    }
    Ok(SourceSnapshot {
        identity: FileIdentity {
            volume: u64::from(info.dwVolumeSerialNumber),
            file: u64::from(info.nFileIndexHigh) << 32 | u64::from(info.nFileIndexLow),
        },
        length: u64::from(info.nFileSizeHigh) << 32 | u64::from(info.nFileSizeLow),
        modified: i128::from(basic.LastWriteTime),
        changed: i128::from(basic.ChangeTime),
    })
}
