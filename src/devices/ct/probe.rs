//! Bounded CT serial probe protocol. The transport is injected so validation
//! and tests never discover or access hardware.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

#[cfg(windows)]
use crate::native::windows_observation::Observation;
use crate::native::windows_observation::{
    SerialSelection, SerialSelectionState, normalize_com_port,
};

use super::protocol::{FrameDecoder, InputEvent, decode_input};

pub(crate) const UPGRADE: &[u8] =
    b"GET /index.html\nHTTP/1.1\nConnection: Upgrade\nUpgrade: websocket\nSec-WebSocket-Key: 123abc\n\n";
const CLOSE: &[u8] = &[0x88, 0x80, 0, 0, 0, 0];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    pub schema_version: u8,
    pub total_timeout_ms: u32,
    pub io_timeout_ms: u32,
    pub capture_ms: u32,
    pub max_events: u16,
    pub max_received_bytes: u32,
    pub test_pattern: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            schema_version: 1,
            total_timeout_ms: 8_000,
            io_timeout_ms: 1_000,
            capture_ms: 2_000,
            max_events: 128,
            max_received_bytes: 32_768,
            test_pattern: false,
        }
    }
}

impl Options {
    pub fn validate(&self) -> Result<(), ProbeErrorKind> {
        if self.schema_version != 1
            || !(1_000..=8_000).contains(&self.total_timeout_ms)
            || !(100..=1_500).contains(&self.io_timeout_ms)
            || self.io_timeout_ms > self.total_timeout_ms
            || self.capture_ms > 5_000
            || self.capture_ms > self.total_timeout_ms
            || !(1..=128).contains(&self.max_events)
            || !(256..=32_768).contains(&self.max_received_bytes)
        {
            return Err(ProbeErrorKind::InvalidOptions);
        }
        Ok(())
    }
}

pub fn select_ct_port(selection: &SerialSelection) -> Result<String, ProbeErrorKind> {
    match selection.state {
        SerialSelectionState::NotFound => return Err(ProbeErrorKind::DeviceUnavailable),
        SerialSelectionState::Incomplete => return Err(ProbeErrorKind::DiscoveryIncomplete),
        SerialSelectionState::Ambiguous => return Err(ProbeErrorKind::AmbiguousDevice),
        SerialSelectionState::Unique => {}
    }
    if selection.present_count == 0 || selection.matching_count != 1 || selection.ports.len() != 1 {
        return Err(ProbeErrorKind::DiscoveryIncomplete);
    }
    let port =
        normalize_com_port(&selection.ports[0]).ok_or(ProbeErrorKind::DiscoveryIncomplete)?;
    let selected = normalize_com_port(
        selection
            .selected_port
            .as_deref()
            .ok_or(ProbeErrorKind::DiscoveryIncomplete)?,
    )
    .ok_or(ProbeErrorKind::DiscoveryIncomplete)?;
    if port == selected {
        Ok(port)
    } else {
        Err(ProbeErrorKind::DiscoveryIncomplete)
    }
}

