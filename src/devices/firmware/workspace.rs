use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::FirmwareError;
use super::artifacts::{ArtifactPin, read_exact_regular, sha256, sync_regular, verify_artifact};
use super::backend::MaintenanceBackend;

#[cfg(unix)]
fn same_file(left: &Metadata, right: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file(left: &Metadata, right: &Metadata) -> bool {
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

#[cfg(unix)]
fn exact_mode(metadata: &Metadata, mode: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.mode() & 0o7777 == mode
}

#[cfg(not(unix))]
fn exact_mode(_metadata: &Metadata, _mode: u32) -> bool {
    // The native worker is unavailable outside macOS; its binding must enforce ACLs.
    true
}

#[cfg(unix)]
fn set_mode(file: &File, mode: u32) -> Result<(), FirmwareError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|error| FirmwareError::new(format!("cannot set private file mode: {error}")))
}

#[cfg(not(unix))]
fn set_mode(_file: &File, _mode: u32) -> Result<(), FirmwareError> {
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), FirmwareError> {
    let builder = DirBuilder::new();
    #[cfg(unix)]
    let mut builder = builder;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|error| {
        FirmwareError::new(format!(
            "cannot create new private directory {}: {error}",
            path.display()
        ))
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        FirmwareError::new(format!("cannot inspect private directory: {error}"))
    })?;
    if !metadata.file_type().is_dir() || !exact_mode(&metadata, 0o700) {
        return Err(FirmwareError::new("workspace directory is not private"));
    }
    Ok(())
}

pub(crate) struct Workspace {
    root: PathBuf,
    #[cfg(unix)]
    directory: File,
    #[cfg(unix)]
    parent: File,
    #[cfg(unix)]
    tool_tmp: Option<File>,
}

