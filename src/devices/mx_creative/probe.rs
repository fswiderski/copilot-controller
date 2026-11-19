use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::native::hid::{
    HidBackend, HidBus, HidDescriptor, HidDevice, HidError, HidIdentity, HidMatcher, enumerate,
};

const VID: u16 = 0x046d;
const KEYPAD_PID: u16 = 0xc354;
const DIALPAD_PID: u16 = 0xbc00;
const VENDOR_PAGE: u16 = 0xff43;
const KEYPAD_USAGE: u16 = 0x1a02;
const DIALPAD_PRIMARY_PAGE: u16 = 0x0001;
const DIALPAD_PRIMARY_USAGE: u16 = 0x0002;
const GENERIC_DESKTOP_PAGE: u16 = 0x0001;
const SYSTEM_CONTROL_USAGE: u16 = 0x0080;
const CONSUMER_PAGE: u16 = 0x000c;
const CONSUMER_CONTROL_USAGE: u16 = 0x0001;
const KEYPAD_LOCATION_ID: u32 = 0x0211_0000;
const CAPTURE_LIMIT: usize = 64;
const READ_SLICE: Duration = Duration::from_millis(50);

const KEYPAD_DESCRIPTOR_HEX: &str = "0643ff0a021aa101851195137508150026ff000902810009029100c00643ff0a081aa1018513961f007508150026ff0009088202010908920201c00643ff0a101aa101851496fe0f7508150026ff000910920201c005010980a10185049501750215012503098209810983810075068103c0050c0901a101850595017501150025010a2402810295078103c0";
const DIALPAD_DESCRIPTOR_HEX: &str = "05010902a10185020901a1009510750115002501050919012910810295020501a4750c1600f826ff07093009318106b4950175081580257f09388106050c0a38028106c0c00643ff0a0202a101851195137508150026ff000902810009029100c0";
const KEYPAD_DESCRIPTOR_SHA256: &str =
    "d6faec8e21db00518c67b4446875b6bdb9d85988d05d888641da069fb302bc54";
const DIALPAD_DESCRIPTOR_SHA256: &str =
    "49ea87e24b5049ea9e7e01dd750754aad63f37f51ce8191fee1c1ff7ae5d2cac";
const KEYPAD_REPORT_SPECS: &[(u8, usize)] = &[(4, 2), (5, 2), (17, 20), (19, 32)];
const DIALPAD_REPORT_SPECS: &[(u8, usize)] = &[(2, 8), (17, 20)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub capture_ms: u32,
}

impl Options {
    pub fn validate(&self) -> std::result::Result<(), &'static str> {
        if self.capture_ms > 30_000 {
            return Err("capture_ms must be in 0..=30000");
        }
        Ok(())
    }

    pub fn budget(&self) -> Duration {
        Duration::from_millis(u64::from(self.capture_ms) + 5_000)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceEvidence {
    pub role: String,
    pub product: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub transport: String,
    pub primary_usage_page: u16,
    pub primary_usage: u16,
    pub max_input_report_len: usize,
    pub max_output_report_len: usize,
    pub location_id: Option<String>,
    pub registry_path: String,
    pub report_descriptor_sha256: String,
    pub report_ids: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DescriptorEvidence {
    pub role: String,
    pub selected_primary_usage_page: u16,
    pub selected_primary_usage: u16,
    pub max_input_report_len: usize,
    pub max_output_report_len: usize,
    pub location_id: Option<u32>,
    pub selected_path: String,
    pub source_path: String,
    pub same_service: bool,
    pub property_type: String,
    pub actual_length: usize,
    pub actual_sha256: String,
    pub expected_length: usize,
    pub expected_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub role: String,
    pub elapsed_ms: u64,
    pub report_id: u8,
    pub usage_page: u16,
    pub usage: u16,
    pub bytes: usize,
    pub hex: String,
    pub values: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Result {
    pub schema: u8,
    pub probe_succeeded: bool,
    pub read_only: bool,
    pub command_ready: bool,
    pub hardware_qualified: bool,
    pub devices: Vec<DeviceEvidence>,
    pub captures: Vec<Capture>,
    pub ignored_reports: usize,
    pub descriptor_mismatch: Option<DescriptorEvidence>,
    pub capture_truncated: bool,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

pub trait ProbeIo {
    fn now(&self) -> Duration;
    fn read(&mut self, role: Role, timeout: Duration) -> std::result::Result<Vec<u8>, HidError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Keypad,
    Dialpad,
}

#[derive(Debug)]
enum SelectionError {
    Message(String),
    Mismatch(Box<DescriptorEvidence>),
}

impl Role {
    const fn name(self) -> &'static str {
        match self {
            Self::Keypad => "keypad",
            Self::Dialpad => "dialpad",
        }
    }
}

struct ClockedHid<'a> {
    keypad: HidDevice,
    dialpad: HidDevice,
    started: std::time::Instant,
    _borrow: std::marker::PhantomData<&'a ()>,
}

impl ClockedHid<'_> {
    fn new(keypad: HidDevice, dialpad: HidDevice) -> Self {
        Self {
            keypad,
            dialpad,
            started: std::time::Instant::now(),
            _borrow: std::marker::PhantomData,
        }
    }
}

impl ProbeIo for ClockedHid<'_> {
    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    fn read(&mut self, role: Role, timeout: Duration) -> std::result::Result<Vec<u8>, HidError> {
        match role {
            Role::Keypad => self.keypad.read_report(32, timeout),
            Role::Dialpad => self.dialpad.read_report(20, timeout),
        }
    }
}