#[cfg(windows)]
fn selection_error<T>(observation: Observation<T>) -> Result<T, ProbeErrorKind> {
    match observation {
        Observation::Observed(value) => Ok(value),
        Observation::Unavailable(error) => Err(match error.kind {
            crate::native::windows_observation::ErrorKind::AccessDenied => {
                ProbeErrorKind::BusyOrDenied
            }
            _ => ProbeErrorKind::DiscoveryIncomplete,
        }),
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Options,
    Discovery,
    Open,
    Configure,
    Handshake,
    Identity,
    Firmware,
    Pattern,
    Capture,
    Cleanup,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeErrorKind {
    InvalidOptions,
    UnsupportedPlatform,
    DeviceUnavailable,
    DiscoveryIncomplete,
    AmbiguousDevice,
    AssociationChanged,
    BusyOrDenied,
    NativeFailure,
    InvalidSerialSettings,
    SerialLineError,
    Cancelled,
    Timeout,
    Disconnected,
    InvalidHandshake,
    SuspectedStaleSession,
    MalformedFrame,
    TruncatedFrame,
    UnexpectedReply,
    InvalidReply,
    InvalidInput,
    InputLimit,
    ByteLimit,
    ShortWrite,
    TransportContract,
    ClockRegression,
    CleanupPending,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Discovery,
    Open,
    AssociationCheck,
    Configure,
    UpgradeWrite,
    UpgradeResponse,
    HeaderValidation,
    IdentityWrite,
    IdentityResponse,
    FirmwareWrite,
    FirmwareResponse,
    Pattern,
    Capture,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseStatus {
    NotAttempted,
    Skipped,
    Sent,
    WriteFailed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Completion {
    CaptureDurationElapsed,
    InputLimitReached,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    Synthetic,
    NativeTransportObservation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseProgress {
    pub phase: Phase,
    pub started_ms: u32,
    pub elapsed_ms: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestProgress {
    pub written_bytes: u32,
    pub total_bytes: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeProgress {
    pub evidence: Evidence,
    pub last_phase: Phase,
    pub elapsed_ms: u32,
    pub phases: Vec<PhaseProgress>,
    #[serde(default)]
    pub port_opened_at_ms: Option<u32>,
    #[serde(default)]
    pub port_configured_at_ms: Option<u32>,
    pub upgrade_request: RequestProgress,
    pub identity_request: RequestProgress,
    pub firmware_request: RequestProgress,
    pub close_request: RequestProgress,
    pub upgrade_response_bytes: u32,
    pub handshake_response_start: Option<String>,
    pub header_bytes: u32,
    pub header_complete: bool,
    pub header_validated: bool,
    pub received_bytes: u32,
    pub frames_received: u32,
    pub identity_frames_received: u32,
    pub firmware_frames_received: u32,
    pub session_close: CloseStatus,
    #[serde(default)]
    pub serial: Option<SerialDiagnostics>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SerialDiagnostics {
    pub read_calls: u32,
    pub write_calls: u32,
    pub read_failures: u32,
    pub write_failures: u32,
    pub completed_read_bytes: u32,
    pub completed_write_bytes: u32,
    pub idle_reads: u32,
    pub line_status_checks: u32,
    pub line_status_failures: u32,
    pub line_error_observations: u32,
    pub line_error_flags: u32,
    pub last_input_queue_bytes: Option<u32>,
    pub last_output_queue_bytes: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeFailure {
    pub kind: ProbeErrorKind,
    pub stage: Stage,
    pub native_code: Option<u32>,
    pub progress: Box<ProbeProgress>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputSample {
    pub elapsed_ms: u32,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeReport {
    pub schema_version: u8,
    pub evidence: Evidence,
    pub hardware_qualification: String,
    pub physical_input_qualification: String,
    pub visual_acceptance: String,
    pub http_101: bool,
    pub identity_response_length: u8,
    pub firmware_version: [u8; 3],
    pub reply_length_disagreements: u32,
    #[serde(default)]
    pub serial_settings: Option<SerialSettings>,
    pub pattern: String,
    pub completion: Completion,
    pub elapsed_ms: u32,
    pub received_bytes: u32,
    pub written_bytes: u32,
    pub close: CloseStatus,
    pub ticks: u32,
    pub inputs: Vec<InputSample>,
    pub progress: ProbeProgress,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StaticLineMode {
    Off,
    On,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SerialSettings {
    pub baud_rate: u32,
    pub data_bits: u8,
    pub parity: String,
    pub stop_bits: u8,
    pub binary: bool,
    pub flow_control: String,
    pub nul_stripping: bool,
    pub dtr: StaticLineMode,
    pub rts: StaticLineMode,
}

#[cfg(any(windows, test))]
fn static_modem_modes(bits: u32) -> Option<(StaticLineMode, StaticLineMode)> {
    let dtr = match bits & 0x30 {
        0 => StaticLineMode::Off,
        0x10 => StaticLineMode::On,
        _ => return None,
    };
    let rts = match bits & 0x3000 {
        0 => StaticLineMode::Off,
        0x1000 => StaticLineMode::On,
        _ => return None,
    };
    Some((dtr, rts))
}

#[cfg(any(windows, test))]
fn win32_error_kind(code: u32) -> ProbeErrorKind {
    match code {
        5 | 32 => ProbeErrorKind::BusyOrDenied,
        1167 => ProbeErrorKind::Disconnected,
        _ => ProbeErrorKind::NativeFailure,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadResult {
    Data(usize),
    Idle,
    Disconnected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportError {
    pub kind: ProbeErrorKind,
    pub native_code: Option<u32>,
}

/// Nonblocking bounded operations. `now_ms` is elapsed monotonic time from
/// transport creation; an idle read must advance it.
pub trait Transport {
    fn now_ms(&mut self) -> u64;
    fn write(
        &mut self,
        bytes: &[u8],
        deadline_ms: u64,
        cancelled: &AtomicBool,
    ) -> Result<usize, TransportError>;
    fn read(
        &mut self,
        bytes: &mut [u8],
        deadline_ms: u64,
        cancelled: &AtomicBool,
    ) -> Result<ReadResult, TransportError>;

    fn progress_origin_ms(&self) -> Option<u64> {
        None
    }

    fn initial_phases(&self) -> &[PhaseProgress] {
        &[]
    }

    fn port_opened_at_ms(&self) -> Option<u32> {
        None
    }

    fn port_configured_at_ms(&self) -> Option<u32> {
        None
    }

    fn serial_diagnostics(&self) -> Option<SerialDiagnostics> {
        None
    }

    fn serial_settings(&self) -> Option<SerialSettings> {
        None
    }
}

#[derive(Clone, Copy)]
struct Clock {
    last: u64,
}

impl Clock {
    fn observe<T: Transport>(&mut self, transport: &mut T) -> Result<u64, ProbeErrorKind> {
        let now = transport.now_ms();
        if now < self.last {
            return Err(ProbeErrorKind::ClockRegression);
        }
        self.last = now;
        Ok(now)
    }
}

struct Progress {
    record: ProbeProgress,
    phase_started: u64,
}

impl Progress {
    fn new(evidence: Evidence) -> Self {
        Self {
            record: ProbeProgress {
                evidence,
                last_phase: Phase::Preparing,
                elapsed_ms: 0,
                phases: vec![PhaseProgress {
                    phase: Phase::Preparing,
                    started_ms: 0,
                    elapsed_ms: 0,
                }],
                port_opened_at_ms: None,
                port_configured_at_ms: None,
                upgrade_request: RequestProgress {
                    written_bytes: 0,
                    total_bytes: UPGRADE.len() as u32,
                },
                identity_request: RequestProgress {
                    written_bytes: 0,
                    total_bytes: 9,
                },
                firmware_request: RequestProgress {
                    written_bytes: 0,
                    total_bytes: 9,
                },
                close_request: RequestProgress {
                    written_bytes: 0,
                    total_bytes: 6,
                },
                upgrade_response_bytes: 0,
                handshake_response_start: None,
                header_bytes: 0,
                header_complete: false,
                header_validated: false,
                received_bytes: 0,
                frames_received: 0,
                identity_frames_received: 0,
                firmware_frames_received: 0,
                session_close: CloseStatus::NotAttempted,
                serial: None,
            },
            phase_started: 0,
        }
    }

    fn tick(&mut self, now: u64) {
        let elapsed = now
            .saturating_sub(self.phase_started)
            .min(u64::from(u32::MAX)) as u32;
        self.record.elapsed_ms = now.min(u64::from(u32::MAX)) as u32;
        if let Some(phase) = self.record.phases.last_mut() {
            phase.elapsed_ms = elapsed;
        }
    }

    fn enter(&mut self, phase: Phase, now: u64) {
        self.tick(now);
        if phase_rank(phase) > phase_rank(self.record.last_phase) && self.record.phases.len() < 14 {
            self.record.phases.push(PhaseProgress {
                phase,
                started_ms: now.min(u64::from(u32::MAX)) as u32,
                elapsed_ms: 0,
            });
            self.record.last_phase = phase;
            self.phase_started = now;
        }
    }

    fn seed(&mut self, phases: &[PhaseProgress]) {
        for phase in phases {
            if phase_rank(phase.phase) > phase_rank(self.record.last_phase)
                && self.record.phases.len() < 14
            {
                self.record.phases.push(phase.clone());
                self.record.last_phase = phase.phase;
                self.phase_started = u64::from(phase.started_ms);
            }
        }
    }
}

fn phase_rank(phase: Phase) -> u8 {
    match phase {
        Phase::Preparing => 0,
        Phase::Discovery => 1,
        Phase::Open => 2,
        Phase::AssociationCheck => 3,
        Phase::Configure => 4,
        Phase::UpgradeWrite => 5,
        Phase::UpgradeResponse => 6,
        Phase::HeaderValidation => 7,
        Phase::IdentityWrite => 8,
        Phase::IdentityResponse => 9,
        Phase::FirmwareWrite => 10,
        Phase::FirmwareResponse => 11,
        Phase::Pattern => 12,
        Phase::Capture => 13,
    }
}

struct Engine<'a, T> {
    transport: T,
    options: Options,
    cancelled: &'a AtomicBool,
    started_at: u64,
    clock: Clock,
    progress: Progress,
    stage: Stage,
    deadline: u64,
    received: u32,
    written: u32,
    disagreement_count: u32,
    input_samples: Vec<InputSample>,
    ticks: u32,
    frame_decoder: FrameDecoder,
    pending: Vec<u8>,
    upgraded: bool,
    uncertain_write: bool,
    close_attempted: bool,
    native_code: Option<u32>,
}

impl<T: Transport> Engine<'_, T> {
    fn elapsed(&self, now: u64) -> u64 {
        now.saturating_sub(self.started_at)
    }

    fn now(&mut self) -> Result<u64, ProbeErrorKind> {
        self.clock.observe(&mut self.transport)
    }

    fn check(&mut self, deadline: u64) -> Result<u64, ProbeErrorKind> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(ProbeErrorKind::Cancelled);
        }
        let now = self.now()?;
        self.progress.tick(self.elapsed(now));
        if now >= deadline {
            return Err(ProbeErrorKind::Timeout);
        }
        Ok(now)
    }

    fn write_all(
        &mut self,
        bytes: &[u8],
        request: u8,
        deadline: u64,
    ) -> Result<(), ProbeErrorKind> {
        let mut offset = 0;
        while offset < bytes.len() {
            if let Err(error) = self.check(deadline) {
                if offset > 0 {
                    self.uncertain_write = true;
                }
                return Err(error);
            }
            match self
                .transport
                .write(&bytes[offset..], deadline, self.cancelled)
            {
                Ok(0) => {
                    self.uncertain_write = true;
                    return Err(ProbeErrorKind::ShortWrite);
                }
                Ok(count) if count <= bytes.len() - offset => {
                    offset += count;
                    self.written = self.written.saturating_add(count as u32);
                    let progress = match request {
                        0 => Some(&mut self.progress.record.upgrade_request),
                        1 => Some(&mut self.progress.record.identity_request),
                        2 => Some(&mut self.progress.record.firmware_request),
                        3 => Some(&mut self.progress.record.close_request),
                        _ => None,
                    };
                    if let Some(progress) = progress {
                        progress.written_bytes =
                            progress.written_bytes.saturating_add(count as u32);
                    }
                }
                Ok(_) => {
                    self.uncertain_write = true;
                    return Err(ProbeErrorKind::TransportContract);
                }
                Err(error) => {
                    self.uncertain_write = true;
                    self.native_code = error.native_code;
                    return Err(error.kind);
                }
            }
        }
        Ok(())
    }

    fn read_more(&mut self, deadline: u64) -> Result<(), ProbeErrorKind> {
        let now = self.check(deadline)?;
        let remaining = (self.options.max_received_bytes - self.received) as usize;
        if remaining == 0 {
            return Err(ProbeErrorKind::ByteLimit);
        }
        let capacity = remaining.min(512);
        let mut buffer = [0_u8; 512];
        match self
            .transport
            .read(&mut buffer[..capacity], deadline, self.cancelled)
        {
            Ok(ReadResult::Data(0)) => Err(ProbeErrorKind::TransportContract),
            Ok(ReadResult::Data(count)) if count <= capacity => {
                let after = self.now()?;
                self.received = self.received.saturating_add(count as u32);
                self.progress.record.received_bytes = self.received;
                if self.stage == Stage::Handshake && !self.upgraded {
                    self.progress.record.upgrade_response_bytes = self
                        .progress
                        .record
                        .upgrade_response_bytes
                        .saturating_add(count as u32);
                }
                if self.cancelled.load(Ordering::Acquire) {
                    return Err(ProbeErrorKind::Cancelled);
                }
                if after >= deadline {
                    return Err(ProbeErrorKind::Timeout);
                }
                let _ = now;
                self.pending.extend_from_slice(&buffer[..count]);
                Ok(())
            }
            Ok(ReadResult::Data(_)) => Err(ProbeErrorKind::TransportContract),
            Ok(ReadResult::Idle) => {
                let after = self.now()?;
                if self.cancelled.load(Ordering::Acquire) {
                    return Err(ProbeErrorKind::Cancelled);
                }
                if after <= now {
                    return Err(ProbeErrorKind::TransportContract);
                }
                if after >= deadline {
                    return Err(ProbeErrorKind::Timeout);
                }
                Ok(())
            }
            Ok(ReadResult::Disconnected) => {
                self.frame_decoder
                    .eof()
                    .map_err(|_| ProbeErrorKind::TruncatedFrame)?;
                Err(ProbeErrorKind::Disconnected)
            }
            Err(error) => {
                self.native_code = error.native_code;
                Err(error.kind)
            }
        }
    }

    fn io_deadline(&mut self) -> Result<u64, ProbeErrorKind> {
        let now = self.now()?;
        Ok(now
            .saturating_add(u64::from(self.options.io_timeout_ms))
            .min(self.deadline))
    }

    fn upgrade(&mut self) -> Result<(), ProbeErrorKind> {
        self.stage = Stage::Handshake;
        let now = self.now()?;
        self.progress.enter(Phase::UpgradeWrite, self.elapsed(now));
        let deadline = self.io_deadline()?;
        self.write_all(UPGRADE, 0, deadline)?;
        let now = self.now()?;
        self.progress
            .enter(Phase::UpgradeResponse, self.elapsed(now));
        let mut header = Vec::with_capacity(256);
        loop {
            while !self.pending.is_empty() {
                let byte = self.pending.remove(0);
                if header.is_empty() {
                    self.progress.record.handshake_response_start = Some(
                        match byte {
                            0x82 => "web_socket_binary_frame",
                            0x88 => "web_socket_close_frame",
                            0x20..=0x7e => "text",
                            _ => "other_binary",
                        }
                        .to_owned(),
                    );
                    match byte {
                        0x82 => return Err(ProbeErrorKind::SuspectedStaleSession),
                        0x88 => return Err(ProbeErrorKind::SuspectedStaleSession),
                        0x20..=0x7e | b'\r' | b'\n' | b'\t' => {}
                        _ => return Err(ProbeErrorKind::InvalidHandshake),
                    }
                }
                header.push(byte);
                self.progress.record.header_bytes = header.len() as u32;
                if header.len() > 1_024 {
                    return Err(ProbeErrorKind::InvalidHandshake);
                }
                if let Some(end) = header_end(&header) {
                    self.progress.record.header_complete = true;
                    if self.pending.is_empty() {
                        // The suffix can arrive in the same read; preserve it below.
                    }
                    let suffix = header.split_off(end);
                    self.pending.splice(0..0, suffix);
                    self.stage = Stage::Handshake;
                    let now = self.now()?;
                    self.progress
                        .enter(Phase::HeaderValidation, self.elapsed(now));
                    validate_header(&header)?;
                    self.progress.record.header_validated = true;
                    self.upgraded = true;
                    return Ok(());
                }
                if !valid_header_byte(byte) {
                    return Err(ProbeErrorKind::InvalidHandshake);
                }
            }
            self.read_more(deadline)?;
        }
    }

    fn next_payload(&mut self, deadline: u64) -> Result<Vec<u8>, ProbeErrorKind> {
        loop {
            if !self.pending.is_empty() {
                let step = self
                    .frame_decoder
                    .decode(&self.pending)
                    .map_err(|_| ProbeErrorKind::MalformedFrame)?;
                self.pending.drain(..step.consumed);
                if let Some(payload) = step.frame {
                    self.progress.record.frames_received =
                        self.progress.record.frames_received.saturating_add(1);
                    return Ok(payload);
                }
            }
            self.read_more(deadline)?;
        }
    }

    fn handle_interleaved(&mut self, payload: &[u8]) -> Result<(), ProbeErrorKind> {
        let event = decode_input(payload).map_err(|_| ProbeErrorKind::InvalidInput)?;
        match event {
            InputEvent::Tick { .. } => self.ticks = self.ticks.saturating_add(1),
            _ => {
                if self.input_samples.len() >= usize::from(self.options.max_events) {
                    return Err(ProbeErrorKind::InputLimit);
                }
                let now = self.now()?;
                self.input_samples.push(InputSample {
                    elapsed_ms: now.min(u64::from(u32::MAX)) as u32,
                    payload: payload.to_vec(),
                });
            }
        }
        Ok(())
    }

    fn query(
        &mut self,
        opcode: u8,
        transaction: u8,
        stage: Stage,
        write_phase: Phase,
        response_phase: Phase,
    ) -> Result<Vec<u8>, ProbeErrorKind> {
        self.stage = stage;
        let message = [opcode, transaction];
        let frame = encode_command(&message);
        let now = self.now()?;
        self.progress.enter(write_phase, self.elapsed(now));
        let deadline = self.io_deadline()?;
        self.write_all(&frame, if opcode == 3 { 1 } else { 2 }, deadline)?;
        let now = self.now()?;
        self.progress.enter(response_phase, self.elapsed(now));
        let reply_deadline = deadline;
        loop {
            let payload = self.next_payload(reply_deadline)?;
            if payload.len() < 3 {
                return Err(ProbeErrorKind::InvalidReply);
            }
            let inner_len = usize::from(payload[0]);
            let reply_opcode = payload[1];
            let reply_transaction = payload[2];
            if reply_transaction == transaction && reply_opcode != opcode {
                return Err(ProbeErrorKind::UnexpectedReply);
            }
            if matches!(reply_opcode, 0x03 | 0x07 | 0x10 | 0x0f) {
                if reply_transaction != transaction || reply_opcode != opcode {
                    return Err(ProbeErrorKind::UnexpectedReply);
                }
                if inner_len > payload.len() {
                    self.disagreement_count = self.disagreement_count.saturating_add(1);
                } else if inner_len < payload.len() {
                    return Err(ProbeErrorKind::InvalidReply);
                }
                if opcode == 0x03 {
                    self.progress.record.identity_frames_received = self
                        .progress
                        .record
                        .identity_frames_received
                        .saturating_add(1);
                } else if opcode == 0x07 {
                    self.progress.record.firmware_frames_received = self
                        .progress
                        .record
                        .firmware_frames_received
                        .saturating_add(1);
                }
                return Ok(payload);
            }
            self.handle_interleaved(&payload)?;
        }
    }

    fn draw_pattern(&mut self) -> Result<(), ProbeErrorKind> {
        for (opcode, transaction, display) in [
            (0x10, 3, b'A'),
            (0x0f, 4, b'A'),
            (0x10, 5, b'W'),
            (0x0f, 6, b'W'),
        ] {
            let mut body = Vec::new();
            if opcode == 0x10 {
                body.extend_from_slice(&[0, display, 0, 0, 0, 0, 0, 48, 0, 48]);
                body.extend_from_slice(&pattern_pixels(display));
            } else {
                body.extend_from_slice(&[0, display]);
            }
            let mut message = vec![opcode, transaction];
            message.extend_from_slice(&body);
            let frame = encode_command(&message);
            self.stage = Stage::Pattern;
            let now = self.now()?;
            self.progress.enter(Phase::Pattern, self.elapsed(now));
            let deadline = self.io_deadline()?;
            self.write_all(&frame, 4, deadline)?;
            let response_deadline = deadline;
            loop {
                let payload = self.next_payload(response_deadline)?;
                if payload.len() < 3 {
                    return Err(ProbeErrorKind::InvalidReply);
                }
                if matches!(payload[1], 0x03 | 0x07 | 0x10 | 0x0f) {
                    if payload[1] != opcode || payload[2] != transaction {
                        return Err(ProbeErrorKind::UnexpectedReply);
                    }
                    if payload.len() != 3 {
                        return Err(ProbeErrorKind::InvalidReply);
                    }
                    let declared = usize::from(payload[0]);
                    if declared < payload.len() {
                        return Err(ProbeErrorKind::InvalidReply);
                    }
                    if declared > payload.len() {
                        self.disagreement_count = self.disagreement_count.saturating_add(1);
                    }
                    break;
                }
                if payload[2] == transaction {
                    return Err(ProbeErrorKind::UnexpectedReply);
                }
                self.handle_interleaved(&payload)?;
            }
        }
        Ok(())
    }
}

/// Run a probe against an injected transport. No port selection is accepted here.
pub fn run<T: Transport>(
    options: Options,
    transport: T,
    cancelled: &AtomicBool,
) -> Result<ProbeReport, ProbeFailure> {
    run_with_evidence(options, transport, cancelled, Evidence::Synthetic)
}

fn run_with_evidence<T: Transport>(
    options: Options,
    mut transport: T,
    cancelled: &AtomicBool,
    evidence: Evidence,
) -> Result<ProbeReport, ProbeFailure> {
    let mut progress = Progress::new(evidence);
    if let Err(kind) = options.validate() {
        return Err(ProbeFailure {
            kind,
            stage: Stage::Options,
            native_code: None,
            progress: Box::new(progress.record),
        });
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(ProbeFailure {
            kind: ProbeErrorKind::Cancelled,
            stage: Stage::Open,
            native_code: None,
            progress: Box::new(progress.record),
        });
    }
    let start = transport.now_ms();
    let started_at = transport.progress_origin_ms().unwrap_or(start);
    progress.record.port_opened_at_ms = transport.port_opened_at_ms();
    progress.record.port_configured_at_ms = transport.port_configured_at_ms();
    progress.phase_started = started_at;
    progress.seed(transport.initial_phases());
    let deadline = start.saturating_add(u64::from(options.total_timeout_ms));
    let mut engine = Engine {
        transport,
        options,
        cancelled,
        started_at,
        clock: Clock { last: start },
        progress,
        stage: Stage::Handshake,
        deadline,
        received: 0,
        written: 0,
        disagreement_count: 0,
        input_samples: Vec::new(),
        ticks: 0,
        frame_decoder: FrameDecoder::default(),
        pending: Vec::new(),
        upgraded: false,
        uncertain_write: false,
        close_attempted: false,
        native_code: None,
    };

    let mut result = (|| {
        engine.upgrade()?;
        let identity = engine.query(
            0x03,
            1,
            Stage::Identity,
            Phase::IdentityWrite,
            Phase::IdentityResponse,
        )?;
        let identity_body = &identity[3..];
        if !(1..=64).contains(&identity_body.len())
            || identity_body
                .iter()
                .any(|byte| !(0x20..=0x7e).contains(byte))
            || identity_body.iter().all(|byte| *byte == b' ')
        {
            return Err(ProbeErrorKind::InvalidReply);
        }
        let firmware = engine.query(
            0x07,
            2,
            Stage::Firmware,
            Phase::FirmwareWrite,
            Phase::FirmwareResponse,
        )?;
        if !(3..=32).contains(&(firmware.len() - 3)) {
            return Err(ProbeErrorKind::InvalidReply);
        }
        let version = [firmware[3], firmware[4], firmware[5]];
        if engine.options.test_pattern {
            engine.draw_pattern()?;
        }
        engine.stage = Stage::Capture;
        let capture_start = engine.check(engine.deadline)?;
        let capture_deadline = capture_start.saturating_add(u64::from(engine.options.capture_ms));
        if capture_deadline > engine.deadline {
            return Err(ProbeErrorKind::Timeout);
        }
        engine
            .progress
            .enter(Phase::Capture, engine.elapsed(capture_start));
        let completion = loop {
            let now = match engine.check(capture_deadline) {
                Ok(now) => now,
                Err(ProbeErrorKind::Timeout) => {
                    engine
                        .frame_decoder
                        .eof()
                        .map_err(|_| ProbeErrorKind::TruncatedFrame)?;
                    break Completion::CaptureDurationElapsed;
                }
                Err(error) => return Err(error),
            };
            if engine.input_samples.len() >= usize::from(engine.options.max_events) {
                break Completion::InputLimitReached;
            }
            match engine.next_payload(capture_deadline) {
                Ok(payload) => engine.handle_interleaved(&payload)?,
                Err(ProbeErrorKind::Timeout) => {
                    engine
                        .frame_decoder
                        .eof()
                        .map_err(|_| ProbeErrorKind::TruncatedFrame)?;
                    break Completion::CaptureDurationElapsed;
                }
                Err(error) => return Err(error),
            }
            let _ = now;
        };
        let elapsed = engine.now()?.saturating_sub(start).min(u64::from(u32::MAX)) as u32;
        Ok((version, identity_body.len() as u8, completion, elapsed))
    })();

    let cleanup_now = engine.now();
    if result.is_ok() {
        if cancelled.load(Ordering::Acquire) {
            engine.stage = Stage::Cleanup;
            result = Err(ProbeErrorKind::Cancelled);
        } else if let Err(error) = cleanup_now {
            result = Err(error);
        }
    }
    let primary_native_code = engine.native_code;
    let now = cleanup_now.unwrap_or(engine.clock.last);
    engine.progress.tick(engine.elapsed(now));
    engine.progress.record.serial = engine.transport.serial_diagnostics();
    if result.is_err() && engine.upgraded {
        let skip = engine.uncertain_write
            || cancelled.load(Ordering::Acquire)
            || cleanup_now.is_err()
            || matches!(
                &result,
                Err(ProbeErrorKind::Disconnected
                    | ProbeErrorKind::TruncatedFrame
                    | ProbeErrorKind::ClockRegression
                    | ProbeErrorKind::CleanupPending
                    | ProbeErrorKind::TransportContract)
            );
        if skip {
            engine.progress.record.session_close = CloseStatus::Skipped;
        } else {
            engine.close_attempted = true;
            let close_deadline =
                now.saturating_add(u64::from(engine.options.io_timeout_ms.min(250)));
            let close_result = engine.write_all(CLOSE, 3, close_deadline);
            engine.progress.record.session_close = if close_result.is_ok() {
                CloseStatus::Sent
            } else {
                CloseStatus::WriteFailed
            };
        }
    } else if result.is_ok() && engine.upgraded {
        if cancelled.load(Ordering::Acquire) {
            engine.progress.record.session_close = CloseStatus::Skipped;
        } else {
            engine.close_attempted = true;
            let close_deadline =
                now.saturating_add(u64::from(engine.options.io_timeout_ms.min(250)));
            let close_result = engine.write_all(CLOSE, 3, close_deadline);
            engine.progress.record.session_close = if close_result.is_ok() {
                CloseStatus::Sent
            } else {
                CloseStatus::WriteFailed
            };
            if matches!(close_result, Err(ProbeErrorKind::Cancelled))
                || cancelled.load(Ordering::Acquire)
            {
                engine.stage = Stage::Cleanup;
                result = Err(ProbeErrorKind::Cancelled);
            }
        }
    }
    let outcome = match result {
        Ok((version, identity_length, completion, elapsed)) => Ok(ProbeReport {
            schema_version: 1,
            evidence,
            hardware_qualification: "unqualified".into(),
            physical_input_qualification: "unqualified".into(),
            visual_acceptance: "unqualified".into(),
            http_101: true,
            identity_response_length: identity_length,
            firmware_version: version,
            reply_length_disagreements: engine.disagreement_count,
            serial_settings: engine.transport.serial_settings(),
            pattern: if engine.options.test_pattern {
                "correlated_replies_only".into()
            } else {
                "not_requested".into()
            },
            completion,
            elapsed_ms: elapsed,
            received_bytes: engine.received,
            written_bytes: engine.written,
            close: engine.progress.record.session_close,
            ticks: engine.ticks,
            inputs: engine.input_samples,
            progress: engine.progress.record,
        }),
        Err(kind) => Err(ProbeFailure {
            kind,
            stage: engine.stage,
            native_code: primary_native_code,
            progress: Box::new(engine.progress.record),
        }),
    };
    let _ = engine.close_attempted;
    outcome
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        collections::VecDeque,
        rc::Rc,
        sync::{Arc, atomic::AtomicBool},
    };

    struct Fake {
        now: u64,
        reads: VecDeque<Vec<u8>>,
        writes: Rc<RefCell<Vec<Vec<u8>>>>,
        max_write: usize,
        write_calls: usize,
        fail_write_at: Option<usize>,
        idle_step: u64,
        clock_calls: usize,
        regress_at_call: Option<usize>,
        read_calls: usize,
        cancel_on_read_at: Option<usize>,
        cancel_on_write_at: Option<usize>,
        cancel_flag: Option<Arc<AtomicBool>>,
    }

    impl Fake {
        fn new(reads: Vec<Vec<u8>>) -> Self {
            Self {
                now: 10,
                reads: reads.into(),
                writes: Rc::new(RefCell::new(Vec::new())),
                max_write: usize::MAX,
                write_calls: 0,
                fail_write_at: None,
                idle_step: 1,
                clock_calls: 0,
                regress_at_call: None,
                read_calls: 0,
                cancel_on_read_at: None,
                cancel_on_write_at: None,
                cancel_flag: None,
            }
        }
    }

    impl Transport for Fake {
        fn now_ms(&mut self) -> u64 {
            self.clock_calls += 1;
            if self.regress_at_call == Some(self.clock_calls) {
                self.regress_at_call = None;
                self.now = self.now.saturating_sub(1);
            }
            self.now
        }

        fn write(
            &mut self,
            bytes: &[u8],
            _deadline_ms: u64,
            _cancelled: &AtomicBool,
        ) -> Result<usize, TransportError> {
            self.write_calls += 1;
            if self.fail_write_at == Some(self.write_calls) {
                return Err(TransportError {
                    kind: ProbeErrorKind::NativeFailure,
                    native_code: Some(5),
                });
            }
            if self.cancel_on_write_at == Some(self.write_calls)
                && let Some(flag) = &self.cancel_flag
            {
                flag.store(true, Ordering::Release);
            }
            let count = bytes.len().min(self.max_write);
            self.writes.borrow_mut().push(bytes[..count].to_vec());
            Ok(count)
        }

        fn read(
            &mut self,
            bytes: &mut [u8],
            _deadline_ms: u64,
            _cancelled: &AtomicBool,
        ) -> Result<ReadResult, TransportError> {
            self.read_calls += 1;
            if let Some(front) = self.reads.front_mut() {
                let count = front.len().min(bytes.len());
                bytes[..count].copy_from_slice(&front[..count]);
                front.drain(..count);
                if front.is_empty() {
                    self.reads.pop_front();
                }
                if self.cancel_on_read_at == Some(self.read_calls)
                    && let Some(flag) = &self.cancel_flag
                {
                    flag.store(true, Ordering::Release);
                }
                return Ok(ReadResult::Data(count));
            }
            self.now = self.now.saturating_add(self.idle_step);
            Ok(ReadResult::Idle)
        }
    }

    fn frame(payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() <= 255);
        let mut bytes = vec![0x82, payload.len() as u8];
        bytes.extend_from_slice(payload);
        bytes
    }

    fn selection(
        state: SerialSelectionState,
        present_count: usize,
        ports: &[&str],
        selected: Option<&str>,
    ) -> SerialSelection {
        SerialSelection {
            state,
            present_count,
            matching_count: ports.len(),
            ports: ports.iter().map(|port| (*port).to_owned()).collect(),
            selected_port: selected.map(str::to_owned),
        }
    }

    #[test]
    fn ct_port_selection_requires_one_present_normalized_association() {
        assert_eq!(
            select_ct_port(&selection(
                SerialSelectionState::Unique,
                1,
                &["com7"],
                Some("COM7")
            ))
            .unwrap(),
            "COM7"
        );
        assert_eq!(
            select_ct_port(&selection(SerialSelectionState::NotFound, 1, &[], None)),
            Err(ProbeErrorKind::DeviceUnavailable)
        );
        assert_eq!(
            select_ct_port(&selection(
                SerialSelectionState::Unique,
                1,
                &["COM7"],
                Some("COM8")
            )),
            Err(ProbeErrorKind::DiscoveryIncomplete)
        );
        assert_eq!(
            select_ct_port(&selection(
                SerialSelectionState::Unique,
                0,
                &["COM7"],
                Some("COM7")
            )),
            Err(ProbeErrorKind::DiscoveryIncomplete)
        );
        assert_eq!(
            select_ct_port(&selection(
                SerialSelectionState::Unique,
                1,
                &["COM0"],
                Some("COM0")
            )),
            Err(ProbeErrorKind::DiscoveryIncomplete)
        );
    }

    #[test]
    fn modem_lines_accept_only_static_off_or_on_modes() {
        assert_eq!(
            static_modem_modes(0),
            Some((StaticLineMode::Off, StaticLineMode::Off))
        );
        assert_eq!(
            static_modem_modes(0x1010),
            Some((StaticLineMode::On, StaticLineMode::On))
        );
        assert_eq!(static_modem_modes(0x20), None);
        assert_eq!(static_modem_modes(0x2000), None);
    }

    #[test]
    fn native_error_mapping_is_typed_and_redacted() {
        assert_eq!(win32_error_kind(5), ProbeErrorKind::BusyOrDenied);
        assert_eq!(win32_error_kind(32), ProbeErrorKind::BusyOrDenied);
        assert_eq!(win32_error_kind(1167), ProbeErrorKind::Disconnected);
        assert_eq!(win32_error_kind(12345), ProbeErrorKind::NativeFailure);
    }

    fn valid_reads() -> Vec<Vec<u8>> {
        vec![
            b"HTTP/1.1 101 Switching Protocols\nUpgrade: websocket\nConnection: keep-alive, Upgrade\n\n".to_vec(),
            frame(&[4, 3, 1, b'C']),
            frame(&[6, 7, 2, 1, 2, 3]),
        ]
    }

    fn run_fake(
        options: Options,
        fake: Fake,
        cancelled: &AtomicBool,
    ) -> Result<ProbeReport, ProbeFailure> {
        run(options, fake, cancelled)
    }

    #[test]
    fn handshake_identity_firmware_and_close() {
        let result = run_fake(
            Options {
                capture_ms: 0,
                ..Options::default()
            },
            Fake::new(valid_reads()),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(result.http_101);
        assert_eq!(result.identity_response_length, 1);
        assert_eq!(result.firmware_version, [1, 2, 3]);
        assert_eq!(result.close, CloseStatus::Sent);
        assert_eq!(
            result.progress.upgrade_request.written_bytes,
            UPGRADE.len() as u32
        );
        assert_eq!(result.progress.identity_request.written_bytes, 9);
        assert_eq!(result.progress.firmware_request.written_bytes, 9);
    }

    #[test]
    fn options_and_cancellation_fail_before_io() {
        let invalid = Options {
            max_received_bytes: 255,
            ..Options::default()
        };
        let mut fake = Fake::new(vec![]);
        let failure = run_fake(invalid, fake, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(failure.kind, ProbeErrorKind::InvalidOptions);
        assert_eq!(failure.stage, Stage::Options);

        let cancelled = AtomicBool::new(true);
        fake = Fake::new(vec![]);
        assert_eq!(
            run_fake(Options::default(), fake, &cancelled)
                .unwrap_err()
                .kind,
            ProbeErrorKind::Cancelled
        );
    }

    #[test]
    fn request_short_writes_continue_on_exact_suffix() {
        let mut fake = Fake::new(valid_reads());
        fake.max_write = 3;
        let writes = Rc::clone(&fake.writes);
        run_fake(
            Options {
                capture_ms: 0,
                ..Options::default()
            },
            fake,
            &AtomicBool::new(false),
        )
        .unwrap();
        let writes: Vec<u8> = writes.borrow().iter().flatten().copied().collect();
        assert_eq!(&writes[..UPGRADE.len()], UPGRADE);
        assert_eq!(
            &writes[UPGRADE.len()..UPGRADE.len() + 9],
            &[0x82, 0x83, 0, 0, 0, 0, 3, 0x03, 1]
        );
        assert_eq!(&writes[writes.len() - CLOSE.len()..], CLOSE);
    }

    #[test]
    fn short_write_and_native_failure_do_not_close_or_replace_primary_error() {
        let mut fake = Fake::new(valid_reads());
        fake.max_write = 0;
        let failure = run_fake(Options::default(), fake, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(failure.kind, ProbeErrorKind::ShortWrite);
        assert_eq!(failure.progress.session_close, CloseStatus::NotAttempted);

        let mut reads = valid_reads();
        reads[1] = frame(&[4, 3, 1, b' ']);
        let mut fake = Fake::new(reads);
        fake.fail_write_at = Some(3);
        let failure = run_fake(
            Options {
                capture_ms: 0,
                ..Options::default()
            },
            fake,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(failure.kind, ProbeErrorKind::InvalidReply);
        assert_eq!(failure.progress.session_close, CloseStatus::WriteFailed);
    }

    #[test]
    fn stale_session_stops_without_another_request() {
        let fake = Fake::new(vec![vec![0x88, 0x80, 0, 0, 0, 0]]);
        let failure = run_fake(Options::default(), fake, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(failure.kind, ProbeErrorKind::SuspectedStaleSession);
        assert_eq!(failure.progress.session_close, CloseStatus::NotAttempted);
        assert_eq!(
            failure.progress.upgrade_request.written_bytes,
            UPGRADE.len() as u32
        );
    }

    #[test]
    fn byte_limit_malformed_input_and_deadline_are_bounded() {
        let mut reads = valid_reads();
        let mut ticks = Vec::new();
        for _ in 0..32 {
            ticks.extend(frame(&[4, 0, 1, 8]));
        }
        reads.push(ticks);
        let failure = run_fake(
            Options {
                max_received_bytes: 256,
                capture_ms: 1_000,
                ..Options::default()
            },
            Fake::new(reads),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(failure.kind, ProbeErrorKind::ByteLimit);

        let mut reads = valid_reads();
        reads[2] = frame(&[4, 7, 2, 1]);
        let failure = run_fake(
            Options {
                capture_ms: 0,
                ..Options::default()
            },
            Fake::new(reads),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(failure.kind, ProbeErrorKind::InvalidReply);

        let mut frozen = Fake::new(vec![]);
        frozen.idle_step = 0;
        assert_eq!(
            run_fake(Options::default(), frozen, &AtomicBool::new(false))
                .unwrap_err()
                .kind,
            ProbeErrorKind::TransportContract
        );
        let timeout = run_fake(
            Options::default(),
            Fake::new(vec![]),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(timeout.kind, ProbeErrorKind::Timeout);
    }

    #[test]
    fn capture_records_input_and_event_limit_completion() {
        let mut reads = valid_reads();
        reads.push(frame(&[4, 0, 9, 0]));
        reads.push(frame(&[5, 0, 2, 7, 0]));
        let report = run_fake(
            Options {
                capture_ms: 20,
                max_events: 1,
                ..Options::default()
            },
            Fake::new(reads),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(report.completion, Completion::InputLimitReached);
        assert_eq!(report.ticks, 1);
        assert_eq!(report.inputs.len(), 1);
        assert_eq!(report.inputs[0].payload, [5, 0, 2, 7, 0]);
    }

    #[test]
    fn clock_regression_and_close_failure_keep_primary_outcome() {
        let mut fake = Fake::new(valid_reads());
        fake.regress_at_call = Some(2);
        let failure = run_fake(Options::default(), fake, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(failure.kind, ProbeErrorKind::ClockRegression);

        let mut fake = Fake::new(valid_reads());
        fake.fail_write_at = Some(4);
        let report = run_fake(
            Options {
                capture_ms: 0,
                ..Options::default()
            },
            fake,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(report.close, CloseStatus::WriteFailed);
    }

    #[test]
    fn pattern_frame_matches_required_layout_and_long_prefix() {
        let pixels = pattern_pixels(b'A');
        assert_eq!(pixels.len(), 48 * 48 * 2);
        assert_eq!(&pixels[..2], &0xffff_u16.to_le_bytes());
        assert_eq!(
            &pixels[2 * (1 + 48)..2 * (1 + 48) + 2],
            &0xf800_u16.to_le_bytes()
        );
        assert_eq!(
            &pixels[2 * (24 + 48)..2 * (24 + 48) + 2],
            &0x07e0_u16.to_le_bytes()
        );
        let encoded = encode_command(&[0; 4_620]);
        assert_eq!(
            &encoded[..14],
            &[0x82, 0xff, 0, 0, 0, 0, 0, 0, 0x12, 0x0d, 0, 0, 0, 0]
        );
        assert_eq!(encoded.len(), 4_635);
        assert_eq!(
            encode_command(&[0x03, 0x01]),
            [0x82, 0x83, 0, 0, 0, 0, 3, 0x03, 0x01]
        );
        assert_eq!(
            &pattern_pixels(b'W')[2 * (1 + 48)..2 * (1 + 48) + 2],
            &0xf800_u16.to_be_bytes()
        );
    }

    #[test]
    fn optional_pattern_awaits_all_correlated_draw_and_refresh_replies() {
        let mut reads = valid_reads();
        for (opcode, transaction) in [(0x10, 3), (0x0f, 4), (0x10, 5), (0x0f, 6)] {
            reads.push(frame(&[3, opcode, transaction]));
        }
        let report = run_fake(
            Options {
                test_pattern: true,
                capture_ms: 0,
                ..Options::default()
            },
            Fake::new(reads),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(report.pattern, "correlated_replies_only");
        assert_eq!(report.progress.frames_received, 6);
        assert_eq!(report.progress.firmware_request.written_bytes, 9);
    }

    #[test]
    fn correlated_pattern_reply_checks_inner_length_and_counts_disagreement() {
        let mut reads = valid_reads();
        reads.extend([
            frame(&[4, 0x10, 3]),
            frame(&[3, 0x0f, 4]),
            frame(&[3, 0x10, 5]),
            frame(&[3, 0x0f, 6]),
        ]);
        let report = run_fake(
            Options {
                test_pattern: true,
                capture_ms: 0,
                ..Options::default()
            },
            Fake::new(reads),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(report.reply_length_disagreements, 1);

        let mut reads = valid_reads();
        reads.push(frame(&[2, 0x10, 3]));
        let failure = run_fake(
            Options {
                test_pattern: true,
                capture_ms: 0,
                ..Options::default()
            },
            Fake::new(reads),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(failure.kind, ProbeErrorKind::InvalidReply);
    }

    #[test]
    fn cancellation_skips_close_and_cancellation_during_close_is_returned() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut fake = Fake::new(valid_reads());
        fake.cancel_flag = Some(Arc::clone(&cancelled));
        fake.cancel_on_read_at = Some(3);
        let failure = run_fake(
            Options {
                capture_ms: 0,
                ..Options::default()
            },
            fake,
            &cancelled,
        )
        .unwrap_err();
        assert_eq!(failure.kind, ProbeErrorKind::Cancelled);
        assert_eq!(failure.progress.session_close, CloseStatus::Skipped);

        let cancelled = Arc::new(AtomicBool::new(false));
        let mut fake = Fake::new(valid_reads());
        fake.cancel_flag = Some(Arc::clone(&cancelled));
        fake.cancel_on_write_at = Some(4);
        let failure = run_fake(
            Options {
                capture_ms: 0,
                ..Options::default()
            },
            fake,
            &cancelled,
        )
        .unwrap_err();
        assert_eq!(failure.kind, ProbeErrorKind::Cancelled);
        assert_eq!(failure.stage, Stage::Cleanup);
        assert_eq!(failure.progress.session_close, CloseStatus::Sent);
    }

    #[test]
    fn header_validation_rejects_duplicate_and_missing_upgrade_fields() {
        assert_eq!(
            validate_header(
                b"HTTP/1.1 101 x\nUpgrade: websocket\nUpgrade: websocket\nConnection: Upgrade\n\n"
            ),
            Err(ProbeErrorKind::InvalidHandshake)
        );
        assert_eq!(
            validate_header(b"HTTP/1.1 101 x\nUpgrade: websocket\nConnection: keep-alive\n\n"),
            Err(ProbeErrorKind::InvalidHandshake)
        );
    }

    #[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
    #[test]
    fn native_entry_point_is_typed_unsupported_without_discovery() {
        let failure = run_native(Options::default(), &AtomicBool::new(false)).unwrap_err();
        assert_eq!(failure.kind, ProbeErrorKind::UnsupportedPlatform);
        assert_eq!(failure.stage, Stage::Discovery);
    }
}

fn header_end(bytes: &[u8]) -> Option<usize> {
    if let Some(index) = bytes.windows(2).position(|window| window == b"\n\n") {
        return Some(index + 2);
    }
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn valid_header_byte(byte: u8) -> bool {
    (0x20..=0x7e).contains(&byte) || matches!(byte, b'\t' | b'\r' | b'\n')
}

fn validate_header(header: &[u8]) -> Result<(), ProbeErrorKind> {
    if header.iter().any(|byte| !valid_header_byte(*byte)) {
        return Err(ProbeErrorKind::InvalidHandshake);
    }
    let text = std::str::from_utf8(header).map_err(|_| ProbeErrorKind::InvalidHandshake)?;
    let normalized = text.replace("\r\n", "\n");
    let mut lines = normalized.split('\n');
    let status = lines.next().ok_or(ProbeErrorKind::InvalidHandshake)?;
    let mut tokens = status.split_ascii_whitespace();
    if tokens.next() != Some("HTTP/1.1") || tokens.next() != Some("101") {
        return Err(ProbeErrorKind::InvalidHandshake);
    }
    let mut upgrades = 0;
    let mut connections = 0;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or(ProbeErrorKind::InvalidHandshake)?;
        if name.eq_ignore_ascii_case("upgrade") {
            upgrades += 1;
            if !value.trim().eq_ignore_ascii_case("websocket") {
                return Err(ProbeErrorKind::InvalidHandshake);
            }
        } else if name.eq_ignore_ascii_case("connection") {
            connections += 1;
            if !value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            {
                return Err(ProbeErrorKind::InvalidHandshake);
            }
        }
    }
    if upgrades == 1 && connections == 1 {
        Ok(())
    } else {
        Err(ProbeErrorKind::InvalidHandshake)
    }
}

fn encode_command(command: &[u8]) -> Vec<u8> {
    let length = command.len() + 1;
    if length <= 255 {
        let mut frame = Vec::with_capacity(length + 6);
        frame.extend_from_slice(&[0x82, (0x80_u8).wrapping_add(length as u8), 0, 0, 0, 0]);
        frame.push(length as u8);
        frame.extend_from_slice(command);
        frame
    } else {
        let mut frame = Vec::with_capacity(length + 14);
        frame.extend_from_slice(&[0x82, 0xff, 0, 0, 0, 0]);
        frame.extend_from_slice(&(length as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0, 0, 0]);
        frame.push(0xff);
        frame.extend_from_slice(command);
        frame
    }
}

fn pattern_pixels(display: u8) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(48 * 48 * 2);
    for y in 0..48 {
        for x in 0..48 {
            let color = if x == 0 || y == 0 || x == 47 || y == 47 {
                0xffff_u16
            } else {
                match (x >= 24, y >= 24) {
                    (false, false) => 0xf800,
                    (true, false) => 0x07e0,
                    (false, true) => 0x001f,
                    (true, true) => 0,
                }
            };
            let bytes = if display == b'W' {
                color.to_be_bytes()
            } else {
                color.to_le_bytes()
            };
            pixels.extend_from_slice(&bytes);
        }
    }
    pixels
}

#[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
pub fn run_native(_options: Options, _cancelled: &AtomicBool) -> Result<ProbeReport, ProbeFailure> {
    let progress = Progress::new(Evidence::NativeTransportObservation).record;
    if let Err(kind) = _options.validate() {
        return Err(ProbeFailure {
            kind,
            stage: Stage::Options,
            native_code: None,
            progress: Box::new(progress),
        });
    }

    if _cancelled.load(Ordering::Acquire) {
        return Err(ProbeFailure {
            kind: ProbeErrorKind::Cancelled,
            stage: Stage::Open,
            native_code: None,
            progress: Box::new(progress),
        });
    }
    Err(ProbeFailure {
        kind: ProbeErrorKind::UnsupportedPlatform,
        stage: Stage::Discovery,
        native_code: None,
        progress: Box::new(progress),
    })
}

#[cfg(windows)]
mod windows_native;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod macos_native;

#[cfg(windows)]
pub(crate) fn run_native(
    options: Options,
    cancelled: &AtomicBool,
) -> Result<ProbeReport, ProbeFailure> {
    windows_native::run_native(options, cancelled)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) fn run_native(
    options: Options,
    cancelled: &AtomicBool,
) -> Result<ProbeReport, ProbeFailure> {
    macos_native::run_native(options, cancelled)
}
