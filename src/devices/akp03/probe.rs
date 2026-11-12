use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::native::hid::{
    EvidenceKind, HidBackend, HidBus, HidDescriptor, HidDevice, HidError, HidIdentity, HidMatcher,
    enumerate,
};

use super::{
    Profile,
    image::{diagnostic_tile_rgb, rgb_image_reports},
    input::{DecodedInput, decode_report},
    output::{self, Command, OutputReport, REPORT_LEN},
    profile::validate_rev2_report_descriptor,
};

const VENDOR_ID: u16 = 0x0300;
const PRODUCT_ID: u16 = 0x3002;
const USAGE_PAGE: u16 = 0xffa0;
const USAGE: u16 = 1;
const CAPTURE_BUDGET: usize = 128;
const CAPTURE_BUFFER: usize = 513;
const READ_SLICE: Duration = Duration::from_millis(50);
const CONNECT_INTERVAL: Duration = Duration::from_secs(5);

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Akp03ProbeOptions {
    #[serde(default)]
    pub capture_ms: u32,
    #[serde(default)]
    pub initialize: bool,
    #[serde(default)]
    pub test_pattern: bool,
    #[serde(default)]
    pub health_check: bool,
}

impl Akp03ProbeOptions {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.capture_ms > 30_000 {
            return Err("capture_ms must be in 0..=30000");
        }
        if self.test_pattern && !self.initialize {
            return Err("test_pattern requires initialize");
        }
        if self.health_check && (self.initialize || self.test_pattern) {
            return Err("health_check conflicts with initialize and test_pattern");
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
    #[serde(deserialize_with = "required_nullable")]
    pub transport: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub elapsed_ms: u64,
    pub bytes: usize,
    pub hex: String,
    #[serde(deserialize_with = "required_nullable")]
    pub decoded: Option<DecodedInput>,
    #[serde(deserialize_with = "required_nullable")]
    pub decode_error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    #[serde(deserialize_with = "required_nullable")]
    pub candidate: Option<Candidate>,
    pub opened: bool,
    pub descriptor_verified: bool,
    pub phase: String,
    pub initialized: bool,
    pub output_reports: usize,
    pub output_bytes: usize,
    pub patterns_written: usize,
    pub captures: Vec<Capture>,
    pub capture_truncated: bool,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Akp03ProbeResult {
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

pub trait ProbeIo {
    fn now(&self) -> Duration;
    fn write(&mut self, report: &[u8]) -> Result<usize, HidError>;
    fn read(&mut self, max_len: usize, timeout: Duration) -> Result<Vec<u8>, HidError>;
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

/// Health check deliberately has no backend parameter and performs no enumeration/open.
pub fn akp03_health_check() -> Akp03ProbeResult {
    Akp03ProbeResult {
        probe_succeeded: true,
        read_only: true,
        report: Progress {
            phase: "health_check".to_owned(),
            ..Progress::default()
        },
        ..Akp03ProbeResult::default()
    }
}

/// Invoke only from a disposable, supervised worker. Enumeration/open and any
/// subsequent writes are synchronous backend operations; containment is external.
pub fn run_akp03_probe(backend: &dyn HidBackend, options: Akp03ProbeOptions) -> Akp03ProbeResult {
    if let Err(error) = options.validate() {
        return failed(error, !options.initialize);
    }
    if options.health_check {
        return akp03_health_check();
    }
    let matcher = HidMatcher {
        identity: HidIdentity {
            vendor_id: VENDOR_ID,
            product_id: PRODUCT_ID,
        },
        usage_page: Some(USAGE_PAGE),
        usage: Some(USAGE),
    };
    let candidates = match enumerate(backend, &matcher, 64) {
        Ok(candidates) => candidates,
        Err(error) => return failed(error.to_string(), !options.initialize),
    };
    if candidates.len() != 1 {
        return failed(
            format!(
                "AKP03 requires exactly one candidate; found {}",
                candidates.len()
            ),
            !options.initialize,
        );
    }
    let selected = &candidates[0];
    if !identity_matches(selected) {
        return failed("AKP03 identity mismatch".to_owned(), !options.initialize);
    }
    if selected.interface_number != Some(0) {
        return failed(
            HidError::EvidenceUnavailable(EvidenceKind::InterfaceNumber).to_string(),
            !options.initialize,
        );
    }
    let mut device = match HidDevice::open(backend, selected) {
        Ok(device) => device,
        Err(error) => return failed(error.to_string(), !options.initialize),
    };
    let report_descriptor = match device.report_descriptor() {
        Ok(bytes) => bytes,
        Err(error) => {
            let mut result = failed(error.to_string(), !options.initialize);
            result.report.candidate = Some(candidate(selected));
            result.report.opened = true;
            return result;
        }
    };
    let mut io = ClockedHid::new(&mut device);
    run_akp03_with_evidence(&mut io, selected, &report_descriptor, options)
}

/// Synthetic entry point: both raw descriptor bytes and identity metadata are
/// explicit, so tests cannot silently substitute parsed descriptor semantics.
pub fn run_akp03_with_evidence(
    io: &mut dyn ProbeIo,
    descriptor: &HidDescriptor,
    report_descriptor: &[u8],
    options: Akp03ProbeOptions,
) -> Akp03ProbeResult {
    if let Err(error) = options.validate() {
        return failed(error, !options.initialize);
    }
    if options.health_check {
        return akp03_health_check();
    }

    let mut result = Akp03ProbeResult {
        read_only: !options.initialize,
        ..Akp03ProbeResult::default()
    };
    result.report.candidate = Some(candidate(descriptor));
    if !identity_matches(descriptor) {
        result.error = Some("AKP03 identity mismatch".to_owned());
        return result;
    }
    if descriptor.interface_number != Some(0) {
        result.error =
            Some(HidError::EvidenceUnavailable(EvidenceKind::InterfaceNumber).to_string());
        return result;
    }
    result.report.opened = true;
    if let Err(error) = validate_rev2_report_descriptor(report_descriptor) {
        result.error = Some(error.to_string());
        return result;
    }
    result.report.descriptor_verified = true;
    let started = io.now();

    let run = run_sequence(io, &options, &mut result.report, started);
    result.report.elapsed_ms = elapsed_ms(io.now(), started);
    match run {
        Ok(()) => {
            result.probe_succeeded = true;
            result.report.phase = "complete".to_owned();
        }
        Err(error) => {
            result.error = Some(error);
            result.probe_succeeded = false;
        }
    }
    result
}

fn run_sequence(
    io: &mut dyn ProbeIo,
    options: &Akp03ProbeOptions,
    report: &mut Progress,
    started: Duration,
) -> Result<(), String> {
    if options.initialize {
        report.phase = "initialize".to_owned();
        write_output(
            io,
            report,
            &output::command_report(Command::Initialize).unwrap(),
        )?;
        write_output(
            io,
            report,
            &output::command_report(Command::Brightness(50)).unwrap(),
        )?;
        report.initialized = true;

        write_output(
            io,
            report,
            &output::command_report(Command::KeepAlive).unwrap(),
        )?;
        if options.test_pattern {
            report.phase = "display".to_owned();
            write_output(
                io,
                report,
                &output::command_report(Command::ClearVolatileScene).unwrap(),
            )?;
            write_output(io, report, &output::command_report(Command::Flush).unwrap())?;
            for key in 0..output::KEY_COUNT {
                let rgb =
                    diagnostic_tile_rgb(Profile::Rev2, key).map_err(|error| error.to_string())?;
                let transaction = rgb_image_reports(Profile::Rev2, key, &rgb)
                    .map_err(|error| error.to_string())?;
                for packet in &transaction {
                    write_output(io, report, packet)?;
                }
                report.patterns_written += 1;
            }
        }
    }

    report.phase = "capture".to_owned();
    let capture_started = io.now();
    let deadline = capture_started
        .checked_add(Duration::from_millis(u64::from(options.capture_ms)))
        .ok_or_else(|| "AKP03 clock out of range".to_owned())?;
    let mut next_connect = options
        .initialize
        .then(|| capture_started.saturating_add(CONNECT_INTERVAL));
    loop {
        let now = io.now();
        if now >= deadline {
            break;
        }
        if report.captures.len() == CAPTURE_BUDGET {
            report.capture_truncated = true;
            break;
        }
        if next_connect.is_some_and(|at| now >= at) {
            write_output(
                io,
                report,
                &output::command_report(Command::KeepAlive).unwrap(),
            )?;
            next_connect = Some(now.saturating_add(CONNECT_INTERVAL));
            continue;
        }
        let remaining = deadline.saturating_sub(now);
        let until_connect = next_connect.map_or(remaining, |at| at.saturating_sub(now));
        let timeout = READ_SLICE.min(remaining).min(until_connect);
        let before = io.now();
        let bytes = match io.read(CAPTURE_BUFFER, timeout) {
            Ok(bytes) => bytes,
            Err(HidError::Timeout) => {
                if io.now() <= before {
                    return Err("AKP03 clock did not advance after read timeout".to_owned());
                }
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        if bytes.len() > 512 {
            return Err("AKP03 input report exceeds 512 bytes".to_owned());
        }
        if bytes.is_empty() {
            if io.now() <= before && timeout > Duration::ZERO {
                return Err("AKP03 clock did not advance during idle read".to_owned());
            }
            continue;
        }
        let (decoded, decode_error) = match decode_report(Profile::Rev2, &bytes) {
            Ok(decoded) => (Some(decoded), None),
            Err(error) => (None, Some(error.to_string())),
        };
        report.captures.push(Capture {
            elapsed_ms: elapsed_ms(io.now(), started),
            bytes: bytes.len(),
            hex: hex(&bytes),
            decoded,
            decode_error,
        });
    }
    Ok(())
}

fn write_output(
    io: &mut dyn ProbeIo,
    report: &mut Progress,
    packet: &OutputReport,
) -> Result<(), String> {
    debug_assert_eq!(packet.len(), REPORT_LEN);
    let count = match io.write(packet) {
        Ok(count) => count,
        Err(error) => return Err(error.to_string()),
    };
    report.output_bytes = report.output_bytes.saturating_add(count);
    if count != REPORT_LEN {
        return Err(format!("AKP03 short HID write: {count}"));
    }
    report.output_reports += 1;
    Ok(())
}

fn identity_matches(descriptor: &HidDescriptor) -> bool {
    descriptor.identity
        == (HidIdentity {
            vendor_id: VENDOR_ID,
            product_id: PRODUCT_ID,
        })
        && descriptor.usage_page == USAGE_PAGE
        && descriptor.usage == USAGE
}

fn candidate(descriptor: &HidDescriptor) -> Candidate {
    Candidate {
        vendor_id: descriptor.identity.vendor_id,
        product_id: descriptor.identity.product_id,
        usage_page: descriptor.usage_page,
        usage: descriptor.usage,
        interface_number: descriptor.interface_number,
        transport: descriptor.bus.map(|bus| {
            match bus {
                HidBus::Usb => "usb",
                HidBus::Bluetooth => "bluetooth",
                HidBus::Unknown => "unknown",
            }
            .to_owned()
        }),
    }
}

fn failed(error: impl Into<String>, read_only: bool) -> Akp03ProbeResult {
    Akp03ProbeResult {
        read_only,
        error: Some(error.into()),
        ..Akp03ProbeResult::default()
    }
}

fn elapsed_ms(now: Duration, started: Duration) -> u64 {
    now.saturating_sub(started)
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::devices::akp03::profile::REV2_REPORT_DESCRIPTOR;
    use crate::native::hid::fake::FakeBackend;

    struct Fake {
        now: Duration,
        reads: VecDeque<Result<Vec<u8>, HidError>>,
        writes: Vec<Vec<u8>>,
        write_count: Option<usize>,
    }

    impl ProbeIo for Fake {
        fn now(&self) -> Duration {
            self.now
        }

        fn write(&mut self, report: &[u8]) -> Result<usize, HidError> {
            self.writes.push(report.to_vec());
            Ok(self.write_count.unwrap_or(report.len()))
        }

        fn read(&mut self, max_len: usize, timeout: Duration) -> Result<Vec<u8>, HidError> {
            match self.reads.pop_front() {
                Some(Ok(report)) => Ok(report[..report.len().min(max_len)].to_vec()),
                Some(Err(error)) => {
                    self.now += timeout;
                    Err(error)
                }
                None => {
                    self.now += timeout;
                    Err(HidError::Timeout)
                }
            }
        }
    }

    fn descriptor() -> HidDescriptor {
        HidDescriptor {
            identity: HidIdentity {
                vendor_id: VENDOR_ID,
                product_id: PRODUCT_ID,
            },
            usage_page: USAGE_PAGE,
            usage: USAGE,
            product: None,
            serial: None,
            max_input_report_len: 513,
            max_output_report_len: REPORT_LEN,
            interface_number: Some(0),
            bus: Some(HidBus::Usb),
            path: crate::native::hid::HidPath::new("fake-akp03").unwrap(),
        }
    }

    #[test]
    fn backend_health_path_does_not_enumerate_or_open() {
        let backend = FakeBackend::new(vec![descriptor()]);
        let result = run_akp03_probe(
            &backend,
            Akp03ProbeOptions {
                health_check: true,
                ..Akp03ProbeOptions::default()
            },
        );
        assert!(result.probe_succeeded);
        assert_eq!(backend.state.borrow().enumerate_calls, 0);
        assert_eq!(backend.state.borrow().open_calls, 0);
    }

    #[test]
    fn missing_native_descriptor_is_typed_and_fails_after_open() {
        let backend = FakeBackend::new(vec![descriptor()]);
        let result = run_akp03_probe(&backend, Akp03ProbeOptions::default());
        assert!(!result.probe_succeeded);
        assert!(result.report.opened);
        assert!(!result.report.descriptor_verified);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("EvidenceUnavailable")
        );
        assert_eq!(backend.state.borrow().open_calls, 1);
    }

    #[test]
    fn backend_opens_only_the_exact_identity_and_descriptor() {
        let backend = FakeBackend::new(vec![descriptor()]);
        backend.state.borrow_mut().report_descriptor = Some(REV2_REPORT_DESCRIPTOR.to_vec());
        let result = run_akp03_probe(&backend, Akp03ProbeOptions::default());
        assert!(result.probe_succeeded, "{:?}", result.error);
        assert!(result.report.opened && result.report.descriptor_verified);
        assert_eq!(backend.state.borrow().enumerate_calls, 1);
        assert_eq!(backend.state.borrow().open_calls, 1);
        assert!(backend.state.borrow().written.is_empty());
    }

    fn fake() -> Fake {
        Fake {
            now: Duration::ZERO,
            reads: VecDeque::new(),
            writes: Vec::new(),
            write_count: None,
        }
    }

    #[test]
    fn health_check_does_not_need_transport_and_options_fail_before_io() {
        let health = akp03_health_check();
        assert!(health.probe_succeeded && health.read_only);
        assert_eq!(health.report.phase, "health_check");
        assert!(!health.command_ready && !health.hardware_qualified);
        let mut io = fake();
        let result = run_akp03_with_evidence(
            &mut io,
            &descriptor(),
            &REV2_REPORT_DESCRIPTOR,
            Akp03ProbeOptions {
                test_pattern: true,
                ..Akp03ProbeOptions::default()
            },
        );
        assert!(!result.probe_succeeded);
        assert!(io.writes.is_empty());
        assert_eq!(io.now, Duration::ZERO);
        assert!(
            Akp03ProbeOptions {
                capture_ms: 30_000,
                ..Akp03ProbeOptions::default()
            }
            .validate()
            .is_ok()
        );
        assert!(
            Akp03ProbeOptions {
                capture_ms: 30_001,
                ..Akp03ProbeOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Akp03ProbeOptions {
                health_check: true,
                initialize: true,
                ..Akp03ProbeOptions::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn passive_zero_duration_verifies_identity_without_reports() {
        let mut io = fake();
        let result = run_akp03_with_evidence(
            &mut io,
            &descriptor(),
            &REV2_REPORT_DESCRIPTOR,
            Akp03ProbeOptions::default(),
        );
        assert!(result.probe_succeeded);
        assert!(result.read_only);
        assert!(result.report.opened && result.report.descriptor_verified);
        assert_eq!(result.report.phase, "complete");
        assert!(io.writes.is_empty());
        assert!(io.reads.is_empty());
    }

    #[test]
    fn initialized_pattern_is_exactly_sequenced_and_counts_six_completed_keys() {
        let mut io = fake();
        let result = run_akp03_with_evidence(
            &mut io,
            &descriptor(),
            &REV2_REPORT_DESCRIPTOR,
            Akp03ProbeOptions {
                initialize: true,
                test_pattern: true,
                capture_ms: 0,
                ..Akp03ProbeOptions::default()
            },
        );
        assert!(result.probe_succeeded, "{:?}", result.error);
        assert!(!result.read_only);
        assert!(result.report.initialized);
        assert_eq!(result.report.patterns_written, 6);
        assert_eq!(result.report.output_reports, io.writes.len());
        assert_eq!(&io.writes[0][6..9], b"DIS");
        assert_eq!(&io.writes[1][6..9], b"LIG");
        assert_eq!(&io.writes[2][6..13], b"CONNECT");
        assert_eq!(&io.writes[3][6..9], b"CLE");
        assert_eq!(&io.writes[4][6..9], b"STP");
        let keys: Vec<u8> = io.writes[5..]
            .iter()
            .filter(|packet| &packet[6..9] == b"BAT")
            .map(|packet| packet[13])
            .collect();
        assert_eq!(keys, [1, 2, 3, 4, 5, 6]);
        assert!(io.writes.iter().all(|packet| packet.len() == REPORT_LEN));
        assert_eq!(io.writes.len(), result.report.output_reports);
        assert!(!result.command_ready && !result.hardware_qualified);
    }

    #[test]
    fn descriptor_mismatch_and_short_write_fail_without_retry_or_cleanup() {
        let mut io = fake();
        let mut changed = REV2_REPORT_DESCRIPTOR.to_vec();
        changed[0] ^= 1;
        let mismatch = run_akp03_with_evidence(
            &mut io,
            &descriptor(),
            &changed,
            Akp03ProbeOptions::default(),
        );
        assert!(!mismatch.probe_succeeded);
        assert!(mismatch.report.opened);
        assert!(!mismatch.report.descriptor_verified);
        assert!(io.writes.is_empty());

        io.write_count = Some(REPORT_LEN - 1);
        let short = run_akp03_with_evidence(
            &mut io,
            &descriptor(),
            &REV2_REPORT_DESCRIPTOR,
            Akp03ProbeOptions {
                initialize: true,
                ..Akp03ProbeOptions::default()
            },
        );
        assert!(!short.probe_succeeded);
        assert_eq!(short.report.output_reports, 0);
        assert_eq!(io.writes.len(), 1);
        assert!(!short.report.initialized);
    }

    #[test]
    fn malformed_capture_is_retained_and_oversized_capture_fails() {
        let mut io = fake();
        io.reads.push_back(Ok(vec![0; 20]));
        let result = run_akp03_with_evidence(
            &mut io,
            &descriptor(),
            &REV2_REPORT_DESCRIPTOR,
            Akp03ProbeOptions {
                capture_ms: 50,
                ..Akp03ProbeOptions::default()
            },
        );
        assert!(result.probe_succeeded);
        assert_eq!(result.report.captures.len(), 1);
        assert!(result.report.captures[0].decode_error.is_some());

        let mut io = fake();
        io.reads.push_back(Ok(vec![0; 513]));
        let result = run_akp03_with_evidence(
            &mut io,
            &descriptor(),
            &REV2_REPORT_DESCRIPTOR,
            Akp03ProbeOptions {
                capture_ms: 50,
                ..Akp03ProbeOptions::default()
            },
        );
        assert!(!result.probe_succeeded);
        assert!(result.error.as_deref().unwrap().contains("exceeds 512"));
        assert!(result.report.captures.is_empty());
    }

    #[test]
    fn capture_bound_marks_truncation_without_extra_reads() {
        let mut io = fake();
        io.reads.extend((0..129).map(|_| Ok(vec![0; 20])));
        let result = run_akp03_with_evidence(
            &mut io,
            &descriptor(),
            &REV2_REPORT_DESCRIPTOR,
            Akp03ProbeOptions {
                capture_ms: 1000,
                ..Akp03ProbeOptions::default()
            },
        );
        assert!(result.probe_succeeded);
        assert!(result.report.capture_truncated);
        assert_eq!(result.report.captures.len(), 128);
        assert_eq!(io.reads.len(), 1);
    }
}