pub fn run(backend: &dyn HidBackend, options: Options) -> Result {
    if let Err(error) = options.validate() {
        return failure(error);
    }
    let keypad = select(
        backend,
        HidMatcher {
            identity: HidIdentity {
                vendor_id: VID,
                product_id: KEYPAD_PID,
            },
            usage_page: Some(VENDOR_PAGE),
            usage: Some(KEYPAD_USAGE),
        },
        Role::Keypad,
    );
    let dialpad = select(
        backend,
        HidMatcher {
            identity: HidIdentity {
                vendor_id: VID,
                product_id: DIALPAD_PID,
            },
            usage_page: Some(DIALPAD_PRIMARY_PAGE),
            usage: Some(DIALPAD_PRIMARY_USAGE),
        },
        Role::Dialpad,
    );
    let keypad = match keypad {
        Ok(value) => value,
        Err(error) => return selection_failure(error),
    };
    let dialpad = match dialpad {
        Ok(value) => value,
        Err(error) => return selection_failure(error),
    };
    let (keypad_device, keypad_evidence) = match open(backend, keypad, Role::Keypad) {
        Ok(value) => value,
        Err(error) => return failure(error),
    };
    let (dialpad_device, dialpad_evidence) = match open(backend, dialpad, Role::Dialpad) {
        Ok(value) => value,
        Err(error) => return failure(error),
    };
    let mut io = ClockedHid::new(keypad_device, dialpad_device);
    run_with_devices(&mut io, options, vec![keypad_evidence, dialpad_evidence])
}

fn select(
    backend: &dyn HidBackend,
    matcher: HidMatcher,
    role: Role,
) -> std::result::Result<HidDescriptor, SelectionError> {
    let found = enumerate(backend, &matcher, 2)
        .map_err(|error| SelectionError::Message(error.to_string()))?;
    if found.len() != 1 {
        return Err(SelectionError::Message(format!(
            "{} requires exactly one collection; found {}",
            role.name(),
            found.len()
        )));
    }
    let descriptor = found
        .into_iter()
        .next()
        .ok_or_else(|| SelectionError::Message("missing collection".to_owned()))?;
    let raw = backend
        .original_report_descriptor_evidence(&descriptor)
        .map_err(|error| SelectionError::Message(error.to_string()))?;
    verify_identity(&descriptor, role).map_err(SelectionError::Message)?;
    if verify_descriptor_bytes(&raw.bytes, role).is_err() {
        let (expected_length, expected_sha256) = expected_descriptor(role);
        let selected_path = descriptor.path.as_str().to_owned();
        let same_service = raw.source_path == selected_path;
        return Err(SelectionError::Mismatch(Box::new(DescriptorEvidence {
            role: role.name().into(),
            selected_primary_usage_page: descriptor.usage_page,
            selected_primary_usage: descriptor.usage,
            max_input_report_len: descriptor.max_input_report_len,
            max_output_report_len: descriptor.max_output_report_len,
            location_id: descriptor.location_id,
            selected_path,
            source_path: raw.source_path,
            same_service,
            property_type: raw.property_type,
            actual_length: raw.bytes.len(),
            actual_sha256: hash(&raw.bytes),
            expected_length,
            expected_sha256: expected_sha256.into(),
        })));
    }
    Ok(descriptor)
}

