//! Pure framing, correlation, and admission for diagnostic-only workers.
//! This module does not launch a worker or interpret a request payload.

use std::{
    fmt,
    io::{self, Read, Write},
    time::Duration,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use uuid::Uuid;

use super::strict::strict_record;

pub const MAX_FRAME_BODY: usize = 65_536;
pub const MAX_WORKER_TIMEOUT: Duration = Duration::from_secs(120);
pub const MIN_WORKER_TIMEOUT: Duration = Duration::from_millis(1);

strict_record! {
    #[derive(Clone, Debug, PartialEq, Serialize)]
    pub struct DiagnosticRequest as RawDiagnosticRequest {
        pub kind: String,
        pub payload: Value,
    }
}

impl DiagnosticRequest {
    pub fn validate(&self) -> Result<(), WorkerErrorKind> {
        let kind = self.kind.as_bytes();
        if kind.is_empty()
            || kind.len() > 64
            || !kind.iter().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(byte)
            })
        {
            return Err(WorkerErrorKind::InvalidRequest);
        }
        Ok(())
    }
}

strict_record! {
    #[derive(Clone, Debug, PartialEq, Serialize)]
    pub struct WireRequest as RawWireRequest {
        pub version: u32,
        #[serde(with = "super::uuid_wire")]
        pub request_nonce: Uuid,
        pub request: DiagnosticRequest,
    }
}

impl WireRequest {
    pub fn validate(&self) -> Result<(), WorkerErrorKind> {
        if self.version != 1 || self.request_nonce.is_nil() {
            return Err(WorkerErrorKind::InvalidRequest);
        }
        self.request.validate()
    }
}

