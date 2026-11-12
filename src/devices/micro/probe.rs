use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::native::hid::{
    EvidenceKind, HidBackend, HidBus, HidDescriptor, HidDevice, HidError, HidIdentity, HidMatcher,
    enumerate,
};

use super::{
    calibration::Calibration,
    enumeration::{APPLICATION_PID, APPLICATION_VID, REPORT_SIZE, USAGE, USAGE_PAGE},
    error::MicroError,
    input::{Bindings, LegacyContext, LegacyDecoder, Slot},
    led::{diagnostic_frame, shutdown_frame},
    rpc::{RpcClient, validate_firmware_version},
    wire::{JSON_CHANNEL, LOG_CHANNEL, REPORT_ID},
};

const READ_SIZE: usize = REPORT_SIZE + 1;
const READ_BUDGET: usize = 512;
const CAPTURE_BUDGET: usize = 128;
const INPUT_BUDGET: usize = 128;
const READ_SLICE: Duration = Duration::from_millis(50);
const DRAIN_INTERVAL: Duration = Duration::from_millis(50);

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MicroProbeOptions {
    #[serde(default)]
    pub capture_ms: u32,
    #[serde(default)]
    pub query: bool,
    #[serde(default)]
    pub read_keymap: bool,
    #[serde(default)]
    pub test_pattern: bool,
    #[serde(default)]
    pub health_check: bool,
}