fn expected_descriptor(role: Role) -> (usize, &'static str) {
    match role {
        Role::Keypad => (
            decode_hex(KEYPAD_DESCRIPTOR_HEX).len(),
            KEYPAD_DESCRIPTOR_SHA256,
        ),
        Role::Dialpad => (
            decode_hex(DIALPAD_DESCRIPTOR_HEX).len(),
            DIALPAD_DESCRIPTOR_SHA256,
        ),
    }
}

fn selection_failure(error: SelectionError) -> Result {
    match error {
        SelectionError::Message(error) => failure(error),
        SelectionError::Mismatch(evidence) => Result {
            schema: 1,
            read_only: true,
            error: Some(format!("{} report descriptor mismatch", evidence.role)),
            descriptor_mismatch: Some(*evidence),
            ..Result::default()
        },
    }
}

fn open(
    backend: &dyn HidBackend,
    descriptor: HidDescriptor,
    role: Role,
) -> std::result::Result<(HidDevice, DeviceEvidence), String> {
    let specs = match role {
        Role::Keypad => KEYPAD_REPORT_SPECS,
        Role::Dialpad => DIALPAD_REPORT_SPECS,
    };
    let mut device =
        HidDevice::open_filtered(backend, &descriptor, specs).map_err(|error| error.to_string())?;
    let descriptor_hash = hash(
        &device
            .report_descriptor()
            .map_err(|error| error.to_string())?,
    );
    let expected_hash = match role {
        Role::Keypad => KEYPAD_DESCRIPTOR_SHA256,
        Role::Dialpad => DIALPAD_DESCRIPTOR_SHA256,
    };
    if descriptor_hash != expected_hash {
        return Err(format!("{} reopened descriptor mismatch", role.name()));
    }
    let evidence = DeviceEvidence {
        role: role.name().into(),
        product: descriptor
            .product
            .clone()
            .ok_or_else(|| format!("{} product name unavailable", role.name()))?,
        vendor_id: descriptor.identity.vendor_id,
        product_id: descriptor.identity.product_id,
        transport: bus_name(descriptor.bus)?.into(),
        primary_usage_page: descriptor.usage_page,
        primary_usage: descriptor.usage,
        max_input_report_len: descriptor.max_input_report_len,
        max_output_report_len: descriptor.max_output_report_len,
        location_id: descriptor.location_id.map(|id| format!("0x{id:08x}")),
        registry_path: descriptor.path.as_str().to_owned(),
        report_descriptor_sha256: descriptor_hash,
        report_ids: specs.iter().map(|spec| spec.0).collect(),
    };
    Ok((device, evidence))
}

fn verify_identity(descriptor: &HidDescriptor, role: Role) -> std::result::Result<(), String> {
    let expected = match role {
        Role::Keypad => (
            KEYPAD_PID,
            VENDOR_PAGE,
            KEYPAD_USAGE,
            32,
            4095,
            HidBus::Usb,
            "MX Creative Keypad",
            Some(KEYPAD_LOCATION_ID),
        ),
        Role::Dialpad => (
            DIALPAD_PID,
            DIALPAD_PRIMARY_PAGE,
            DIALPAD_PRIMARY_USAGE,
            20,
            20,
            HidBus::Bluetooth,
            "MX Dialpad",
            None,
        ),
    };
    if descriptor.identity.vendor_id != VID
        || descriptor.identity.product_id != expected.0
        || descriptor.usage_page != expected.1
        || descriptor.usage != expected.2
        || descriptor.max_input_report_len != expected.3
        || descriptor.max_output_report_len != expected.4
        || descriptor.bus != Some(expected.5)
        || descriptor.product.as_deref() != Some(expected.6)
        || (role == Role::Keypad && descriptor.location_id != expected.7)
    {
        return Err(format!("{} identity or transport mismatch", role.name()));
    }
    Ok(())
}

