//! Contained, one-shot Windows and macOS ARM64 diagnostic workers.
//!
//! The executable supplies a static allowlist of diagnostic callbacks and routes
//! [`HIDDEN_WORKER_ARGUMENT`] (or the fixed AKP03 route) to
//! [`maybe_run_hidden_worker`] with that route's registry before doing any
//! console attachment or normal command-line processing. The hidden invocation
//! must exit immediately after the returned result, with a nonzero status on
//! error. This module never selects an executable or executes a shell command.
//!
//! A successful diagnostic is not action readiness or physical qualification.

use std::ffi::OsStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(not(windows))]
use uuid::Uuid;

#[cfg(any(test, all(windows, any(feature = "micro-hid", feature = "akp03-hid"))))]
mod hid_scope;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod macos;
mod protocol;
#[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
mod supervision;
#[cfg(windows)]
mod windows;
#[cfg(all(windows, any(feature = "micro-hid", feature = "akp03-hid")))]
pub(crate) use hid_scope::{HidWorkerCapability, hid_worker_capability};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) use macos::worker_health;
#[cfg(all(
    target_os = "macos",
    target_arch = "aarch64",
    any(feature = "micro-hid", feature = "akp03-hid")
))]
pub(crate) use macos::{HidWorkerCapability, hid_worker_capability};

#[cfg(test)]
mod tests;

/// The executable must treat this as a private, exact single-argument route.
pub const HIDDEN_WORKER_ARGUMENT: &str = "__probe-worker";
/// AKP03 uses the same supervisor with a separate kind-only hidden registry.
pub const AKP03_HIDDEN_WORKER_ARGUMENT: &str = "__akp03-worker";

/// Fixed same-executable routes, never arbitrary command-line arguments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticWorkerRoute {
    Shared,
    Akp03,
}

impl DiagnosticWorkerRoute {
    fn argument(self) -> &'static str {
        match self {
            Self::Shared => HIDDEN_WORKER_ARGUMENT,
            Self::Akp03 => AKP03_HIDDEN_WORKER_ARGUMENT,
        }
    }
}
/// Maximum JSON body size, excluding the four-byte frame header.
pub const MAX_FRAME_BODY_BYTES: usize = 65_536;
/// Leaves room for the fixed response envelope, even with maximal worker IDs.
pub const MAX_CALLBACK_PAYLOAD_BYTES: usize = MAX_FRAME_BODY_BYTES - 256;
/// Maximum discarded worker stderr, including the boundary.
pub const MAX_DIAGNOSTIC_BYTES: usize = 16_384;

const MIN_TIMEOUT: Duration = Duration::from_millis(1);
const MAX_TIMEOUT: Duration = Duration::from_secs(120);

/// The public request has no executable, command, or shell field.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticRequest {
    pub kind: String,
    pub payload: Value,
}

/// The process identity retained from the actual worker process handle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerIdentity {
    pub pid: u32,
    /// Windows FILETIME; on macOS, native process start time since Unix epoch.
    pub creation_time_100ns: u64,
}

/// A correlated payload, returned only after the retained worker has exited.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticResult {
    pub worker: WorkerIdentity,
    pub payload: Value,
}

/// Static error categories; no native, callback, or request strings are kept.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerErrorKind {
    UnsupportedPlatform,
    InvalidTimeout,
    InvalidRequest,
    Busy,
    SupervisionBlocked,
    Containment,
    Spawn,
    Protocol,
    FrameLimit,
    DiagnosticLimit,
    Transport,
    Callback,
    TimedOut,
    Cancelled,
    WorkerFailed,
    CleanupUnconfirmed,
}

/// A bounded failure and the resulting process-local replacement decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerError {
    pub kind: WorkerErrorKind,
    pub worker: Option<WorkerIdentity>,
    pub replacement_allowed: bool,
}

impl WorkerError {
    fn before_launch(kind: WorkerErrorKind) -> Self {
        Self {
            kind,
            worker: None,
            replacement_allowed: !matches!(
                kind,
                WorkerErrorKind::Busy
                    | WorkerErrorKind::SupervisionBlocked
                    | WorkerErrorKind::CleanupUnconfirmed
            ),
        }
    }

    #[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64"), test))]
    fn after_launch(
        kind: WorkerErrorKind,
        worker: Option<WorkerIdentity>,
        process_exited: bool,
        io_joined: bool,
    ) -> Self {
        let replacement_allowed = process_exited && io_joined;
        Self {
            kind: if replacement_allowed {
                kind
            } else {
                WorkerErrorKind::CleanupUnconfirmed
            },
            worker,
            replacement_allowed,
        }
    }
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "diagnostic worker: {:?}", self.kind)
    }
}

impl std::error::Error for WorkerError {}