impl MicroProbeOptions {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.capture_ms > 30_000 {
            return Err("capture_ms must be in 0..=30000");
        }
        if self.read_keymap && !self.query {
            return Err("read_keymap requires query");
        }
        if self.test_pattern && !self.query {
            return Err("test_pattern requires query");
        }
        if self.health_check && (self.query || self.read_keymap || self.test_pattern) {
            return Err("health_check conflicts with query, read_keymap, and test_pattern");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
    #[serde(deserialize_with = "required_nullable")]
    pub interface_number: Option<i32>,
    pub transport: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub elapsed_ms: u64,
    pub bytes: usize,
    pub hex: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Joystick {
    pub angle: f64,
    pub distance: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedInput {
    pub elapsed_ms: u64,
    pub message: Value,
    #[serde(deserialize_with = "required_nullable")]
    pub slot_requested: Option<u8>,
    #[serde(deserialize_with = "required_nullable")]
    pub command_key_observed: Option<String>,
    pub mic_observed: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub joystick: Option<Joystick>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    #[serde(deserialize_with = "required_nullable")]
    pub candidate: Option<Candidate>,
    pub opened: bool,
    pub descriptor_verified: bool,
    pub phase: String,
    #[serde(deserialize_with = "required_nullable")]
    pub firmware: Option<String>,
    pub firmware_reference_compatible: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub device_status: Option<Value>,
    #[serde(deserialize_with = "required_nullable")]
    pub keymap: Option<Value>,
    pub output_attempts: usize,
    pub output_reports: usize,
    pub output_bytes: usize,
    pub write_failed: bool,
    pub read_reports: usize,
    pub nonvendor_reports_ignored: usize,
    pub captures: Vec<Capture>,
    pub inputs: Vec<ObservedInput>,
    pub capture_truncated: bool,
    pub led_test_attempted: bool,
    pub led_clear_acknowledged: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub cleanup_error: Option<String>,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MicroProbeResult {
    pub probe_succeeded: bool,
    pub command_ready: bool,
    pub hardware_qualified: bool,
    pub read_only: bool,
    pub report: Progress,
    #[serde(deserialize_with = "required_nullable")]
    pub error: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub response_limit: Option<super::super::probe_result::ResponseLimit>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayResult {
    pub read_only: bool,
    pub command_ready: bool,
    pub hardware_qualified: bool,
    pub messages: Vec<Value>,
    pub incomplete_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayError {
    ReportCount,
    InvalidHex { index: usize },
    InvalidReportLength { index: usize },
    Decode { index: usize, message: String },
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReportCount => f.write_str("replay requires 1..=128 reports"),
            Self::InvalidHex { index } => {
                write!(f, "replay report {index} must be 128 ASCII hex characters")
            }
            Self::InvalidReportLength { index } => {
                write!(f, "replay report {index} must decode to exactly 64 bytes")
            }
            Self::Decode { index, message } => {
                write!(f, "replay report {index} could not be decoded: {message}")
            }
        }
    }
}

impl std::error::Error for ReplayError {}

pub trait ProbeIo {
    fn now(&self) -> Duration;
    fn write(&mut self, report: &[u8]) -> Result<usize, HidError>;
    fn read(&mut self, max_len: usize, timeout: Duration) -> Result<Vec<u8>, HidError>;
}

pub fn replay_micro_reports(reports: &[String]) -> Result<ReplayResult, ReplayError> {
    if !(1..=128).contains(&reports.len()) {
        return Err(ReplayError::ReportCount);
    }
    let mut decoder = super::wire::Decoder::new();
    let mut messages = Vec::new();
    for (index, encoded) in reports.iter().enumerate() {
        if encoded.len() != 128 || !encoded.is_ascii() {
            return Err(ReplayError::InvalidHex { index });
        }
        let packet = decode_replay_hex(encoded).ok_or(ReplayError::InvalidHex { index })?;
        if packet.len() != REPORT_SIZE {
            return Err(ReplayError::InvalidReportLength { index });
        }
        if let Some(message) =
            decoder
                .feed(&packet, Duration::ZERO)
                .map_err(|error| ReplayError::Decode {
                    index,
                    message: error.to_string(),
                })?
        {
            messages.push(message.as_value().clone());
        }
    }
    Ok(ReplayResult {
        read_only: true,
        command_ready: false,
        hardware_qualified: false,
        messages,
        incomplete_bytes: decoder.buffered_bytes().len(),
    })
}

pub struct ClockedHid<'a> {
    device: &'a mut HidDevice,
    started: std::time::Instant,
}

impl<'a> ClockedHid<'a> {
    pub fn new(device: &'a mut HidDevice) -> Self {
        Self {
            device,
            started: std::time::Instant::now(),
        }
    }
}

impl ProbeIo for ClockedHid<'_> {
    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    fn write(&mut self, report: &[u8]) -> Result<usize, HidError> {
        self.device.write_report(report)
    }

    fn read(&mut self, max_len: usize, timeout: Duration) -> Result<Vec<u8>, HidError> {
        self.device.read_report(max_len, timeout)
    }
}

/// Pure health-check path. It intentionally has no backend or transport parameter.
pub fn micro_health_check() -> MicroProbeResult {
    let report = Progress {
        phase: "health_check".to_owned(),
        ..Progress::default()
    };
    MicroProbeResult {
        probe_succeeded: true,
        read_only: true,
        report,
        ..MicroProbeResult::default()
    }
}

pub fn run_micro_health_check(options: MicroProbeOptions) -> MicroProbeResult {
    if let Err(error) = options.validate() {
        return invalid_result(error);
    }
    if !options.health_check {
        return invalid_result("health_check must be requested");
    }
    micro_health_check()
}

/// Executes against an already-open injected transport; live callers must provide
/// the same disposable-worker containment required by the native HID boundary.
pub fn run_micro_probe(
    io: &mut dyn ProbeIo,
    descriptor: &HidDescriptor,
    report_descriptor: &[u8],
    options: MicroProbeOptions,
) -> MicroProbeResult {
    if let Err(error) = options.validate() {
        return invalid_result(error);
    }
    if options.health_check {
        return invalid_result("health_check must use micro_health_check without a transport");
    }

    let mut result = MicroProbeResult {
        read_only: true,
        ..MicroProbeResult::default()
    };
    let candidate = Candidate {
        vendor_id: descriptor.identity.vendor_id,
        product_id: descriptor.identity.product_id,
        usage_page: descriptor.usage_page,
        usage: descriptor.usage,
        interface_number: descriptor.interface_number,
        transport: bus_name(descriptor.bus).to_owned(),
    };
    result.report.candidate = Some(candidate);

    let identity_valid = descriptor.identity
        == (HidIdentity {
            vendor_id: APPLICATION_VID,
            product_id: APPLICATION_PID,
        })
        && descriptor.usage_page == USAGE_PAGE
        && descriptor.usage == USAGE
        && descriptor.max_input_report_len == REPORT_SIZE
        && descriptor.max_output_report_len == REPORT_SIZE;
    if !identity_valid {
        result.error = Some("Micro HID identity or report layout mismatch".to_owned());
        return result;
    }
    result.report.opened = true;
    if let Err(error) = verify_micro_descriptor(descriptor, report_descriptor) {
        result.error = Some(error.to_string());
        return result;
    }
    result.report.descriptor_verified = true;
    run_micro_qualified(io, options, result)
}

fn run_micro_qualified(
    io: &mut dyn ProbeIo,
    options: MicroProbeOptions,
    mut result: MicroProbeResult,
) -> MicroProbeResult {
    let started = io.now();
    let mut rpc = RpcClient::new();
    let mut decoder = LegacyDecoder::new(Calibration::default());
    let succeeded = {
        let mut session = ProbeSession {
            io,
            rpc: &mut rpc,
            decoder: &mut decoder,
            report: &mut result.report,
            started,
        };
        let mut succeeded = true;

        if options.query {
            session.report.phase = "firmware".to_owned();
            match session.call("sys.version", Value::Null, None, false) {
                Ok(value) => {
                    let version = value
                        .as_ref()
                        .and_then(|value| value.as_value().get("version"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    session.report.firmware = version;
                    match value
                        .as_ref()
                        .map(|value| validate_firmware_version(value.as_value()))
                    {
                        Some(Ok(_)) => session.report.firmware_reference_compatible = true,
                        Some(Err(error)) => {
                            result.error = Some(error.to_string());
                            succeeded = false;
                        }
                        None => {
                            result.error = Some(MicroError::MalformedFirmwareResult.to_string());
                            succeeded = false;
                        }
                    }
                }
                Err(error) => {
                    result.error = Some(error);
                    succeeded = false;
                }
            }

            if succeeded {
                session.report.phase = "status".to_owned();
                match session.call("device.status", Value::Null, None, false) {
                    Ok(value) => {
                        session.report.device_status = value.map(|value| value.as_value().clone());
                    }
                    Err(error) => {
                        result.error = Some(error);
                        succeeded = false;
                    }
                }
            }
            if succeeded && options.read_keymap {
                session.report.phase = "keymap".to_owned();
                match session.call("fs.read", json!({"file":"keymap.json"}), None, false) {
                    Ok(value) => {
                        session.report.keymap = value.map(|value| value.as_value().clone());
                    }
                    Err(error) => {
                        result.error = Some(error);
                        succeeded = false;
                    }
                }
            }
            if succeeded && options.test_pattern {
                session.report.phase = "led_test".to_owned();
                session.report.led_test_attempted = true;
                if let Err(error) = session.call(
                    "v.oai.thstatus",
                    Value::Null,
                    Some(&diagnostic_frame()),
                    false,
                ) {
                    result.error = Some(error);
                    succeeded = false;
                }
            }
        }

        if succeeded {
            session.report.phase = "capture".to_owned();
            if let Err(error) =
                session.capture(Duration::from_millis(u64::from(options.capture_ms)))
            {
                result.error = Some(error);
                succeeded = false;
            }
        }

        if session.report.led_test_attempted && !session.report.write_failed {
            session.report.phase = "clear_leds".to_owned();
            match session.call("v.oai.thstatus", Value::Null, Some(&shutdown_frame()), true) {
                Ok(_) => session.report.led_clear_acknowledged = true,
                Err(error) => {
                    session.report.cleanup_error = Some(error.clone());
                    if result.error.is_none() {
                        result.error = Some(error);
                    }
                    succeeded = false;
                }
            }
        }

        succeeded
    };
    result.report.elapsed_ms = elapsed_ms(io.now(), started);
    result.report.phase = if succeeded {
        "complete".to_owned()
    } else {
        "failed".to_owned()
    };
    result.probe_succeeded = succeeded;
    result.read_only = result.report.output_attempts == 0;
    result
}

/// Enumerates and opens exactly one candidate. Live callers must invoke this in
/// a disposable, supervised worker; this function itself is not containment.
pub fn run_micro_probe_with_backend(
    backend: &dyn HidBackend,
    options: MicroProbeOptions,
) -> MicroProbeResult {
    if let Err(error) = options.validate() {
        return invalid_result(error);
    }
    if options.health_check {
        return invalid_result("health_check must use micro_health_check without a backend");
    }
    let matcher = HidMatcher {
        identity: HidIdentity {
            vendor_id: APPLICATION_VID,
            product_id: APPLICATION_PID,
        },
        usage_page: Some(USAGE_PAGE),
        usage: Some(USAGE),
    };
    let candidates = match enumerate(backend, &matcher, 64) {
        Ok(candidates) => candidates,
        Err(error) => return invalid_result(&error.to_string()),
    };
    if candidates.len() != 1 {
        return invalid_result(&format!(
            "Micro requires exactly one candidate; found {}",
            candidates.len()
        ));
    }
    let descriptor = &candidates[0];
    if descriptor.interface_number.is_none() {
        let mut result = invalid_result(
            &HidError::EvidenceUnavailable(EvidenceKind::InterfaceNumber).to_string(),
        );
        result.report.candidate = Some(Candidate {
            vendor_id: descriptor.identity.vendor_id,
            product_id: descriptor.identity.product_id,
            usage_page: descriptor.usage_page,
            usage: descriptor.usage,
            interface_number: descriptor.interface_number,
            transport: bus_name(descriptor.bus).to_owned(),
        });
        return result;
    }
    if descriptor.bus.is_none() || descriptor.bus == Some(HidBus::Unknown) {
        let mut result = invalid_result(
            &HidError::EvidenceUnavailable(EvidenceKind::BusClassification).to_string(),
        );
        result.report.candidate = Some(Candidate {
            vendor_id: descriptor.identity.vendor_id,
            product_id: descriptor.identity.product_id,
            usage_page: descriptor.usage_page,
            usage: descriptor.usage,
            interface_number: descriptor.interface_number,
            transport: bus_name(descriptor.bus).to_owned(),
        });
        return result;
    }
    if descriptor.max_input_report_len != REPORT_SIZE
        || descriptor.max_output_report_len != REPORT_SIZE
    {
        return invalid_result("Micro HID report layout mismatch");
    }
    let mut device = match HidDevice::open_allowing_usage_change(backend, descriptor) {
        Ok(device) => device,
        Err(error) => return invalid_result(&error.to_string()),
    };
    let report_descriptor = match device.report_descriptor() {
        Ok(bytes) => bytes,
        Err(error) => {
            let mut result = invalid_result(&error.to_string());
            result.report.candidate = Some(Candidate {
                vendor_id: descriptor.identity.vendor_id,
                product_id: descriptor.identity.product_id,
                usage_page: descriptor.usage_page,
                usage: descriptor.usage,
                interface_number: descriptor.interface_number,
                transport: bus_name(descriptor.bus).to_owned(),
            });
            result.report.opened = true;
            return result;
        }
    };
    let mut io = ClockedHid::new(&mut device);
    run_micro_probe(&mut io, descriptor, &report_descriptor, options)
}

pub fn verify_micro_descriptor(
    descriptor: &HidDescriptor,
    report_descriptor: &[u8],
) -> Result<(), HidError> {
    let interface = descriptor
        .interface_number
        .ok_or(HidError::EvidenceUnavailable(EvidenceKind::InterfaceNumber))?;
    let bus = descriptor.bus.ok_or(HidError::EvidenceUnavailable(
        EvidenceKind::BusClassification,
    ))?;
    let (length, expected_hash) = match (bus, interface) {
        (HidBus::Usb, 0) => (
            275,
            "9257d7361f9c784e0fc0b260bbac0feadd49bf79cbb6202d6c41560cbae96fb6",
        ),
        (HidBus::Usb, 2) => (
            36,
            "08229e86dff4b74981ad1c14f06deaee61a6d41d147273f2cdde860ea435986b",
        ),
        (HidBus::Bluetooth, -1) if cfg!(target_os = "macos") => (
            206,
            "a173b5298035f16f1d96a99d8cef32ae8176262da4666ee18ad74f9aa3a23115",
        ),
        (HidBus::Unknown, _) => {
            return Err(HidError::EvidenceUnavailable(
                EvidenceKind::BusClassification,
            ));
        }
        (HidBus::Bluetooth, -1) => {
            return Err(HidError::Protocol(
                "Bluetooth Micro descriptor qualification is macOS-only",
            ));
        }
        _ => return Err(HidError::Protocol("unqualified Micro transport/interface")),
    };
    if report_descriptor.len() != length
        || format!("{:x}", Sha256::digest(report_descriptor)) != expected_hash
    {
        return Err(HidError::Protocol(
            "Micro report descriptor does not match a qualified profile",
        ));
    }
    Ok(())
}

fn invalid_result(error: &str) -> MicroProbeResult {
    MicroProbeResult {
        read_only: true,
        error: Some(error.to_owned()),
        ..MicroProbeResult::default()
    }
}

fn bus_name(bus: Option<HidBus>) -> &'static str {
    match bus {
        Some(HidBus::Usb) => "usb",
        Some(HidBus::Bluetooth) => "bluetooth",
        Some(HidBus::Unknown) | None => "unknown",
    }
}

struct ProbeSession<'a> {
    io: &'a mut dyn ProbeIo,
    rpc: &'a mut RpcClient,
    decoder: &'a mut LegacyDecoder,
    report: &'a mut Progress,
    started: Duration,
}

impl ProbeSession<'_> {
    fn call(
        &mut self,
        method: &str,
        params: Value,
        led: Option<&[super::led::LedEntry; 6]>,
        cleanup: bool,
    ) -> Result<Option<super::value::JsonValue>, String> {
        let drain_complete = if self.rpc.next_request_id() != Some(101) {
            self.drain(cleanup)?
        } else {
            true
        };
        if cleanup {
            self.rpc.abandon_pending();
        }
        let request = match led {
            Some(entries) => self.rpc.begin_led(entries),
            None => self.rpc.begin(method, &params),
        }
        .map_err(|error| error.to_string())?;
        for packet in &request.reports {
            self.report.output_attempts += 1;
            let count = match self.io.write(packet) {
                Ok(count) => count,
                Err(error) => {
                    self.report.write_failed = true;
                    return Err(error.to_string());
                }
            };
            self.report.output_bytes = self.report.output_bytes.saturating_add(count);
            if count == REPORT_SIZE {
                self.report.output_reports += 1;
            } else {
                self.report.write_failed = true;
                return Err(MicroError::ShortWrite { count }.to_string());
            }
            self.rpc
                .confirm_write(count, self.io.now())
                .map_err(|error| error.to_string())?;
        }

        if !drain_complete {
            return Err(
                "Micro read budget exhausted; cleanup drain incomplete; shutdown written without acknowledgement"
                    .to_owned(),
            );
        }

        loop {
            let timeout = READ_SLICE.min(
                self.rpc
                    .read_time_remaining(self.io.now())
                    .map_err(|error| error.to_string())?,
            );
            match read_one(self.io, self.report, self.started, timeout)? {
                ReadOutcome::Packet(packet) => {
                    let response = self
                        .rpc
                        .feed_report(&packet, self.io.now())
                        .map_err(|error| error.to_string())?;
                    record_inputs(
                        self.rpc,
                        self.decoder,
                        self.report,
                        self.io.now().saturating_sub(self.started),
                    )?;
                    if response.is_some() {
                        return Ok(response);
                    }
                }
                ReadOutcome::Ignored | ReadOutcome::Idle => self
                    .rpc
                    .check_read_deadline(self.io.now())
                    .map_err(|error| error.to_string())?,
            }
        }
    }

    fn drain(&mut self, cleanup: bool) -> Result<bool, String> {
        let deadline = self
            .io
            .now()
            .checked_add(DRAIN_INTERVAL)
            .ok_or_else(|| "Micro clock out of range".to_owned())?;
        while self.io.now() < deadline {
            // An exhausted receive budget cannot forbid a complete shutdown
            // write. It still forbids draining or reading its acknowledgement.
            if cleanup && self.report.read_reports >= READ_BUDGET {
                return Ok(false);
            }
            let timeout = READ_SLICE.min(deadline.saturating_sub(self.io.now()));
            match read_one(self.io, self.report, self.started, timeout)? {
                ReadOutcome::Packet(packet) => {
                    self.rpc
                        .feed_observation_report(&packet, self.io.now())
                        .map_err(|error| error.to_string())?;
                    record_inputs(
                        self.rpc,
                        self.decoder,
                        self.report,
                        self.io.now().saturating_sub(self.started),
                    )?;
                }
                ReadOutcome::Ignored | ReadOutcome::Idle => {}
            }
        }
        Ok(!cleanup || self.report.read_reports < READ_BUDGET)
    }

    fn capture(&mut self, duration: Duration) -> Result<(), String> {
        let deadline = self
            .io
            .now()
            .checked_add(duration)
            .ok_or_else(|| "Micro clock out of range".to_owned())?;
        while self.io.now() < deadline {
            if self.report.captures.len() == CAPTURE_BUDGET {
                self.report.capture_truncated = true;
                break;
            }
            let timeout = READ_SLICE.min(deadline.saturating_sub(self.io.now()));
            match read_one(self.io, self.report, self.started, timeout)? {
                ReadOutcome::Packet(packet) => {
                    self.rpc
                        .feed_observation_report(&packet, self.io.now())
                        .map_err(|error| error.to_string())?;
                    record_inputs(
                        self.rpc,
                        self.decoder,
                        self.report,
                        self.io.now().saturating_sub(self.started),
                    )?;
                }
                ReadOutcome::Ignored | ReadOutcome::Idle => {}
            }
        }
        Ok(())
    }
}

/// Reads at most 65 bytes. A nonempty packet is accounted once before its
/// channel-specific validation, except oversized packets which are rejected first.
enum ReadOutcome {
    Packet(Vec<u8>),
    Ignored,
    Idle,
}

fn read_one(
    io: &mut dyn ProbeIo,
    report: &mut Progress,
    started: Duration,
    timeout: Duration,
) -> Result<ReadOutcome, String> {
    if report.read_reports >= READ_BUDGET {
        return Err("Micro read budget exhausted".to_owned());
    }
    let before = io.now();
    let packet = match io.read(READ_SIZE, timeout) {
        Ok(packet) => packet,
        Err(HidError::Timeout) => {
            if io.now() <= before {
                return Err("Micro clock did not advance after read timeout".to_owned());
            }
            return Ok(ReadOutcome::Idle);
        }
        Err(error) => return Err(error.to_string()),
    };
    if packet.is_empty() {
        if timeout > Duration::ZERO && io.now() <= before {
            return Err("Micro clock did not advance after empty read".to_owned());
        }
        return Ok(ReadOutcome::Idle);
    }
    if packet.len() > REPORT_SIZE {
        return Err("Micro report exceeds 64 bytes".to_owned());
    }
    report.read_reports += 1;
    match packet[0] {
        1..=4 => {
            report.nonvendor_reports_ignored += 1;
            Ok(ReadOutcome::Ignored)
        }
        REPORT_ID => {
            if packet.len() != REPORT_SIZE {
                return Err(MicroError::ReportSize.to_string());
            }
            if report.captures.len() < CAPTURE_BUDGET {
                report.captures.push(Capture {
                    elapsed_ms: elapsed_ms(io.now(), started),
                    bytes: packet.len(),
                    hex: hex(&packet),
                });
            } else {
                report.capture_truncated = true;
            }
            if !packet
                .get(1)
                .is_some_and(|channel| matches!(*channel, JSON_CHANNEL | LOG_CHANNEL))
            {
                return Err(MicroError::UnknownChannel.to_string());
            }
            Ok(ReadOutcome::Packet(packet))
        }
        _ => Err(MicroError::InvalidFrame.to_string()),
    }
}

fn record_inputs(
    rpc: &mut RpcClient,
    decoder: &mut LegacyDecoder,
    report: &mut Progress,
    elapsed: Duration,
) -> Result<(), String> {
    while let Some(message) = rpc.pop_input() {
        if report.inputs.len() == INPUT_BUDGET {
            return Err("Micro input budget exhausted; events not dispatched".to_owned());
        }
        let all_slots = Slot::ALL;
        let bindings = Bindings::empty();
        let context = LegacyContext {
            monotonic: elapsed,
            wall_seconds: 0.0,
            bindings: &bindings,
            controls_enabled: true,
            active_slots: Some(&all_slots),
            selected_slot: None,
        };
        let decoded = decoder
            .decode(message.as_value(), &context)
            .map_err(|error| error.to_string())?;
        let joystick = decoder.telemetry().map(|value| Joystick {
            angle: value.angle,
            distance: value.distance,
        });
        report.inputs.push(ObservedInput {
            elapsed_ms: elapsed_ms(elapsed, Duration::ZERO),
            message: message.as_value().clone(),
            slot_requested: decoded.slot.map(Slot::get),
            command_key_observed: decoded.command.map(|command| command.key.name().to_owned()),
            mic_observed: decoded.microphone,
            joystick,
        });
    }
    Ok(())
}

fn elapsed_ms(now: Duration, start: Duration) -> u64 {
    now.saturating_sub(start)
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn decode_replay_hex(text: &str) -> Option<Vec<u8>> {
    let mut decoded = Vec::with_capacity(REPORT_SIZE);
    for pair in text.as_bytes().as_chunks::<2>().0 {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        decoded.push((high << 4) | low);
    }
    Some(decoded)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::native::hid::fake::FakeBackend;

    struct Fake {
        now: Duration,
        incoming: VecDeque<Vec<u8>>,
        writes: Vec<Vec<u8>>,
        write_count: Option<usize>,
        reply_after_writes: VecDeque<(usize, Vec<u8>)>,
        fail_read_after_writes: Option<usize>,
        read_failure: Option<HidError>,
    }

    impl ProbeIo for Fake {
        fn now(&self) -> Duration {
            self.now
        }

        fn write(&mut self, report: &[u8]) -> Result<usize, HidError> {
            self.writes.push(report.to_vec());
            while self
                .reply_after_writes
                .front()
                .is_some_and(|(count, _)| *count == self.writes.len())
            {
                if let Some((_, reply)) = self.reply_after_writes.pop_front() {
                    self.incoming.push_back(reply);
                }
            }
            Ok(self.write_count.unwrap_or(report.len()))
        }

        fn read(&mut self, max_len: usize, timeout: Duration) -> Result<Vec<u8>, HidError> {
            if let Some(report) = self.incoming.pop_front() {
                return Ok(report[..report.len().min(max_len)].to_vec());
            }
            if self
                .fail_read_after_writes
                .is_some_and(|count| self.writes.len() >= count)
                && let Some(error) = self.read_failure.take()
            {
                return Err(error);
            }
            self.now += timeout;
            Err(HidError::Timeout)
        }
    }

    fn descriptor() -> HidDescriptor {
        HidDescriptor {
            identity: HidIdentity {
                vendor_id: APPLICATION_VID,
                product_id: APPLICATION_PID,
            },
            usage_page: USAGE_PAGE,
            usage: USAGE,
            product: None,
            serial: None,
            max_input_report_len: REPORT_SIZE,
            max_output_report_len: REPORT_SIZE,
            interface_number: Some(2),
            bus: Some(HidBus::Usb),
            path: crate::native::hid::HidPath::new("fake").unwrap(),
        }
    }

    fn run_qualified(
        fake: &mut dyn ProbeIo,
        descriptor: &HidDescriptor,
        options: MicroProbeOptions,
    ) -> MicroProbeResult {
        let mut result = MicroProbeResult {
            read_only: true,
            ..MicroProbeResult::default()
        };
        result.report.candidate = Some(Candidate {
            vendor_id: descriptor.identity.vendor_id,
            product_id: descriptor.identity.product_id,
            usage_page: descriptor.usage_page,
            usage: descriptor.usage,
            interface_number: descriptor.interface_number,
            transport: bus_name(descriptor.bus).to_owned(),
        });
        result.report.opened = true;
        result.report.descriptor_verified = true;
        run_micro_qualified(fake, options, result)
    }

    fn response(id: u64, result: Value) -> Vec<u8> {
        let json = serde_json::to_vec(&json!({"id":id,"result":result})).unwrap();
        let mut report = vec![0; REPORT_SIZE];
        report[0] = REPORT_ID;
        report[1] = JSON_CHANNEL;
        report[2] = json.len() as u8;
        report[3..3 + json.len()].copy_from_slice(&json);
        report
    }

    fn replay_report(payload: &[u8], channel: u8) -> String {
        let mut report = vec![0; REPORT_SIZE];
        report[0] = REPORT_ID;
        report[1] = channel;
        report[2] = payload.len() as u8;
        report[3..3 + payload.len()].copy_from_slice(payload);
        hex(&report)
    }

    fn vendor_report(channel: u8, payload: &[u8]) -> Vec<u8> {
        let mut report = vec![0; REPORT_SIZE];
        report[0] = REPORT_ID;
        report[1] = channel;
        report[2] = payload.len() as u8;
        report[3..3 + payload.len()].copy_from_slice(payload);
        report
    }

    fn pattern_flow_fake(include_cleanup_reply: bool, fail_capture: bool) -> (Fake, usize) {
        let pattern_reports = RpcClient::new()
            .begin_led(&diagnostic_frame())
            .unwrap()
            .reports
            .len();
        let cleanup_reports = RpcClient::new()
            .begin_led(&shutdown_frame())
            .unwrap()
            .reports
            .len();
        let pattern_complete = 2 + pattern_reports;
        let cleanup_complete = pattern_complete + cleanup_reports;
        let mut replies = VecDeque::from([
            (1, response(101, json!({"version":"0.6.3"}))),
            (2, response(102, json!({"connected":true}))),
            (pattern_complete, response(103, Value::Null)),
        ]);
        if include_cleanup_reply {
            replies.push_back((cleanup_complete, response(104, Value::Null)));
        }
        (
            Fake {
                now: Duration::ZERO,
                incoming: VecDeque::new(),
                writes: Vec::new(),
                write_count: None,
                reply_after_writes: replies,
                fail_read_after_writes: fail_capture.then_some(pattern_complete),
                read_failure: fail_capture.then_some(HidError::Os {
                    op: crate::native::hid::HidOp::Read,
                    code: 9,
                }),
            },
            pattern_complete,
        )
    }

    #[test]
    fn health_check_is_transport_free_and_options_are_validated_first() {
        let result = run_micro_health_check(MicroProbeOptions {
            health_check: true,
            ..MicroProbeOptions::default()
        });
        assert!(result.probe_succeeded);
        assert_eq!(result.report.phase, "health_check");
        assert!(result.read_only);
        assert!(!result.command_ready && !result.hardware_qualified);
        assert!(
            MicroProbeOptions {
                read_keymap: true,
                ..MicroProbeOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(!run_micro_health_check(MicroProbeOptions::default()).probe_succeeded);
        assert!(
            MicroProbeOptions {
                capture_ms: 30_000,
                ..MicroProbeOptions::default()
            }
            .validate()
            .is_ok()
        );
        assert!(
            MicroProbeOptions {
                capture_ms: 30_001,
                ..MicroProbeOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            MicroProbeOptions {
                health_check: true,
                query: true,
                ..MicroProbeOptions::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn query_runs_version_gate_then_status_and_stays_unqualified() {
        let mut fake = Fake {
            now: Duration::ZERO,
            incoming: VecDeque::new(),
            writes: Vec::new(),
            write_count: None,
            reply_after_writes: VecDeque::from([
                (1, response(101, json!({"version":"0.6.3"}))),
                (2, response(102, json!({"connected":true}))),
            ]),
            fail_read_after_writes: None,
            read_failure: None,
        };
        let result = run_qualified(
            &mut fake,
            &descriptor(),
            MicroProbeOptions {
                query: true,
                ..MicroProbeOptions::default()
            },
        );
        assert!(result.probe_succeeded, "{:?}", result.error);
        assert_eq!(result.report.firmware.as_deref(), Some("0.6.3"));
        assert_eq!(result.report.device_status, Some(json!({"connected":true})));
        assert_eq!(result.report.output_reports, 2);
        assert_eq!(result.report.output_attempts, 2);
        assert!(!result.command_ready && !result.hardware_qualified);
    }

    #[test]
    fn quiet_capture_continues_across_multiple_read_slices() {
        let (mut fake, _) = pattern_flow_fake(true, false);
        let result = run_qualified(
            &mut fake,
            &descriptor(),
            MicroProbeOptions {
                capture_ms: 125,
                ..Default::default()
            },
        );
        assert!(result.probe_succeeded, "{:?}", result.error);
        assert_eq!(result.report.phase, "complete");
        assert_eq!(result.report.elapsed_ms, 125);
        assert_eq!(result.report.read_reports, 0);
        assert!(result.report.captures.is_empty());
        assert!(fake.writes.is_empty());
    }

    #[test]
    fn early_idle_reads_continue_drain_until_its_fixed_deadline() {
        struct EarlyIdle {
            now: Duration,
            timeouts: Vec<Duration>,
            empty: bool,
        }
        impl ProbeIo for EarlyIdle {
            fn now(&self) -> Duration {
                self.now
            }
            fn write(&mut self, _: &[u8]) -> Result<usize, HidError> {
                panic!("quiet drain must not write");
            }
            fn read(&mut self, _: usize, timeout: Duration) -> Result<Vec<u8>, HidError> {
                self.timeouts.push(timeout);
                self.now += timeout.min(Duration::from_millis(10));
                if self.empty {
                    Ok(Vec::new())
                } else {
                    Err(HidError::Timeout)
                }
            }
        }
        for empty in [false, true] {
            let mut io = EarlyIdle {
                now: Duration::ZERO,
                timeouts: Vec::new(),
                empty,
            };
            let mut rpc = RpcClient::new();
            let mut decoder = LegacyDecoder::new(Calibration::default());
            let mut report = Progress::default();
            ProbeSession {
                io: &mut io,
                rpc: &mut rpc,
                decoder: &mut decoder,
                report: &mut report,
                started: Duration::ZERO,
            }
            .drain(false)
            .unwrap();
            assert_eq!(io.now, DRAIN_INTERVAL);
            assert_eq!(io.timeouts, [50, 40, 30, 20, 10].map(Duration::from_millis));
            assert_eq!(report.read_reports, 0);
        }
    }

    #[test]
    fn short_write_is_never_retried_and_fails_closed() {
        let mut fake = Fake {
            now: Duration::ZERO,
            incoming: VecDeque::new(),
            writes: Vec::new(),
            write_count: Some(63),
            reply_after_writes: VecDeque::new(),
            fail_read_after_writes: None,
            read_failure: None,
        };
        let result = run_qualified(
            &mut fake,
            &descriptor(),
            MicroProbeOptions {
                query: true,
                ..MicroProbeOptions::default()
            },
        );
        assert!(!result.probe_succeeded);
        assert!(result.report.write_failed);
        assert_eq!(result.report.output_attempts, 1);
        assert_eq!(fake.writes.len(), 1);
    }

    #[test]
    fn missing_interface_evidence_fails_before_reads_or_writes() {
        let mut fake = Fake {
            now: Duration::ZERO,
            incoming: VecDeque::new(),
            writes: Vec::new(),
            write_count: None,
            reply_after_writes: VecDeque::new(),
            fail_read_after_writes: None,
            read_failure: None,
        };
        let mut descriptor = descriptor();
        descriptor.interface_number = None;
        let result = run_micro_probe(&mut fake, &descriptor, &[], MicroProbeOptions::default());
        assert!(!result.probe_succeeded);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("EvidenceUnavailable")
        );
        assert!(fake.writes.is_empty());
        assert_eq!(fake.now, Duration::ZERO);
    }

    #[test]
    fn descriptor_qualification_requires_native_transport_and_exact_bytes() {
        let descriptor = descriptor();
        assert!(matches!(
            verify_micro_descriptor(&descriptor, &[]),
            Err(HidError::Protocol(_))
        ));
        assert!(matches!(
            verify_micro_descriptor(&descriptor, &[0; 36]),
            Err(HidError::Protocol(_))
        ));

        let mut missing_bus = descriptor.clone();
        missing_bus.bus = None;
        assert_eq!(
            verify_micro_descriptor(&missing_bus, &[]),
            Err(HidError::EvidenceUnavailable(
                EvidenceKind::BusClassification
            ))
        );

        let mut unsupported_interface = descriptor.clone();
        unsupported_interface.interface_number = Some(3);
        assert!(matches!(
            verify_micro_descriptor(&unsupported_interface, &[]),
            Err(HidError::Protocol(_))
        ));

        let mut bluetooth = descriptor;
        bluetooth.interface_number = Some(-1);
        bluetooth.bus = Some(HidBus::Bluetooth);
        if !cfg!(target_os = "macos") {
            assert!(matches!(
                verify_micro_descriptor(&bluetooth, &[]),
                Err(HidError::Protocol(_))
            ));
        }
    }

    #[test]
    fn backend_runner_requires_one_matching_candidate_and_opens_once() {
        let backend = FakeBackend::new(vec![descriptor()]);
        let result = run_micro_probe_with_backend(&backend, MicroProbeOptions::default());
        assert!(!result.probe_succeeded);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("EvidenceUnavailable")
        );
        assert!(result.report.opened);
        assert_eq!(backend.state.borrow().enumerate_calls, 1);
        assert_eq!(backend.state.borrow().open_calls, 1);
        assert!(backend.state.borrow().written.is_empty());
    }

    #[test]
    fn backend_runner_rejects_bad_options_before_enumeration() {
        let backend = FakeBackend::new(vec![descriptor()]);
        let result = run_micro_probe_with_backend(
            &backend,
            MicroProbeOptions {
                capture_ms: 30_001,
                ..MicroProbeOptions::default()
            },
        );
        assert!(!result.probe_succeeded);
        assert_eq!(backend.state.borrow().enumerate_calls, 0);
        assert_eq!(backend.state.borrow().open_calls, 0);
    }

    #[test]
    fn offline_replay_decodes_without_rpc_admission_and_retains_incomplete_tail() {
        let first = replay_report(b"{\"a\":", JSON_CHANNEL);
        let second = replay_report(b"1}", JSON_CHANNEL);
        let result = replay_micro_reports(&[first, second]).unwrap();
        assert!(result.read_only);
        assert!(!result.command_ready && !result.hardware_qualified);
        assert_eq!(result.messages, vec![json!({"a":1})]);
        assert_eq!(result.incomplete_bytes, 0);

        let incomplete =
            replay_micro_reports(&[replay_report(b"{\"partial\":", JSON_CHANNEL)]).unwrap();
        assert!(incomplete.messages.is_empty());
        assert_eq!(incomplete.incomplete_bytes, 11);
    }

    #[test]
    fn replay_requires_exact_ascii_hex_report_size() {
        assert_eq!(replay_micro_reports(&[]), Err(ReplayError::ReportCount));
        assert!(matches!(
            replay_micro_reports(&["00".repeat(63)]),
            Err(ReplayError::InvalidHex { index: 0 })
        ));
        let mut invalid = "00".repeat(64);
        invalid.replace_range(0..1, " ");
        assert!(matches!(
            replay_micro_reports(&[invalid]),
            Err(ReplayError::InvalidHex { index: 0 })
        ));
    }

    #[test]
    fn passive_capture_ignores_short_nonvendor_reports_and_records_vendor_hex() {
        let mut fake = Fake {
            now: Duration::ZERO,
            incoming: VecDeque::from([
                vec![1, 2, 3],
                vendor_report(
                    JSON_CHANNEL,
                    b"{\"m\":\"v.oai.hid\",\"p\":{\"k\":\"ACT06\",\"act\":1}}",
                ),
            ]),
            writes: Vec::new(),
            write_count: None,
            reply_after_writes: VecDeque::new(),
            fail_read_after_writes: None,
            read_failure: None,
        };
        let result = run_qualified(
            &mut fake,
            &descriptor(),
            MicroProbeOptions {
                capture_ms: 50,
                ..MicroProbeOptions::default()
            },
        );
        assert!(result.probe_succeeded, "{:?}", result.error);
        assert_eq!(result.report.nonvendor_reports_ignored, 1);
        assert_eq!(result.report.read_reports, 2);
        assert_eq!(result.report.captures.len(), 1);
        assert!(
            result.report.captures[0]
                .hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        );
        assert_eq!(result.report.inputs.len(), 1);
        assert_eq!(
            result.report.inputs[0].command_key_observed.as_deref(),
            Some("ACT06")
        );
    }

    #[test]
    fn read_report_budget_blocks_the_next_timeout_read() {
        let mut fake = Fake {
            now: Duration::ZERO,
            incoming: VecDeque::from(vec![vec![1]; 512]),
            writes: Vec::new(),
            write_count: None,
            reply_after_writes: VecDeque::new(),
            fail_read_after_writes: None,
            read_failure: None,
        };
        let result = run_qualified(
            &mut fake,
            &descriptor(),
            MicroProbeOptions {
                capture_ms: 1000,
                ..MicroProbeOptions::default()
            },
        );
        assert!(!result.probe_succeeded);
        assert_eq!(result.report.read_reports, 512);
        assert_eq!(result.report.nonvendor_reports_ignored, 512);
        assert_eq!(fake.incoming.len(), 0);
        assert_eq!(result.error.as_deref(), Some("Micro read budget exhausted"));
    }

    #[test]
    fn exhausted_capture_or_cleanup_drain_still_writes_shutdown_without_extra_reads() {
        for capture_ms in [0, 100] {
            let (mut fake, pattern_complete) = pattern_flow_fake(true, false);
            let cleanup_reply = fake.reply_after_writes.pop_back().unwrap();
            // Version, status and pattern replies consume three report reads.
            fake.reply_after_writes.extend(std::iter::repeat_n(
                (pattern_complete, vec![1]),
                READ_BUDGET - 3,
            ));
            fake.reply_after_writes.push_back(cleanup_reply);
            let result = run_qualified(
                &mut fake,
                &descriptor(),
                MicroProbeOptions {
                    query: true,
                    test_pattern: true,
                    capture_ms,
                    ..Default::default()
                },
            );
            assert!(!result.probe_succeeded);
            assert!(result.report.led_test_attempted);
            assert!(!result.report.led_clear_acknowledged);
            assert!(!result.report.write_failed);
            assert_eq!(result.report.read_reports, READ_BUDGET);
            assert_eq!(result.report.nonvendor_reports_ignored, READ_BUDGET - 3);
            assert_eq!(result.report.elapsed_ms, 100);
            assert_eq!(fake.incoming, VecDeque::from([response(104, Value::Null)]));
            assert_eq!(
                result.report.cleanup_error.as_deref(),
                Some(
                    "Micro read budget exhausted; cleanup drain incomplete; shutdown written without acknowledgement"
                ),
            );
            if capture_ms == 0 {
                assert_eq!(result.error, result.report.cleanup_error);
            } else {
                assert_eq!(result.error.as_deref(), Some("Micro read budget exhausted"));
            }
            let shutdown_count = RpcClient::new()
                .begin_led(&shutdown_frame())
                .unwrap()
                .reports
                .len();
            assert_eq!(fake.writes.len(), pattern_complete + shutdown_count);
            assert_eq!(result.report.output_attempts, fake.writes.len());
            assert_eq!(result.report.output_reports, fake.writes.len());
            assert_eq!(result.report.output_bytes, fake.writes.len() * REPORT_SIZE);
            let mut decoder = crate::devices::micro::wire::Decoder::new();
            let mut clear = None;
            for report in &fake.writes[pattern_complete..] {
                if let Some(message) = decoder.feed(report, Duration::ZERO).unwrap() {
                    assert!(clear.replace(message).is_none());
                }
            }
            let clear = clear.unwrap();
            assert_eq!(clear.as_map().get("method"), Some(&json!("v.oai.thstatus")));
            assert_eq!(clear.as_map().get("id"), Some(&json!(104)));
            assert_eq!(
                clear.as_map().get("params"),
                Some(&serde_json::to_value(shutdown_frame()).unwrap()),
            );
        }
    }

    #[test]
    fn cleanup_budget_exhausted_exactly_at_drain_deadline_reports_incomplete_cleanup() {
        struct DeadlineRead {
            inner: Fake,
            reports: usize,
        }
        impl ProbeIo for DeadlineRead {
            fn now(&self) -> Duration {
                self.inner.now()
            }
            fn write(&mut self, report: &[u8]) -> Result<usize, HidError> {
                self.inner.write(report)
            }
            fn read(&mut self, max_len: usize, timeout: Duration) -> Result<Vec<u8>, HidError> {
                assert!(self.reports < READ_BUDGET, "read after budget exhaustion");
                let report = self.inner.read(max_len, timeout)?;
                if !report.is_empty() {
                    self.reports += 1;
                    if self.reports == READ_BUDGET {
                        self.inner.now += timeout;
                    }
                }
                Ok(report)
            }
        }
        let (mut fake, pattern_complete) = pattern_flow_fake(true, false);
        let cleanup_reply = fake.reply_after_writes.pop_back().unwrap();
        fake.reply_after_writes.extend(std::iter::repeat_n(
            (pattern_complete, vec![1]),
            READ_BUDGET - 3,
        ));
        fake.reply_after_writes.push_back(cleanup_reply);
        let mut io = DeadlineRead {
            inner: fake,
            reports: 0,
        };
        let result = run_qualified(
            &mut io,
            &descriptor(),
            MicroProbeOptions {
                query: true,
                test_pattern: true,
                capture_ms: 0,
                ..Default::default()
            },
        );
        assert_eq!(io.reports, READ_BUDGET);
        assert_eq!(result.report.read_reports, READ_BUDGET);
        assert_eq!(result.report.elapsed_ms, 150);
        assert_eq!(
            result.report.cleanup_error.as_deref(),
            Some(
                "Micro read budget exhausted; cleanup drain incomplete; shutdown written without acknowledgement"
            ),
        );
        assert_eq!(result.error, result.report.cleanup_error);
        assert!(!result.probe_succeeded && !result.report.led_clear_acknowledged);
        assert!(result.report.led_test_attempted && !result.report.write_failed);
        let shutdown_count = RpcClient::new()
            .begin_led(&shutdown_frame())
            .unwrap()
            .reports
            .len();
        assert_eq!(io.inner.writes.len(), pattern_complete + shutdown_count);
        assert_eq!(result.report.output_attempts, io.inner.writes.len());
        assert_eq!(result.report.output_reports, io.inner.writes.len());
        assert_eq!(
            io.inner.incoming,
            VecDeque::from([response(104, Value::Null)])
        );
    }

    #[test]
    fn capture_and_input_budgets_are_independent_and_bounded() {
        let report = vendor_report(LOG_CHANNEL, b"ignored");
        let mut fake = Fake {
            now: Duration::ZERO,
            incoming: VecDeque::from(vec![report; 129]),
            writes: Vec::new(),
            write_count: None,
            reply_after_writes: VecDeque::new(),
            fail_read_after_writes: None,
            read_failure: None,
        };
        let truncated = run_qualified(
            &mut fake,
            &descriptor(),
            MicroProbeOptions {
                capture_ms: 1000,
                ..MicroProbeOptions::default()
            },
        );
        assert!(truncated.probe_succeeded);
        assert!(truncated.report.capture_truncated);
        assert_eq!(truncated.report.captures.len(), 128);
        assert_eq!(fake.incoming.len(), 1);

        let event = vendor_report(
            JSON_CHANNEL,
            b"{\"m\":\"v.oai.hid\",\"p\":{\"k\":\"ACT06\",\"act\":1}}",
        );
        let mut fake = Fake {
            now: Duration::ZERO,
            incoming: VecDeque::from(vec![event; 129]),
            writes: Vec::new(),
            write_count: None,
            reply_after_writes: VecDeque::from([(1, response(101, json!({"version":"0.6.3"})))]),
            fail_read_after_writes: None,
            read_failure: None,
        };
        let exhausted = run_qualified(
            &mut fake,
            &descriptor(),
            MicroProbeOptions {
                query: true,
                ..MicroProbeOptions::default()
            },
        );
        assert!(!exhausted.probe_succeeded);
        assert_eq!(exhausted.report.inputs.len(), 128);
        assert_eq!(
            exhausted.error.as_deref(),
            Some("Micro input budget exhausted; events not dispatched")
        );
    }

    #[test]
    fn attempted_led_test_always_cleans_up_and_preserves_primary_failure() {
        let (mut fake, _) = pattern_flow_fake(true, true);
        let result = run_qualified(
            &mut fake,
            &descriptor(),
            MicroProbeOptions {
                query: true,
                test_pattern: true,
                capture_ms: 100,
                ..MicroProbeOptions::default()
            },
        );
        assert!(!result.probe_succeeded);
        assert!(result.report.led_test_attempted);
        assert!(result.report.led_clear_acknowledged);
        assert!(result.report.cleanup_error.is_none());
        assert_eq!(result.error.as_deref(), Some("Os { op: Read, code: 9 }"));
        assert_eq!(fake.writes.len(), result.report.output_attempts);

        let (mut fake, _) = pattern_flow_fake(false, true);
        let result = run_qualified(
            &mut fake,
            &descriptor(),
            MicroProbeOptions {
                query: true,
                test_pattern: true,
                capture_ms: 100,
                ..MicroProbeOptions::default()
            },
        );
        assert!(!result.probe_succeeded);
        assert!(result.report.led_test_attempted);
        assert!(!result.report.led_clear_acknowledged);
        assert!(result.report.cleanup_error.is_some());
        assert_eq!(result.error.as_deref(), Some("Os { op: Read, code: 9 }"));
    }

    #[test]
    fn cleanup_failure_is_reported_and_converts_prior_success() {
        let (mut fake, _) = pattern_flow_fake(false, false);
        let result = run_qualified(
            &mut fake,
            &descriptor(),
            MicroProbeOptions {
                query: true,
                test_pattern: true,
                ..MicroProbeOptions::default()
            },
        );
        assert!(!result.probe_succeeded);
        assert!(result.report.led_test_attempted);
        assert!(!result.report.led_clear_acknowledged);
        assert_eq!(
            result.error.as_deref(),
            result.report.cleanup_error.as_deref()
        );
        assert!(
            result
                .report
                .cleanup_error
                .as_deref()
                .unwrap()
                .contains("No device response")
        );
        assert_eq!(fake.writes.len(), result.report.output_attempts);
    }

    #[test]
    fn timed_out_led_rpc_is_abandoned_only_for_shutdown_cleanup() {
        let pattern_reports = RpcClient::new()
            .begin_led(&diagnostic_frame())
            .unwrap()
            .reports
            .len();
        let shutdown_reports = RpcClient::new()
            .begin_led(&shutdown_frame())
            .unwrap()
            .reports
            .len();
        let cleanup_complete = 2 + pattern_reports + shutdown_reports;
        let mut fake = Fake {
            now: Duration::ZERO,
            incoming: VecDeque::new(),
            writes: Vec::new(),
            write_count: None,
            reply_after_writes: VecDeque::from([
                (1, response(101, json!({"version":"0.6.3"}))),
                (2, response(102, json!({"connected":true}))),
                (cleanup_complete, response(104, Value::Null)),
            ]),
            fail_read_after_writes: None,
            read_failure: None,
        };
        let result = run_qualified(
            &mut fake,
            &descriptor(),
            MicroProbeOptions {
                query: true,
                test_pattern: true,
                ..MicroProbeOptions::default()
            },
        );
        assert!(!result.probe_succeeded);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("No device response to v.oai.thstatus")
        );
        assert!(result.report.led_test_attempted);
        assert!(result.report.led_clear_acknowledged);
        assert!(result.report.cleanup_error.is_none());
        assert_eq!(fake.writes.len(), result.report.output_attempts);
    }

    #[test]
    fn old_firmware_stops_before_status_keymap_or_led_operations() {
        let mut fake = Fake {
            now: Duration::ZERO,
            incoming: VecDeque::new(),
            writes: Vec::new(),
            write_count: None,
            reply_after_writes: VecDeque::from([(1, response(101, json!({"version":"0.5.9"})))]),
            fail_read_after_writes: None,
            read_failure: None,
        };
        let result = run_qualified(
            &mut fake,
            &descriptor(),
            MicroProbeOptions {
                query: true,
                read_keymap: true,
                test_pattern: true,
                ..MicroProbeOptions::default()
            },
        );
        assert!(!result.probe_succeeded);
        assert_eq!(result.report.firmware.as_deref(), Some("0.5.9"));
        assert!(!result.report.firmware_reference_compatible);
        assert_eq!(result.report.output_attempts, 1);
        assert!(result.report.device_status.is_none());
        assert!(result.report.keymap.is_none());
        assert!(!result.report.led_test_attempted);
    }
}