strict_record! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    pub struct WorkerIdentity as RawWorkerIdentity {
        pub pid: u32,
        pub creation_time_100ns: u64,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkerOutcome {
    Ok { payload: Value },
    CallbackFailed,
}

strict_record! {
    #[derive(Clone, Debug, PartialEq, Serialize)]
    pub struct WireResponse as RawWireResponse {
        pub version: u32,
        #[serde(with = "super::uuid_wire")]
        pub request_nonce: Uuid,
        pub worker: WorkerIdentity,
        pub outcome: WorkerOutcome,
    }
}

impl WireResponse {
    pub fn validate_correlation(
        &self,
        expected_nonce: Uuid,
        expected_worker: WorkerIdentity,
    ) -> Result<(), WorkerErrorKind> {
        if self.version != 1
            || expected_nonce.is_nil()
            || self.request_nonce != expected_nonce
            || expected_worker.pid == 0
            || expected_worker.creation_time_100ns == 0
            || self.worker != expected_worker
        {
            return Err(WorkerErrorKind::Protocol);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    FrameLimit,
    Transport,
    Protocol,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for FrameError {}

struct BoundedBody(Vec<u8>);

impl Write for BoundedBody {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() > MAX_FRAME_BODY - self.0.len() {
            return Err(io::Error::other("frame limit"));
        }
        self.0.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Serialization itself is bounded; a serializer failure is also a frame limit.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let mut body = BoundedBody(Vec::new());
    serde_json::to_writer(&mut body, value).map_err(|_| FrameError::FrameLimit)?;
    if body.0.is_empty() {
        return Err(FrameError::FrameLimit);
    }
    let mut frame = Vec::with_capacity(4 + body.0.len());
    frame.extend_from_slice(&(body.0.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body.0);
    Ok(frame)
}

/// Pass the owned pipe writer so it is closed after the frame is flushed.
pub fn write_frame<W: Write, T: Serialize>(mut writer: W, value: &T) -> Result<(), FrameError> {
    let frame = encode_frame(value)?;
    writer
        .write_all(&frame)
        .map_err(|_| FrameError::Transport)?;
    writer.flush().map_err(|_| FrameError::Transport)
}

/// Requires EOF after one frame. A supervisor must bound blocking I/O.
pub fn read_frame<R: Read, T: DeserializeOwned>(mut reader: R) -> Result<T, FrameError> {
    let mut header = [0_u8; 4];
    reader
        .read_exact(&mut header)
        .map_err(|_| FrameError::Transport)?;
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BODY {
        return Err(FrameError::FrameLimit);
    }
    let mut body = vec![0_u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|_| FrameError::Transport)?;
    let mut trailing = [0_u8; 1];
    loop {
        match reader.read(&mut trailing) {
            Ok(0) => break,
            Ok(_) => return Err(FrameError::Protocol),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(FrameError::Transport),
        }
    }
    serde_json::from_slice(&body).map_err(|_| FrameError::Protocol)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
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

impl From<FrameError> for WorkerErrorKind {
    fn from(error: FrameError) -> Self {
        match error {
            FrameError::FrameLimit => Self::FrameLimit,
            FrameError::Transport => Self::Transport,
            FrameError::Protocol => Self::Protocol,
        }
    }
}

/// Cleanup uncertainty overrides the original error and forbids replacement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkerError {
    kind: WorkerErrorKind,
    worker: Option<WorkerIdentity>,
    replacement_allowed: bool,
}

impl WorkerError {
    pub fn before_launch(kind: WorkerErrorKind) -> Self {
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

    pub fn after_launch(
        kind: WorkerErrorKind,
        worker: Option<WorkerIdentity>,
        cleanup_confirmed: bool,
    ) -> Self {
        if !cleanup_confirmed {
            return Self {
                kind: WorkerErrorKind::CleanupUnconfirmed,
                worker,
                replacement_allowed: false,
            };
        }
        Self {
            kind,
            worker,
            replacement_allowed: !matches!(
                kind,
                WorkerErrorKind::Busy
                    | WorkerErrorKind::SupervisionBlocked
                    | WorkerErrorKind::CleanupUnconfirmed
            ),
        }
    }

    pub fn replacement_allowed(&self) -> bool {
        self.replacement_allowed
    }

    pub fn kind(&self) -> WorkerErrorKind {
        self.kind
    }

    pub fn worker(&self) -> Option<WorkerIdentity> {
        self.worker
    }
}

impl fmt::Display for WorkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "diagnostic worker: {:?}", self.kind)
    }
}

impl std::error::Error for WorkerError {}

/// Encoded before platform dispatch; the actual worker supplies its own
/// diagnostic-only allowlist and process containment.
#[derive(Debug)]
pub struct PreparedProbe {
    request_nonce: Uuid,
    timeout: Duration,
    frame: Vec<u8>,
}

impl PreparedProbe {
    pub fn frame(&self) -> &[u8] {
        &self.frame
    }

    pub fn request_nonce(&self) -> Uuid {
        self.request_nonce
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

pub fn prepare_probe(
    timeout: Duration,
    cancelled: bool,
    request: DiagnosticRequest,
) -> Result<PreparedProbe, WorkerError> {
    if !(MIN_WORKER_TIMEOUT..=MAX_WORKER_TIMEOUT).contains(&timeout) {
        return Err(WorkerError::before_launch(WorkerErrorKind::InvalidTimeout));
    }
    if cancelled {
        return Err(WorkerError::before_launch(WorkerErrorKind::Cancelled));
    }
    request.validate().map_err(WorkerError::before_launch)?;
    let wire = WireRequest {
        version: 1,
        request_nonce: Uuid::new_v4(),
        request,
    };
    let frame = encode_frame(&wire).map_err(|error| WorkerError::before_launch(error.into()))?;
    Ok(PreparedProbe {
        request_nonce: wire.request_nonce,
        timeout,
        frame,
    })
}

/// Non-Windows dispatch performs all admission checks but never calls a worker.
#[cfg(not(windows))]
pub fn unavailable_framed_worker(
    timeout: Duration,
    cancelled: bool,
    request: DiagnosticRequest,
) -> Result<Value, WorkerError> {
    let _prepared = prepare_probe(timeout, cancelled, request)?;
    Err(WorkerError::before_launch(
        WorkerErrorKind::UnsupportedPlatform,
    ))
}
