use std::time::Duration;

use serde_json::Value;

use super::FirmwareError;
use super::artifacts::sha256;
use super::backend::{
    HidCandidate, HidOpened, HidTransport, MaintenanceBackend, RegistryNode, RegistryQuery,
    RegistrySnapshot,
};

const APPLICATION_VID: u16 = 0x303a;
const APPLICATION_PID: u16 = 0x8298;
const BOOTLOADER_PID: u16 = 0x1001;
const MAX_REGISTRY_DEPTH: usize = 32;
const OUTPUT_LIMIT: usize = 4_194_304;
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_BUDGET: Duration = Duration::from_secs(30);
const VERSION_BUDGET: Duration = Duration::from_secs(4);
const READ_SLICE: Duration = Duration::from_millis(50);
const MAX_NONEMPTY_READS: usize = 128;
const MAX_JSON: usize = 1_048_576;
const MAX_JSON_AGE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
pub(crate) struct DescriptorPin<'a> {
    pub(crate) interface: i32,
    pub(crate) length: usize,
    pub(crate) sha256: &'a str,
}

pub(crate) const USB_DESCRIPTOR_PINS: &[DescriptorPin<'static>] = &[
    DescriptorPin {
        interface: 0,
        length: 275,
        sha256: "9257d7361f9c784e0fc0b260bbac0feadd49bf79cbb6202d6c41560cbae96fb6",
    },
    DescriptorPin {
        interface: 2,
        length: 36,
        sha256: "08229e86dff4b74981ad1c14f06deaee61a6d41d147273f2cdde860ea435986b",
    },
];

fn backend_error(error: impl std::fmt::Display) -> FirmwareError {
    FirmwareError::new(error.to_string())
}

fn registry<B: MaintenanceBackend>(
    backend: &mut B,
    timeout: Duration,
) -> Result<RegistrySnapshot, FirmwareError> {
    backend.check_cancelled().map_err(backend_error)?;
    let query = RegistryQuery {
        timeout: timeout.min(REGISTRY_TIMEOUT),
        stdout_limit: OUTPUT_LIMIT,
        stderr_limit: OUTPUT_LIMIT,
        clear_environment: true,
        require_top_level_array: true,
    };
    backend.registry(&query).map_err(backend_error)
}

fn collect_nodes<'a>(
    node: &'a RegistryNode,
    depth: usize,
    nodes: &mut Vec<&'a RegistryNode>,
) -> Result<(), FirmwareError> {
    if depth > MAX_REGISTRY_DEPTH {
        return Err(FirmwareError::new("USB registry nesting exceeds depth 32"));
    }
    nodes.push(node);
    for child in &node.children {
        collect_nodes(child, depth + 1, nodes)?;
    }
    Ok(())
}

fn all_nodes(snapshot: &RegistrySnapshot) -> Result<Vec<&RegistryNode>, FirmwareError> {
    let mut nodes = Vec::new();
    for root in &snapshot.roots {
        collect_nodes(root, 0, &mut nodes)?;
    }
    Ok(nodes)
}

fn application_location(snapshot: &RegistrySnapshot) -> Result<Option<u64>, FirmwareError> {
    let candidates: Vec<_> = all_nodes(snapshot)?
        .into_iter()
        .filter(|node| {
            node.vendor_id == Some(APPLICATION_VID) && node.product_id == Some(APPLICATION_PID)
        })
        .collect();
    if candidates.len() > 1 {
        return Err(FirmwareError::new("application USB identity is not unique"));
    }
    match candidates.first() {
        None => Ok(None),
        Some(node) => match node.location_id {
            Some(location) if location != 0 => Ok(Some(location)),
            _ => Err(FirmwareError::new(
                "application USB location is missing or zero",
            )),
        },
    }
}

pub(crate) fn require_application<B: MaintenanceBackend>(
    backend: &mut B,
) -> Result<u64, FirmwareError> {
    application_location(&registry(backend, REGISTRY_TIMEOUT)?)?
        .ok_or_else(|| FirmwareError::new("application USB device is absent"))
}

fn count_callouts(node: &RegistryNode, port: &str) -> usize {
    let mut count = usize::from(node.callout_device.as_deref() == Some(port));
    for child in &node.children {
        count += count_callouts(child, port);
    }
    count
}

fn bootloader_matches(
    snapshot: &RegistrySnapshot,
    location: u64,
    port: &str,
) -> Result<bool, FirmwareError> {
    let mut matches = 0_usize;
    for node in all_nodes(snapshot)? {
        if node.vendor_id == Some(APPLICATION_VID)
            && node.product_id == Some(BOOTLOADER_PID)
            && node.location_id == Some(location)
        {
            for child in &node.children {
                matches = matches.saturating_add(count_callouts(child, port));
            }
        }
    }
    match matches {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(FirmwareError::new(
            "bootloader USB port binding is not unique",
        )),
    }
}