impl Workspace {
    pub(crate) fn prepare(path: &Path, image: &[u8], tool: &[u8]) -> Result<Self, FirmwareError> {
        create_private_directory(path)?;
        let root = fs::canonicalize(path).map_err(|error| {
            FirmwareError::new(format!("cannot canonicalize workspace: {error}"))
        })?;
        let path_metadata = fs::symlink_metadata(path)
            .map_err(|error| FirmwareError::new(format!("cannot recheck workspace: {error}")))?;
        let root_metadata = fs::symlink_metadata(&root).map_err(|error| {
            FirmwareError::new(format!("cannot inspect canonical workspace: {error}"))
        })?;
        if !path_metadata.file_type().is_dir()
            || !root_metadata.file_type().is_dir()
            || !same_file(&path_metadata, &root_metadata)
            || !exact_mode(&root_metadata, 0o700)
        {
            return Err(FirmwareError::new("workspace changed during preparation"));
        }
        #[cfg(unix)]
        let directory = File::open(&root)
            .map_err(|error| FirmwareError::new(format!("cannot open workspace: {error}")))?;
        #[cfg(unix)]
        let parent = File::open(
            root.parent()
                .ok_or_else(|| FirmwareError::new("workspace has no parent"))?,
        )
        .map_err(|error| FirmwareError::new(format!("cannot open workspace parent: {error}")))?;
        let workspace = Self {
            root,
            #[cfg(unix)]
            directory,
            #[cfg(unix)]
            parent,
            #[cfg(unix)]
            tool_tmp: None,
        };
        workspace.check()?;
        workspace.create_file("firmware.bin", image, 0o600)?;
        workspace.create_file("esptool", tool, 0o700)?;
        create_private_directory(&workspace.path("tool-tmp"))?;
        #[cfg(unix)]
        let mut workspace = workspace;
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut options = OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC);
            workspace.tool_tmp =
                Some(options.open(workspace.path("tool-tmp")).map_err(|error| {
                    FirmwareError::new(format!("cannot open private tool directory: {error}"))
                })?);
        }
        workspace.check()?;
        workspace.check_tool_tmp()?;
        Ok(workspace)
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub(crate) fn check(&self) -> Result<(), FirmwareError> {
        let current = fs::symlink_metadata(&self.root)
            .map_err(|error| FirmwareError::new(format!("cannot recheck workspace: {error}")))?;
        if !current.file_type().is_dir() || !exact_mode(&current, 0o700) {
            return Err(FirmwareError::new("workspace is no longer private"));
        }
        #[cfg(unix)]
        {
            let opened = self.directory.metadata().map_err(|error| {
                FirmwareError::new(format!("cannot inspect workspace handle: {error}"))
            })?;
            if !same_file(&current, &opened) {
                return Err(FirmwareError::new("workspace directory was substituted"));
            }
            let parent_path = self
                .root
                .parent()
                .ok_or_else(|| FirmwareError::new("workspace has no parent"))?;
            let parent_path_metadata = fs::symlink_metadata(parent_path).map_err(|error| {
                FirmwareError::new(format!("cannot inspect workspace parent: {error}"))
            })?;
            let parent_opened = self.parent.metadata().map_err(|error| {
                FirmwareError::new(format!("cannot inspect workspace parent handle: {error}"))
            })?;
            if !parent_path_metadata.file_type().is_dir()
                || !same_file(&parent_path_metadata, &parent_opened)
            {
                return Err(FirmwareError::new("workspace parent was substituted"));
            }
        }
        Ok(())
    }

    pub(crate) fn check_tool_tmp(&self) -> Result<(), FirmwareError> {
        self.check()?;
        let current = fs::symlink_metadata(self.path("tool-tmp")).map_err(|error| {
            FirmwareError::new(format!("cannot inspect private tool directory: {error}"))
        })?;
        if !current.file_type().is_dir() || !exact_mode(&current, 0o700) {
            return Err(FirmwareError::new("tool staging directory is not private"));
        }
        #[cfg(unix)]
        {
            let opened = self
                .tool_tmp
                .as_ref()
                .ok_or_else(|| FirmwareError::new("tool staging directory is not open"))?
                .metadata()
                .map_err(|error| {
                    FirmwareError::new(format!("cannot inspect tool directory handle: {error}"))
                })?;
            if !same_file(&current, &opened) {
                return Err(FirmwareError::new("tool staging directory was substituted"));
            }
        }
        Ok(())
    }

    pub(crate) fn create_file(
        &self,
        name: &str,
        bytes: &[u8],
        mode: u32,
    ) -> Result<(), FirmwareError> {
        self.check()?;
        let path = self.path(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let mut file = options.open(&path).map_err(|error| {
            FirmwareError::new(format!("cannot create {}: {error}", path.display()))
        })?;
        set_mode(&file, mode)?;
        file.write_all(bytes).map_err(|error| {
            FirmwareError::new(format!("cannot write {}: {error}", path.display()))
        })?;
        file.sync_all().map_err(|error| {
            FirmwareError::new(format!("cannot sync {}: {error}", path.display()))
        })?;
        let opened = file
            .metadata()
            .map_err(|error| FirmwareError::new(format!("cannot inspect created file: {error}")))?;
        let current = fs::symlink_metadata(&path)
            .map_err(|error| FirmwareError::new(format!("cannot recheck created file: {error}")))?;
        if !opened.file_type().is_file()
            || !current.file_type().is_file()
            || !same_file(&opened, &current)
            || !exact_mode(&opened, mode)
            || opened.len() != bytes.len() as u64
        {
            return Err(FirmwareError::new("created private file was substituted"));
        }
        self.check()
    }

    pub(crate) fn private_file(&self, name: &str, mode: u32) -> Result<PathBuf, FirmwareError> {
        self.check()?;
        let path = self.path(name);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            FirmwareError::new(format!("cannot inspect {}: {error}", path.display()))
        })?;
        if !metadata.file_type().is_file() || !exact_mode(&metadata, mode) {
            return Err(FirmwareError::new(format!(
                "{} is not a private regular file",
                path.display()
            )));
        }
        Ok(path)
    }

    pub(crate) fn verify_staged(
        &self,
        name: &str,
        mode: u32,
        pin: ArtifactPin<'_>,
    ) -> Result<Vec<u8>, FirmwareError> {
        let path = self.private_file(name, mode)?;
        let bytes = verify_artifact(&path, pin)?;
        let _ = self.private_file(name, mode)?;
        self.check()?;
        Ok(bytes)
    }

    pub(crate) fn read_private(&self, name: &str, expected: u64) -> Result<Vec<u8>, FirmwareError> {
        let path = self.private_file(name, 0o600)?;
        let bytes = read_exact_regular(&path, expected)?;
        let _ = self.private_file(name, 0o600)?;
        self.check()?;
        Ok(bytes)
    }

    pub(crate) fn persist_backup<B: MaintenanceBackend>(
        &self,
        backend: &mut B,
        backup: &[u8],
    ) -> Result<String, FirmwareError> {
        let checksum = format!("{}  full-flash.bin\n", sha256(backup));
        self.create_file("SHA256SUMS", checksum.as_bytes(), 0o600)?;
        let path = self.private_file("full-flash.bin", 0o600)?;
        sync_regular(backend, &path, backup.len() as u64)?;
        let checksum_path = self.private_file("SHA256SUMS", 0o600)?;
        sync_regular(backend, &checksum_path, checksum.len() as u64)?;
        #[cfg(unix)]
        {
            backend.full_sync(&self.directory).map_err(|error| {
                FirmwareError::new(format!("cannot fully sync workspace: {error}"))
            })?;
            backend.full_sync(&self.parent).map_err(|error| {
                FirmwareError::new(format!("cannot fully sync workspace parent: {error}"))
            })?;
        }
        #[cfg(not(unix))]
        return Err(FirmwareError::new("directory durability is unsupported"));
        #[cfg(unix)]
        {
            self.check()?;
            Ok(checksum)
        }
    }

    pub(crate) fn verify_backup_evidence(
        &self,
        backup: &[u8],
        checksum: &str,
    ) -> Result<(), FirmwareError> {
        if self.read_private("full-flash.bin", backup.len() as u64)? != backup
            || self.read_private("SHA256SUMS", checksum.len() as u64)? != checksum.as_bytes()
        {
            return Err(FirmwareError::new(
                "durable backup or checksum changed after verification",
            ));
        }
        Ok(())
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if self.check().is_ok() {
            let _ = fs::remove_file(self.path("firmware.bin"));
            let _ = fs::remove_file(self.path("esptool"));
            let _ = fs::remove_dir(self.path("tool-tmp"));
        }
    }
}
