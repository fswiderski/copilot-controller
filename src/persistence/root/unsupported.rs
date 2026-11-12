use super::{Error, ErrorKind, Name, PublishMode, Result};
use std::fs::File as StdFile;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Identity;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ChangeIdentity;

pub(super) struct Tail {
    pub(super) file: StdFile,
    pub(super) id: Identity,
}

impl Tail {
    pub(super) fn open(_path: &Path) -> Result<Self> {
        unavailable()
    }

    pub(super) fn validate(&self) -> Result<(u64, ChangeIdentity)> {
        unavailable()
    }
}

pub(super) struct File {
    pub(super) file: StdFile,
    pub(super) id: Identity,
}

pub(super) struct Root;

fn unavailable<T>() -> Result<T> {
    Err(Error::new(ErrorKind::UnsupportedPlatform, "private_root"))
}

impl Root {
    pub(super) fn open(_path: &Path) -> Result<Self> {
        unavailable()
    }

    pub(super) fn validate(&self) -> Result<()> {
        unavailable()
    }

    pub(super) fn open_file(&self, _name: &Name, _writable: bool) -> Result<Option<File>> {
        unavailable()
    }

    pub(super) fn create_file(&self, _name: &Name) -> Result<File> {
        unavailable()
    }

    pub(super) fn check_file(
        &self,
        _name: Option<&Name>,
        _file: &File,
        _allow_unlinked: bool,
    ) -> Result<u64> {
        unavailable()
    }

    pub(super) fn sync_dir(&self) -> Result<()> {
        unavailable()
    }

    pub(super) fn sync_named(&self, _name: &Name, _file: &File) -> Result<()> {
        unavailable()
    }

    pub(super) fn rename(
        &self,
        _from: &Name,
        _to: &Name,
        _mode: PublishMode,
        _source: Identity,
        _destination: Option<Identity>,
    ) -> Result<()> {
        unavailable()
    }

    pub(super) fn unlink(&self, _name: &Name, _expected: Identity) -> Result<()> {
        unavailable()
    }

    pub(super) fn is_empty_except(&self, _name: &Name) -> Result<bool> {
        unavailable()
    }
}

pub(super) fn sync_file(_file: &StdFile) -> Result<()> {
    unavailable()
}

pub(super) fn try_lock(_file: &StdFile) -> Result<()> {
    unavailable()
}