fn verify_descriptor_bytes(raw: &[u8], role: Role) -> std::result::Result<(), String> {
    let expected_hex = match role {
        Role::Keypad => KEYPAD_DESCRIPTOR_HEX,
        Role::Dialpad => DIALPAD_DESCRIPTOR_HEX,
    };
    if raw != decode_hex(expected_hex) {
        return Err(format!("{} report descriptor mismatch", role.name()));
    }
    Ok(())
}

fn run_with_devices(
    io: &mut dyn ProbeIo,
    options: Options,
    devices: Vec<DeviceEvidence>,
) -> Result {
    let started = io.now();
    let deadline = started.saturating_add(Duration::from_millis(u64::from(options.capture_ms)));
    let mut result = Result {
        schema: 1,
        read_only: true,
        devices,
        ..Result::default()
    };
    while io.now() < deadline && result.captures.len() < CAPTURE_LIMIT {
        for role in [Role::Keypad, Role::Dialpad] {
            if io.now() >= deadline || result.captures.len() >= CAPTURE_LIMIT {
                break;
            }
            let remaining = deadline.saturating_sub(io.now());
            let before = io.now();
            match io.read(role, READ_SLICE.min(remaining)) {
                Ok(report) => {
                    if let Some(capture) = capture(role, &report, elapsed_ms(io.now(), started)) {
                        result.captures.push(capture);
                    } else {
                        result.ignored_reports += 1;
                    }
                }
                Err(HidError::Timeout) => {
                    if io.now() <= before && remaining > Duration::ZERO {
                        result.error = Some("HID clock did not advance".into());
                        return result;
                    }
                }
                Err(error) => {
                    result.error = Some(error.to_string());
                    result.elapsed_ms = elapsed_ms(io.now(), started);
                    return result;
                }
            }
        }
    }
    result.capture_truncated = result.captures.len() == CAPTURE_LIMIT && io.now() < deadline;
    result.elapsed_ms = elapsed_ms(io.now(), started);
    result.probe_succeeded = result.error.is_none();
    result
}

fn capture(role: Role, report: &[u8], elapsed_ms: u64) -> Option<Capture> {
    let (report_id, values) = report.split_first()?;
    let (usage_page, usage, expected) = match (role, *report_id) {
        (Role::Keypad, 4) => (GENERIC_DESKTOP_PAGE, SYSTEM_CONTROL_USAGE, 1),
        (Role::Keypad, 5) => (CONSUMER_PAGE, CONSUMER_CONTROL_USAGE, 1),
        (Role::Keypad, 17) => (VENDOR_PAGE, 2, 19),
        (Role::Keypad, 19) => (VENDOR_PAGE, 8, 31),
        (Role::Dialpad, 2) => (GENERIC_DESKTOP_PAGE, DIALPAD_PRIMARY_USAGE, 7),
        (Role::Dialpad, 17) => (VENDOR_PAGE, 2, 19),
        _ => return None,
    };
    if values.len() != expected {
        return None;
    }
    Some(Capture {
        role: role.name().into(),
        elapsed_ms,
        report_id: *report_id,
        usage_page,
        usage,
        bytes: report.len(),
        hex: encode_hex(report),
        values: values.to_vec(),
    })
}

fn failure(error: impl Into<String>) -> Result {
    Result {
        schema: 1,
        read_only: true,
        error: Some(error.into()),
        ..Result::default()
    }
}

fn bus_name(bus: Option<HidBus>) -> std::result::Result<&'static str, String> {
    match bus {
        Some(HidBus::Usb) => Ok("usb"),
        Some(HidBus::Bluetooth) => Ok("bluetooth"),
        _ => Err("HID transport unavailable".into()),
    }
}

