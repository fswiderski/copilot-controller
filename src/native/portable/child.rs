//! Bounded diagnostic capture for trusted, crate-internal callers only.
//!
//! Process groups and direct-child cleanup are not sandboxes, parent-death
//! containment, or proof that all action writers have stopped.
//! Other code must not reap children owned by this supervisor.

use std::{
    fmt,
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use serde::Serialize;

pub const MAX_STDOUT_BYTES: usize = 262_144;
pub const MAX_CHILD_TIMEOUT: Duration = Duration::from_secs(120);

static CLEANUP_BLOCKED: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static TEST_CAPTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildErrorKind {
    InvalidTimeout,
    Unsupported,
    Spawn,
    Transport,
    Failed,
    OutputLimit,
    TimedOut,
    CleanupUnconfirmed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChildError {
    pub kind: ChildErrorKind,
}

impl fmt::Display for ChildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "diagnostic child: {:?}", self.kind)
    }
}

impl std::error::Error for ChildError {}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn block_after_unconfirmed_cleanup() {
    CLEANUP_BLOCKED.store(true, Ordering::SeqCst);
}

/// Exposed for status reporting; the block is deliberately not resettable.
pub fn cleanup_blocked() -> bool {
    CLEANUP_BLOCKED.load(Ordering::SeqCst)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "child/unix.rs"]
mod implementation;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) use implementation::{GroupGuard, make_nonblocking, peek_exited};
#[cfg(windows)]
#[path = "child/windows.rs"]
mod implementation;
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod implementation {
    use std::{process::Command, time::Duration};

    use super::ChildErrorKind;

    pub(super) fn capture(
        _command: &mut Command,
        _timeout: Duration,
    ) -> Result<Vec<u8>, ChildErrorKind> {
        Err(ChildErrorKind::Unsupported)
    }
}

// The integration layer supplies a preselected diagnostic Command, never a
// command or executable supplied through a public diagnostic request.
#[allow(dead_code)]
pub(crate) fn capture(command: &mut Command, timeout: Duration) -> Result<Vec<u8>, ChildError> {
    #[cfg(test)]
    let _test_capture_guard = TEST_CAPTURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cleanup_blocked() {
        return Err(ChildError {
            kind: ChildErrorKind::CleanupUnconfirmed,
        });
    }
    if timeout.is_zero() || timeout > MAX_CHILD_TIMEOUT {
        return Err(ChildError {
            kind: ChildErrorKind::InvalidTimeout,
        });
    }
    implementation::capture(command, timeout).map_err(|kind| ChildError { kind })
}
