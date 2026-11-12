#[cfg(test)]
use std::fs::File;
use std::fs::{Metadata, OpenOptions, symlink_metadata};
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::FirmwareError;
#[cfg(test)]
use super::backend::MaintenanceBackend;

pub(crate) const IMAGE_SIZE: u64 = 2_214_944;
pub(crate) const TOOL_SIZE: u64 = 13_920_608;
#[cfg(test)]
pub(crate) const FLASH_SIZE: u64 = 0x1000000;
#[cfg(test)]
pub(crate) const PARTITION_START: usize = 0x8000;
pub(crate) const PARTITION_END: usize = 0x9000;
pub(crate) const SETTINGS_START: usize = 0x810000;
#[cfg(test)]
pub(crate) const SETTINGS_SIZE: usize = 0x220000;
#[cfg(test)]
pub(crate) const SETTINGS_END: usize = SETTINGS_START + SETTINGS_SIZE;

#[derive(Clone, Copy)]
pub(crate) struct ArtifactPin<'a> {
    pub(crate) size: u64,
    pub(crate) sha256: &'a str,
}

pub(crate) const IMAGE_PIN: ArtifactPin<'static> = ArtifactPin {
    size: IMAGE_SIZE,
    sha256: "1d8bd545b9047182d7112dd86b14f6c636bad3834725ee0a6eca7cacee0ec6a7",
};
pub(crate) const TOOL_PIN: ArtifactPin<'static> = ArtifactPin {
    size: TOOL_SIZE,
    sha256: "fe300417054386e8c15f916216924accb05d0132411bfce3b223ad621f0de6d9",
};

pub(crate) fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(unix)]
fn same_file(left: &Metadata, right: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file(left: &Metadata, right: &Metadata) -> bool {
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

fn regular_size(metadata: &Metadata, expected: u64) -> bool {
    metadata.file_type().is_file() && metadata.len() == expected
}

/// Read no more than `expected + 1`; check both the path and the opened file.
pub(crate) fn read_exact_regular(path: &Path, expected: u64) -> Result<Vec<u8>, FirmwareError> {
    let before = symlink_metadata(path).map_err(|error| {
        FirmwareError::new(format!("cannot inspect {}: {error}", path.display()))
    })?;
    if !regular_size(&before, expected) {
        return Err(FirmwareError::new(format!(
            "{} is not a regular file of the required size",
            path.display()
        )));
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options
        .open(path)
        .map_err(|error| FirmwareError::new(format!("cannot open {}: {error}", path.display())))?;
    let opened = file
        .metadata()
        .map_err(|error| FirmwareError::new(format!("cannot inspect opened file: {error}")))?;
    if !regular_size(&opened, expected) || !same_file(&before, &opened) {
        return Err(FirmwareError::new("file changed while opening"));
    }
    let limit = expected
        .checked_add(1)
        .ok_or_else(|| FirmwareError::new("file size limit overflow"))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| FirmwareError::new(format!("cannot read {}: {error}", path.display())))?;
    let after = file
        .metadata()
        .map_err(|error| FirmwareError::new(format!("cannot recheck opened file: {error}")))?;
    let path_after = symlink_metadata(path)
        .map_err(|error| FirmwareError::new(format!("cannot recheck file path: {error}")))?;
    if u64::try_from(bytes.len()).ok() != Some(expected)
        || !regular_size(&after, expected)
        || !regular_size(&path_after, expected)
        || !same_file(&opened, &after)
        || !same_file(&opened, &path_after)
    {
        return Err(FirmwareError::new("file changed while reading"));
    }
    Ok(bytes)
}

pub(crate) fn verify_artifact(path: &Path, pin: ArtifactPin<'_>) -> Result<Vec<u8>, FirmwareError> {
    let bytes = read_exact_regular(path, pin.size)?;
    if sha256(&bytes) != pin.sha256 {
        return Err(FirmwareError::new(format!(
            "{} does not match the pinned SHA-256",
            path.display()
        )));
    }
    Ok(bytes)
}

pub(crate) fn verify_image_extent(image: &[u8]) -> Result<(), FirmwareError> {
    if image.first() != Some(&0xe9) {
        return Err(FirmwareError::new("image has an invalid first byte"));
    }
    if image.len() < PARTITION_END {
        return Err(FirmwareError::new(
            "image does not contain the partition sector",
        ));
    }
    let rounded = image
        .len()
        .checked_add(4095)
        .map(|value| value & !4095)
        .ok_or_else(|| FirmwareError::new("image extent overflow"))?;
    if rounded > SETTINGS_START {
        return Err(FirmwareError::new(
            "sector-rounded image overlaps the preserved settings span",
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn sync_regular<B: MaintenanceBackend>(
    backend: &mut B,
    path: &Path,
    expected: u64,
) -> Result<(), FirmwareError> {
    // The full bounded read also rejects a replaced path before syncing.
    let _ = read_exact_regular(path, expected)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file: File = options
        .open(path)
        .map_err(|error| FirmwareError::new(format!("cannot open file to sync: {error}")))?;
    let opened = file
        .metadata()
        .map_err(|error| FirmwareError::new(format!("cannot inspect file to sync: {error}")))?;
    let current = symlink_metadata(path)
        .map_err(|error| FirmwareError::new(format!("cannot recheck file to sync: {error}")))?;
    if !regular_size(&opened, expected)
        || !regular_size(&current, expected)
        || !same_file(&opened, &current)
    {
        return Err(FirmwareError::new("file changed before sync"));
    }
    backend
        .full_sync(&file)
        .map_err(|error| FirmwareError::new(format!("cannot fully sync file: {error}")))?;
    let after = symlink_metadata(path)
        .map_err(|error| FirmwareError::new(format!("cannot recheck synced file: {error}")))?;
    if !regular_size(&after, expected) || !same_file(&opened, &after) {
        return Err(FirmwareError::new("file changed during sync"));
    }
    Ok(())
}