/// Callback failures intentionally carry no private text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticCallbackError;

/// A diagnostic callback must not perform an action or write protocol stdout.
pub type DiagnosticCallbackFn = fn(Value) -> Result<Value, DiagnosticCallbackError>;

/// An entry must be compiled into the executable's diagnostic-only allowlist.
#[derive(Clone, Copy)]
pub struct DiagnosticCallback {
    kind: &'static str,
    callback: DiagnosticCallbackFn,
}

impl DiagnosticCallback {
    pub const fn new(kind: &'static str, callback: DiagnosticCallbackFn) -> Self {
        Self { kind, callback }
    }
}

/// A finite static callback allowlist; it contains no executable paths.
pub struct DiagnosticAllowlist {
    callbacks: &'static [DiagnosticCallback],
    route: DiagnosticWorkerRoute,
}

impl DiagnosticAllowlist {
    pub fn new(callbacks: &'static [DiagnosticCallback]) -> Result<Self, WorkerError> {
        Self::new_for_route(callbacks, DiagnosticWorkerRoute::Shared)
    }

    pub fn new_for_route(
        callbacks: &'static [DiagnosticCallback],
        route: DiagnosticWorkerRoute,
    ) -> Result<Self, WorkerError> {
        for (index, entry) in callbacks.iter().enumerate() {
            if !protocol::valid_kind(entry.kind)
                || callbacks[..index]
                    .iter()
                    .any(|previous| previous.kind == entry.kind)
            {
                return Err(WorkerError::before_launch(WorkerErrorKind::InvalidRequest));
            }
        }
        Ok(Self { callbacks, route })
    }

    fn find(&self, kind: &str) -> Option<DiagnosticCallbackFn> {
        self.callbacks
            .iter()
            .find(|entry| entry.kind == kind)
            .map(|entry| entry.callback)
    }
}

/// Validate and perform a single diagnostic probe.
///
/// Timeout, prelaunch cancellation, request/allowlist validation and bounded
/// encoding occur before platform dispatch. macOS ARM64 admits only health,
/// Micro and AKP03 diagnostics; other non-Windows targets fail closed.
pub fn probe(
    request: &DiagnosticRequest,
    timeout: Duration,
    cancelled: &AtomicBool,
    allowlist: &DiagnosticAllowlist,
) -> Result<DiagnosticResult, WorkerError> {
    use WorkerErrorKind as Kind;

    if !(MIN_TIMEOUT..=MAX_TIMEOUT).contains(&timeout) {
        return Err(WorkerError::before_launch(Kind::InvalidTimeout));
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(WorkerError::before_launch(Kind::Cancelled));
    }
    if !protocol::valid_kind(&request.kind) || allowlist.find(&request.kind).is_none() {
        return Err(WorkerError::before_launch(Kind::InvalidRequest));
    }

    #[cfg(windows)]
    let nonce = windows::new_nonce().map_err(WorkerError::before_launch)?;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let nonce = Uuid::new_v4();
    #[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
    let nonce = Uuid::from_u128(1);

    let frame = protocol::encode_frame(&protocol::RequestEnvelopeRef {
        version: protocol::VERSION,
        request_nonce: nonce,
        request,
    })
    .map_err(WorkerError::before_launch)?;

    #[cfg(windows)]
    {
        windows::supervise(frame, nonce, timeout, cancelled, allowlist.route)
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        let budget =
            macos::validate_platform_request(request).map_err(WorkerError::before_launch)?;
        macos::supervise(
            frame,
            nonce,
            timeout.min(budget),
            cancelled,
            allowlist.route,
        )
    }
    #[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
    {
        let _ = (frame, nonce);
        Err(WorkerError::before_launch(Kind::UnsupportedPlatform))
    }
}

/// Recognize and serve the registry's exact hidden route, or return `None` for a
/// normal invocation. `arguments` excludes the executable's own argv[0].
///
/// Call this before any console attachment. A `Some` result must be followed
/// by immediate process exit; do not continue into the public CLI.
pub fn maybe_run_hidden_worker<I, S>(
    arguments: I,
    allowlist: &DiagnosticAllowlist,
) -> Option<Result<(), WorkerError>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut arguments = arguments.into_iter();
    if arguments.next()?.as_ref() != OsStr::new(allowlist.route.argument()) {
        return None;
    }
    if arguments.next().is_some() {
        return Some(Err(WorkerError::before_launch(
            WorkerErrorKind::InvalidRequest,
        )));
    }
    #[cfg(windows)]
    {
        Some(windows::worker_main(allowlist))
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some(macos::worker_main(allowlist))
    }
    #[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
    {
        let _ = allowlist;
        Some(Err(WorkerError::before_launch(
            WorkerErrorKind::UnsupportedPlatform,
        )))
    }
}
