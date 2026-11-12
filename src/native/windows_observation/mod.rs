//! Read-only Windows observations. None of these observations grants authority.
//!
//! Integration dependencies: `serde` with `derive`, `uuid`, and, on Windows,
//! `windows-sys` 0.61 with the Win32 features listed in `WINDOWS_FEATURES`, and
//! `windows` 0.62 with `WINDOWS_COM_FEATURES` for UI Automation.
//! Native helpers must run in a disposable, deadline-supervised process.

mod clock;
mod process;
mod qualification;
mod report;
mod serial;

#[cfg(not(windows))]
mod unsupported;
#[cfg(windows)]
mod windows;

pub use clock::*;
pub use process::*;
pub use qualification::*;
pub use report::*;
pub use serial::*;
#[cfg(not(windows))]
pub use unsupported::*;
#[cfg(windows)]
pub use windows::*;

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Features required on the target-specific `windows-sys` dependency.
pub const WINDOWS_FEATURES: &[&str] = &[
    "Win32_Foundation",
    "Win32_Devices_DeviceAndDriverInstallation",
    "Win32_Security",
    "Win32_Storage_FileSystem",
    "Win32_System_Console",
    "Win32_System_Diagnostics_ToolHelp",
    "Win32_System_Performance",
    "Win32_System_Registry",
    "Win32_System_RemoteDesktop",
    "Win32_System_StationsAndDesktops",
    "Win32_System_Threading",
    "Win32_UI_WindowsAndMessaging",
];

pub const WINDOWS_COM_FEATURES: &[&str] = &[
    "Win32_Foundation",
    "Win32_System_Com",
    "Win32_UI_Accessibility",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    UnsupportedPlatform,
    UnsupportedContext,
    AccessDenied,
    ProcessExited,
    NoForeground,
    ObservationChanged,
    InvalidData,
    LimitExceeded,
    NativeFailure,
}

/// Only static operation names and numeric codes may cross the reporting boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlatformError {
    pub kind: ErrorKind,
    pub operation: &'static str,
    pub native_code: Option<u32>,
}

impl PlatformError {
    pub const fn new(kind: ErrorKind, operation: &'static str) -> Self {
        Self {
            kind,
            operation,
            native_code: None,
        }
    }

    pub const fn with_code(kind: ErrorKind, operation: &'static str, native_code: u32) -> Self {
        Self {
            kind,
            operation,
            native_code: Some(native_code),
        }
    }
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {:?}", self.operation, self.kind)?;
        if let Some(code) = self.native_code {
            write!(f, " (0x{code:X})")?;
        }
        Ok(())
    }
}

impl std::error::Error for PlatformError {}

#[cfg(any(windows, test))]
pub(crate) fn windows_native_error(operation: &'static str, code: u32) -> PlatformError {
    const ERROR_ACCESS_DENIED: u32 = 5;
    const E_ACCESSDENIED: u32 = 0x8007_0005;
    let kind = if code == ERROR_ACCESS_DENIED || code == E_ACCESSDENIED {
        ErrorKind::AccessDenied
    } else {
        ErrorKind::NativeFailure
    };
    PlatformError::with_code(kind, operation, code)
}

#[cfg(any(windows, test))]
pub(crate) fn configuration_manager_error(operation: &'static str, code: u32) -> PlatformError {
    const CR_BUFFER_SMALL: u32 = 26;
    const CR_ACCESS_DENIED: u32 = 51;
    let kind = match code {
        CR_ACCESS_DENIED => ErrorKind::AccessDenied,
        CR_BUFFER_SMALL => ErrorKind::LimitExceeded,
        _ => ErrorKind::NativeFailure,
    };
    PlatformError::with_code(kind, operation, code)
}

pub type PlatformResult<T> = Result<T, PlatformError>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "evidence", rename_all = "snake_case")]
pub enum Observation<T> {
    Observed(T),
    Unavailable(PlatformError),
}

impl<T> From<PlatformResult<T>> for Observation<T> {
    fn from(value: PlatformResult<T>) -> Self {
        match value {
            Ok(value) => Self::Observed(value),
            Err(error) => Self::Unavailable(error),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub creation_time_100ns: u64,
    pub session_id: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonotonicStamp {
    #[serde(with = "canonical_uuid")]
    pub epoch: Uuid,
    pub ticks: u64,
    pub frequency: u64,
}

mod canonical_uuid {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    use uuid::Uuid;

    pub fn serialize<S: Serializer>(value: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.hyphenated().to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Uuid, D::Error> {
        let value = String::deserialize(deserializer)?;
        let bytes = value.as_bytes();
        if bytes.len() != 36
            || bytes.iter().enumerate().any(|(i, b)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    *b != b'-'
                } else {
                    !b.is_ascii_hexdigit()
                }
            })
        {
            return Err(D::Error::custom("expected a canonical UUID"));
        }
        Uuid::parse_str(&value).map_err(|_| D::Error::custom("invalid UUID"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutableClass {
    NativeHost,
    Cli,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProcessEvidence {
    pub identity: ProcessIdentity,
    pub class: ExecutableClass,
    pub stamp: MonotonicStamp,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ForegroundEvidence {
    pub window: u64,
    pub owner_thread_id: u32,
    pub owner: ProcessEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DesktopContext {
    pub process_session_id: u32,
    pub active_console_session_id: Option<u32>,
    pub active: bool,
    pub local: bool,
    pub interactive_station: bool,
    pub thread_default_desktop: bool,
    pub input_default_desktop: bool,
    pub stamp: MonotonicStamp,
}

#[cfg(test)]
mod tests;