fn elapsed_ms(now: Duration, started: Duration) -> u64 {
    now.saturating_sub(started)
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn hash(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
}

fn decode_hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let nibble = |b: u8| match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                _ => unreachable!("compile-time descriptor"),
            };
            (nibble(pair[0]) << 4) | nibble(pair[1])
        })
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::native::hid::fake::FakeBackend;

    #[test]
    fn descriptors_are_exact_and_report_ids_are_bounded() {
        let keypad = decode_hex(KEYPAD_DESCRIPTOR_HEX);
        assert_eq!(keypad[122..128], [0x95, 0x01, 0x75, 0x01, 0x15, 0x00]);
        assert_eq!(keypad[128..132], [0x25, 0x01, 0x0a, 0x24]);
        assert!(
            keypad
                .windows(16)
                .any(|bytes| { bytes == decode_hex("85049501750215012503098209810983") })
        );
        assert!(
            keypad
                .windows(15)
                .any(|bytes| { bytes == decode_hex("850595017501150025010a24028102") })
        );
        assert_eq!(hash(&keypad), KEYPAD_DESCRIPTOR_SHA256);
        assert_eq!(
            hash(&decode_hex(DIALPAD_DESCRIPTOR_HEX)),
            DIALPAD_DESCRIPTOR_SHA256
        );
        assert!(
            decode_hex(DIALPAD_DESCRIPTOR_HEX)
                .windows(22)
                .any(|bytes| bytes == decode_hex("85020901a10095107501150025010509190129108102"))
        );
        assert_eq!(KEYPAD_REPORT_SPECS, [(4, 2), (5, 2), (17, 20), (19, 32)]);
        assert_eq!(DIALPAD_REPORT_SPECS, [(2, 8), (17, 20)]);
        let mut report = vec![17];
        report.extend([0x5a; 19]);
        let observed = capture(Role::Keypad, &report, 3).unwrap();
        assert_eq!(observed.report_id, 17);
        assert_eq!(observed.values, vec![0x5a; 19]);
        assert_eq!(observed.bytes, 20);
        for (role, report) in [
            (Role::Keypad, vec![4; 1]),
            (Role::Keypad, vec![4; 3]),
            (Role::Keypad, vec![5; 3]),
            (Role::Keypad, vec![17; 19]),
            (Role::Keypad, vec![17; 21]),
            (Role::Keypad, vec![4; 3]),
            (Role::Dialpad, vec![2; 7]),
            (Role::Dialpad, vec![2; 9]),
            (Role::Dialpad, vec![19; 32]),
            (Role::Dialpad, vec![20; 20]),
        ] {
            assert!(capture(role, &report, 0).is_none());
        }
        for (role, report, page, usage) in [
            (Role::Keypad, vec![4, 3], 1, 0x80),
            (Role::Keypad, vec![5, 2], 0x0c, 1),
            (Role::Dialpad, vec![2, 0, 0, 0, 0, 0, 1, 2], 1, 2),
        ] {
            let observed = capture(role, &report, 0).unwrap();
            assert_eq!((observed.usage_page, observed.usage), (page, usage));
        }
    }

    #[derive(Default)]
    struct FakeIo {
        now: Duration,
        reads: VecDeque<(Role, std::result::Result<Vec<u8>, HidError>)>,
    }

    impl ProbeIo for FakeIo {
        fn now(&self) -> Duration {
            self.now
        }

        fn read(
            &mut self,
            role: Role,
            timeout: Duration,
        ) -> std::result::Result<Vec<u8>, HidError> {
            let next = self
                .reads
                .pop_front()
                .unwrap_or((role, Err(HidError::Timeout)));
            if next.0 != role {
                panic!("unexpected read role");
            }
            self.now += timeout;
            next.1
        }
    }

    #[test]
    fn collection_rejects_wrong_lengths_without_exposing_them() {
        let mut io = FakeIo::default();
        io.reads.push_back((Role::Keypad, Ok(vec![17; 5])));
        io.reads.push_back((Role::Dialpad, Err(HidError::Timeout)));
        let result = run_with_devices(&mut io, Options { capture_ms: 50 }, vec![]);
        assert!(result.probe_succeeded);
        assert!(result.captures.is_empty());
        assert_eq!(result.ignored_reports, 1);
    }

    #[test]
    fn identity_checks_transport_descriptor_size_location_and_multiplicity_metadata() {
        let base = HidDescriptor {
            identity: HidIdentity {
                vendor_id: VID,
                product_id: KEYPAD_PID,
            },
            usage_page: VENDOR_PAGE,
            usage: KEYPAD_USAGE,
            product: Some("MX Creative Keypad".into()),
            serial: Some("private".into()),
            max_input_report_len: 32,
            max_output_report_len: 4095,
            interface_number: None,
            bus: Some(HidBus::Usb),
            location_id: Some(KEYPAD_LOCATION_ID),
            path: crate::native::hid::HidPath::new("iokit:1").unwrap(),
        };
        assert!(verify_identity(&base, Role::Keypad).is_ok());
        let mut changed = base.clone();
        changed.location_id = Some(KEYPAD_LOCATION_ID + 1);
        assert!(verify_identity(&changed, Role::Keypad).is_err());
        changed = base;
        changed.bus = Some(HidBus::Bluetooth);
        assert!(verify_identity(&changed, Role::Keypad).is_err());
        assert!(verify_descriptor_bytes(&decode_hex(KEYPAD_DESCRIPTOR_HEX), Role::Keypad).is_ok());
        assert!(verify_descriptor_bytes(&[0], Role::Keypad).is_err());
    }

    #[test]
    fn selection_rejects_multiple_matching_collections() {
        let descriptor = HidDescriptor {
            identity: HidIdentity {
                vendor_id: VID,
                product_id: KEYPAD_PID,
            },
            usage_page: VENDOR_PAGE,
            usage: KEYPAD_USAGE,
            product: Some("MX Creative Keypad".into()),
            serial: None,
            max_input_report_len: 32,
            max_output_report_len: 4095,
            interface_number: None,
            bus: Some(HidBus::Usb),
            location_id: Some(KEYPAD_LOCATION_ID),
            path: crate::native::hid::HidPath::new("iokit:1").unwrap(),
        };
        let matcher = HidMatcher {
            identity: descriptor.identity,
            usage_page: Some(VENDOR_PAGE),
            usage: Some(KEYPAD_USAGE),
        };
        let backend = FakeBackend::new(vec![descriptor.clone(), descriptor]);
        backend.state.borrow_mut().report_descriptor = Some(decode_hex(KEYPAD_DESCRIPTOR_HEX));
        let SelectionError::Message(error) = select(&backend, matcher, Role::Keypad).unwrap_err()
        else {
            panic!("expected count failure");
        };
        assert!(error.contains("exactly one"));
        assert_eq!(backend.state.borrow().open_calls, 0);
    }

    #[test]
    fn descriptor_mismatch_reports_hash_length_source_and_no_bytes() {
        let descriptor = HidDescriptor {
            identity: HidIdentity {
                vendor_id: VID,
                product_id: KEYPAD_PID,
            },
            usage_page: VENDOR_PAGE,
            usage: KEYPAD_USAGE,
            product: Some("MX Creative Keypad".into()),
            serial: None,
            max_input_report_len: 32,
            max_output_report_len: 4095,
            interface_number: None,
            bus: Some(HidBus::Usb),
            location_id: Some(KEYPAD_LOCATION_ID),
            path: crate::native::hid::HidPath::new("iokit:7").unwrap(),
        };
        let matcher = HidMatcher {
            identity: descriptor.identity,
            usage_page: Some(VENDOR_PAGE),
            usage: Some(KEYPAD_USAGE),
        };
        let backend = FakeBackend::new(vec![descriptor]);
        backend.state.borrow_mut().report_descriptor = Some(vec![0x05, 0x01, 0x09, 0x02]);
        let SelectionError::Mismatch(evidence) =
            select(&backend, matcher, Role::Keypad).unwrap_err()
        else {
            panic!("expected descriptor mismatch");
        };
        assert_eq!(evidence.selected_path, "iokit:7");
        assert_eq!(evidence.source_path, "iokit:7");
        assert!(evidence.same_service);
        assert_eq!(evidence.property_type, "backend");
        assert_eq!(evidence.actual_length, 4);
        assert_eq!(evidence.actual_sha256, hash(&[0x05, 0x01, 0x09, 0x02]));
        assert_eq!(evidence.expected_length, 140);
        assert_eq!(evidence.expected_sha256, KEYPAD_DESCRIPTOR_SHA256);
        assert!(
            !serde_json::to_string(&evidence)
                .unwrap()
                .contains("05010902")
        );
    }
}
