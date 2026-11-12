use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

pub const CONFIRMATION: &str = "INSTALL MICRO 0.6.3";
const PORT_PREFIX: &str = "/dev/cu.usbmodem";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateOptions {
    pub image: PathBuf,
    pub esptool: PathBuf,
    pub port: OsString,
    pub backup_dir: PathBuf,
    pub confirm: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OptionError {
    Confirmation,
    ImageNotAbsolute,
    ToolNotAbsolute,
    BackupNotAbsolute,
    InvalidPort,
}

impl fmt::Display for OptionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Confirmation => "confirmation must be exactly INSTALL MICRO 0.6.3",
            Self::ImageNotAbsolute => "image path must be absolute",
            Self::ToolNotAbsolute => "tool path must be absolute",
            Self::BackupNotAbsolute => "backup path must be absolute",
            Self::InvalidPort => "port must be /dev/cu.usbmodem followed by ASCII alphanumerics",
        })
    }
}

impl std::error::Error for OptionError {}

/// Validate without touching files, starting a worker, or accessing devices.
pub fn validate_options(options: &UpdateOptions) -> Result<(), OptionError> {
    if options.confirm != CONFIRMATION {
        return Err(OptionError::Confirmation);
    }
    if !options.image.is_absolute() {
        return Err(OptionError::ImageNotAbsolute);
    }
    if !options.esptool.is_absolute() {
        return Err(OptionError::ToolNotAbsolute);
    }
    if !options.backup_dir.is_absolute() {
        return Err(OptionError::BackupNotAbsolute);
    }
    let suffix = options
        .port
        .to_str()
        .and_then(|port| port.strip_prefix(PORT_PREFIX))
        .ok_or(OptionError::InvalidPort)?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(OptionError::InvalidPort);
    }
    Ok(())
}
