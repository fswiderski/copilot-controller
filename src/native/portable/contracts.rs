use std::fmt;

use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformErrorKind {
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

/// Operation names are static identifiers, never native error strings or paths.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlatformError {
    pub kind: PlatformErrorKind,
    pub operation: &'static str,
    pub native_code: Option<u32>,
}

impl PlatformError {
    pub const fn new(
        kind: PlatformErrorKind,
        operation: &'static str,
        native_code: Option<u32>,
    ) -> Self {
        Self {
            kind,
            operation,
            native_code,
        }
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    pub(crate) fn last_native_error(operation: &'static str) -> Self {
        let native_code = std::io::Error::last_os_error()
            .raw_os_error()
            .map(|code| code as u32);
        Self::new(PlatformErrorKind::NativeFailure, operation, native_code)
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

/// Unavailability is distinct from an observed empty result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "evidence", rename_all = "snake_case")]
pub enum Observation<T> {
    Observed(T),
    Unavailable(PlatformError),
}