pub(crate) fn require_bound_bootloader<B: MaintenanceBackend>(
    backend: &mut B,
    location: u64,
    port: &str,
) -> Result<(), FirmwareError> {
    if bootloader_matches(&registry(backend, REGISTRY_TIMEOUT)?, location, port)? {
        Ok(())
    } else {
        Err(FirmwareError::new(
            "explicit serial port is not bound to the qualified USB location",
        ))
    }
}

fn poll<B, F>(backend: &mut B, mut found: F) -> Result<(), FirmwareError>
where
    B: MaintenanceBackend,
    F: FnMut(&RegistrySnapshot) -> Result<bool, FirmwareError>,
{
    let start = backend.monotonic_now().map_err(backend_error)?;
    let deadline = start
        .checked_add(POLL_BUDGET)
        .ok_or_else(|| FirmwareError::new("USB polling deadline overflow"))?;
    let mut previous = start;
    // The limit also fails closed if a faulty injected clock never advances.
    for _ in 0..=120 {
        let now = backend.monotonic_now().map_err(backend_error)?;
        if now < previous {
            return Err(FirmwareError::new("monotonic clock moved backwards"));
        }
        if now >= deadline {
            break;
        }
        let snapshot = registry(backend, deadline - now)?;
        let after_query = backend.monotonic_now().map_err(backend_error)?;
        if after_query < now {
            return Err(FirmwareError::new("monotonic clock moved backwards"));
        }
        if after_query >= deadline {
            break;
        }
        if found(&snapshot)? {
            return Ok(());
        }
        let delay = POLL_INTERVAL.min(deadline - after_query);
        backend.check_cancelled().map_err(backend_error)?;
        backend.sleep(delay).map_err(backend_error)?;
        let after_sleep = backend.monotonic_now().map_err(backend_error)?;
        if after_sleep < after_query.saturating_add(delay) {
            return Err(FirmwareError::new(
                "polling clock did not advance for the requested interval",
            ));
        }
        previous = after_sleep;
    }
    Err(FirmwareError::new("USB identity polling timed out"))
}

pub(crate) fn wait_for_bootloader<B: MaintenanceBackend>(
    backend: &mut B,
    location: u64,
    port: &str,
) -> Result<(), FirmwareError> {
    poll(backend, |snapshot| {
        bootloader_matches(snapshot, location, port)
    })
}

pub(crate) fn wait_for_application<B: MaintenanceBackend>(
    backend: &mut B,
    location: u64,
) -> Result<(), FirmwareError> {
    poll(backend, |snapshot| match application_location(snapshot)? {
        None => Ok(false),
        Some(found) if found == location => Ok(true),
        Some(_) => Err(FirmwareError::new(
            "application USB location changed after reset",
        )),
    })
}

pub(crate) fn open_qualified<B: MaintenanceBackend>(
    backend: &mut B,
    open: &mut bool,
    pins: &[DescriptorPin<'_>],
    location: u64,
) -> Result<(), FirmwareError> {
    backend.check_cancelled().map_err(backend_error)?;
    let candidates = backend.enumerate_hid().map_err(backend_error)?;
    let matches: Vec<&HidCandidate> = candidates
        .iter()
        .filter(|candidate| {
            candidate.vendor_id == APPLICATION_VID
                && candidate.product_id == APPLICATION_PID
                && candidate.usage_page == 0xff00
                && candidate.usage == 1
                && candidate.transport == HidTransport::Usb
        })
        .collect();
    if matches.len() != 1 {
        return Err(FirmwareError::new(
            "qualified USB HID collection is not unique",
        ));
    }
    let candidate = matches[0];
    if candidate.location_id != Some(location) || location == 0 {
        return Err(FirmwareError::new(
            "USB HID candidate location does not match registry",
        ));
    }
    let opened = backend.open_hid(candidate).map_err(backend_error)?;
    *open = true;
    check_opened(candidate, &opened, pins)
}

fn check_opened(
    candidate: &HidCandidate,
    opened: &HidOpened,
    pins: &[DescriptorPin<'_>],
) -> Result<(), FirmwareError> {
    if opened.vendor_id != candidate.vendor_id
        || opened.product_id != candidate.product_id
        || opened.interface_number != candidate.interface_number
        || opened.transport != candidate.transport
        || opened.path != candidate.path
        || opened.location_id != candidate.location_id
        || opened.report_descriptor.len() > 1024
    {
        return Err(FirmwareError::new("opened USB HID identity changed"));
    }
    if !pins.iter().any(|pin| {
        pin.interface == opened.interface_number
            && pin.length == opened.report_descriptor.len()
            && pin.sha256 == sha256(&opened.report_descriptor)
    }) {
        return Err(FirmwareError::new("USB HID descriptor is not qualified"));
    }
    Ok(())
}

fn report(payload: &[u8]) -> Result<[u8; 64], FirmwareError> {
    if payload.len() > 61 {
        return Err(FirmwareError::new(
            "maintenance request needs multiple frames",
        ));
    }
    let mut report = [0_u8; 64];
    report[0] = 6;
    report[1] = 2;
    report[2] = payload.len() as u8;
    report[3..3 + payload.len()].copy_from_slice(payload);
    Ok(report)
}

pub(crate) fn bootloader_report() -> Result<[u8; 64], FirmwareError> {
    report(br#"{"method":"sys.bootloader","params":null,"id":102}"#)
}

struct Decoder {
    bytes: Vec<u8>,
    started: Option<Duration>,
}

impl Decoder {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            started: None,
        }
    }

    fn feed(&mut self, packet: &[u8], now: Duration) -> Result<Option<Value>, FirmwareError> {
        if packet.len() != 64 || packet[0] != 6 || packet[2] > 61 {
            return Err(FirmwareError::new("invalid maintenance HID frame"));
        }
        match packet[1] {
            1 => return Ok(None),
            2 => {}
            _ => return Err(FirmwareError::new("unknown maintenance HID channel")),
        }
        let chunk = &packet[3..3 + usize::from(packet[2])];
        if chunk.is_empty()
            || (self.bytes.is_empty() && chunk.iter().all(|byte| matches!(byte, b'\r' | b'\n')))
        {
            return Ok(None);
        }
        let started = *self.started.get_or_insert(now);
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > MAX_JSON || now.saturating_sub(started) > MAX_JSON_AGE {
            return Err(FirmwareError::new(
                "oversized or incomplete maintenance JSON",
            ));
        }
        let parsed = match serde_json::from_slice::<Value>(&self.bytes) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        self.bytes.clear();
        self.started = None;
        if !parsed.is_object() {
            return Err(FirmwareError::new("maintenance response is not an object"));
        }
        Ok(Some(parsed))
    }
}

pub(crate) fn query_version<B: MaintenanceBackend>(
    backend: &mut B,
) -> Result<String, FirmwareError> {
    backend.check_cancelled().map_err(backend_error)?;
    let request = report(br#"{"method":"sys.version","params":null,"id":101}"#)?;
    let count = backend.write_hid(&request).map_err(backend_error)?;
    if count != 64 {
        return Err(FirmwareError::new(format!(
            "maintenance version write was short: {count}"
        )));
    }
    let start = backend.monotonic_now().map_err(backend_error)?;
    let deadline = start
        .checked_add(VERSION_BUDGET)
        .ok_or_else(|| FirmwareError::new("version query deadline overflow"))?;
    let mut previous = start;
    let mut reads = 0_usize;
    let mut decoder = Decoder::new();
    loop {
        backend.check_cancelled().map_err(backend_error)?;
        let now = backend.monotonic_now().map_err(backend_error)?;
        if now < previous {
            return Err(FirmwareError::new("monotonic clock moved backwards"));
        }
        if now >= deadline || reads >= MAX_NONEMPTY_READS {
            return Err(FirmwareError::new("maintenance version query timed out"));
        }
        let packet = backend
            .read_hid(READ_SLICE.min(deadline - now))
            .map_err(backend_error)?;
        let after_read = backend.monotonic_now().map_err(backend_error)?;
        if after_read < now {
            return Err(FirmwareError::new("monotonic clock moved backwards"));
        }
        previous = after_read;
        let Some(packet) = packet.filter(|packet| !packet.is_empty()) else {
            if after_read == now {
                let delay = READ_SLICE.min(deadline - now);
                backend.sleep(delay).map_err(backend_error)?;
                let advanced = backend.monotonic_now().map_err(backend_error)?;
                if advanced < after_read.saturating_add(delay) {
                    return Err(FirmwareError::new("version query clock did not advance"));
                }
                previous = advanced;
            }
            continue;
        };
        reads += 1;
        if matches!(packet.first(), Some(1..=4)) {
            continue;
        }
        let Some(value) = decoder.feed(&packet, after_read)? else {
            continue;
        };
        let id = value.get("id").and_then(Value::as_number);
        if !id.is_some_and(|number| number.is_u64() && number.as_u64() == Some(101)) {
            continue;
        }
        if value.get("error").is_some() {
            return Err(FirmwareError::new(
                "device rejected maintenance version query",
            ));
        }
        if let Some(result) = value.get("result") {
            return result
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| FirmwareError::new("malformed maintenance version result"));
        }
    }
}
