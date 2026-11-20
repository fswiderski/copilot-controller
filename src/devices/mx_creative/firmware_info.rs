use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::probe;
use crate::native::hid::{
    HidBus, MxCreativeDisplayFeature as Feature, MxCreativeFirmwareInfoBackend,
    MxCreativeFirmwareQuery as Query, MxCreativeFirmwareReply as Reply,
};

const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
pub const MAX_FIRMWARES: u8 = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {}

impl Options {
    pub fn budget(&self) -> Duration {
        Duration::from_secs(22)
    }

    pub const fn validate(&self) -> std::result::Result<(), &'static str> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureStatus {
    Present,
    Absent,
    Unsupported,
    QueryError,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureInfo {
    pub id: u16,
    pub status: FeatureStatus,
    pub feature_index: Option<u8>,
    pub hidpp_error: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareInfo {
    pub index: u8,
    pub entity_type: u8,
    pub entity_name: String,
    pub name: Option<String>,
    pub name_hex: String,
    pub version: String,
    pub build: String,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub transport: String,
    pub primary_usage_page: u16,
    pub primary_usage: u16,
    pub max_input_report_len: usize,
    pub max_output_report_len: usize,
    pub report_descriptor_sha256: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Result {
    pub schema: u8,
    pub semantic_read_only: bool,
    pub probe_succeeded: bool,
    pub command_ready: bool,
    pub hardware_qualified: bool,
    pub device: Option<DeviceInfo>,
    pub feature: Option<FeatureInfo>,
    pub firmware_count: Option<u8>,
    pub firmwares: Vec<FirmwareInfo>,
    pub requests_sent: u8,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

pub(crate) fn run(backend: &MxCreativeFirmwareInfoBackend, options: Options) -> Result {
    if let Err(error) = options.validate() {
        return failure(error);
    }
    let started = Instant::now();
    let descriptor = match probe::select_display_keypad(backend) {
        Ok(descriptor) => descriptor,
        Err(error) => return failure(error),
    };
    if descriptor.bus != Some(HidBus::Usb) {
        return failure("keypad identity or transport mismatch");
    }
    let mut transport = match backend.open(&descriptor) {
        Ok(transport) => transport,
        Err(error) => return failure(error.to_string()),
    };
    let reopened_hash = match transport.report_descriptor() {
        Ok(bytes) => probe::report_descriptor_sha256(&bytes),
        Err(error) => return failure(error.to_string()),
    };
    if reopened_hash != probe::KEYPAD_DESCRIPTOR_SHA256 {
        return failure("keypad reopened descriptor mismatch");
    }
    let mut result = Result {
        schema: 1,
        semantic_read_only: true,
        device: Some(DeviceInfo {
            vendor_id: descriptor.identity.vendor_id,
            product_id: descriptor.identity.product_id,
            transport: "usb".into(),
            primary_usage_page: descriptor.usage_page,
            primary_usage: descriptor.usage,
            max_input_report_len: descriptor.max_input_report_len,
            max_output_report_len: descriptor.max_output_report_len,
            report_descriptor_sha256: reopened_hash,
        }),
        ..Result::default()
    };

    let discovery = match transport.query(Query::Discover(Feature::DeviceFirmware), QUERY_TIMEOUT) {
        Ok(reply) => reply,
        Err(error) => {
            finish_error(&mut result, &transport, error.to_string(), started);
            return result;
        }
    };
    let feature_index = match discovery {
        Reply::FeatureIndex(0) => {
            result.feature = Some(FeatureInfo {
                id: 0x0003,
                status: FeatureStatus::Absent,
                feature_index: None,
                hidpp_error: None,
            });
            finish_success(&mut result, &transport, started);
            return result;
        }
        Reply::FeatureIndex(index) => index,
        Reply::HidppError(code @ (0x01 | 0x06 | 0x09)) => {
            result.feature = Some(FeatureInfo {
                id: 0x0003,
                status: if code == 0x09 {
                    FeatureStatus::Unsupported
                } else {
                    FeatureStatus::Absent
                },
                feature_index: None,
                hidpp_error: Some(code),
            });
            finish_success(&mut result, &transport, started);
            return result;
        }
        Reply::HidppError(code) => {
            result.feature = Some(FeatureInfo {
                id: 0x0003,
                status: FeatureStatus::QueryError,
                feature_index: None,
                hidpp_error: Some(code),
            });
            finish_error(
                &mut result,
                &transport,
                "firmware feature discovery failed",
                started,
            );
            return result;
        }
        Reply::Count(_) | Reply::Version(_) => {
            finish_error(
                &mut result,
                &transport,
                "unexpected firmware feature discovery response",
                started,
            );
            return result;
        }
    };
    result.feature = Some(FeatureInfo {
        id: 0x0003,
        status: FeatureStatus::Present,
        feature_index: Some(feature_index),
        hidpp_error: None,
    });

    let count = match transport.query(Query::Count { feature_index }, QUERY_TIMEOUT) {
        Ok(Reply::Count(count)) if count <= MAX_FIRMWARES => count,
        Ok(Reply::Count(_)) => {
            finish_error(
                &mut result,
                &transport,
                "firmware count exceeds bounded maximum",
                started,
            );
            return result;
        }
        Ok(Reply::HidppError(_)) => {
            finish_error(
                &mut result,
                &transport,
                "firmware count query rejected",
                started,
            );
            return result;
        }
        Ok(_) => {
            finish_error(
                &mut result,
                &transport,
                "unexpected firmware count response",
                started,
            );
            return result;
        }
        Err(error) => {
            finish_error(&mut result, &transport, error.to_string(), started);
            return result;
        }
    };
    result.firmware_count = Some(count);
    for firmware_index in 0..count {
        let reply = match transport.query(
            Query::Version {
                feature_index,
                firmware_index,
            },
            QUERY_TIMEOUT,
        ) {
            Ok(reply) => reply,
            Err(error) => {
                finish_error(&mut result, &transport, error.to_string(), started);
                return result;
            }
        };
        let payload = match reply {
            Reply::Version(payload) => payload,
            Reply::HidppError(_) => {
                finish_error(
                    &mut result,
                    &transport,
                    "firmware metadata query rejected",
                    started,
                );
                return result;
            }
            _ => {
                finish_error(
                    &mut result,
                    &transport,
                    "unexpected firmware metadata response",
                    started,
                );
                return result;
            }
        };
        match decode_firmware(firmware_index, payload) {
            Ok(firmware) => result.firmwares.push(firmware),
            Err(error) => {
                finish_error(&mut result, &transport, error, started);
                return result;
            }
        }
    }
    finish_success(&mut result, &transport, started);
    result
}

fn decode_firmware(index: u8, payload: [u8; 16]) -> std::result::Result<FirmwareInfo, String> {
    let prefix = &payload[1..4];
    let text_length = prefix
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |position| position + 1);
    let name = if text_length != 0
        && prefix[..text_length]
            .iter()
            .all(|byte| (b' '..=b'~').contains(byte))
        && prefix[text_length..].iter().all(|byte| *byte == 0)
    {
        Some(
            std::str::from_utf8(&prefix[..text_length])
                .map_err(|_| "invalid firmware name encoding")?
                .to_owned(),
        )
    } else {
        None
    };
    let major = decode_bcd_byte(payload[4]).ok_or("invalid firmware major version")?;
    let revision = decode_bcd_byte(payload[5]).ok_or("invalid firmware revision")?;
    let build = decode_bcd_word([payload[6], payload[7]]).ok_or("invalid firmware build")?;
    let entity_type = payload[0];
    Ok(FirmwareInfo {
        index,
        entity_type,
        entity_name: entity_name(entity_type).into(),
        name,
        name_hex: format!("{:02x}{:02x}{:02x}", prefix[0], prefix[1], prefix[2]),
        version: format!("{major:02}.{revision:02}"),
        build: format!("{build:04}"),
        active: payload[8] & 1 != 0,
    })
}

fn entity_name(entity_type: u8) -> &'static str {
    match entity_type {
        0 => "main",
        1 => "bootloader",
        2 => "hardware",
        _ => "other",
    }
}

const fn decode_bcd_byte(value: u8) -> Option<u8> {
    let high = value >> 4;
    let low = value & 0x0f;
    if high <= 9 && low <= 9 {
        Some(high * 10 + low)
    } else {
        None
    }
}

fn decode_bcd_word(value: [u8; 2]) -> Option<u16> {
    let high = decode_bcd_byte(value[0])?;
    let low = decode_bcd_byte(value[1])?;
    Some(u16::from(high) * 100 + u16::from(low))
}

fn finish_success(
    result: &mut Result,
    transport: &crate::native::hid::MxCreativeFirmwareInfoDevice,
    started: Instant,
) {
    result.requests_sent = transport.requests_sent();
    result.elapsed_ms = elapsed(started);
    result.probe_succeeded = true;
}

fn finish_error(
    result: &mut Result,
    transport: &crate::native::hid::MxCreativeFirmwareInfoDevice,
    error: impl Into<String>,
    started: Instant,
) {
    result.requests_sent = transport.requests_sent();
    result.elapsed_ms = elapsed(started);
    result.error = Some(error.into());
}

fn failure(error: impl Into<String>) -> Result {
    Result {
        schema: 1,
        semantic_read_only: true,
        error: Some(error.into()),
        ..Result::default()
    }
}

fn elapsed(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::hid::{HidDescriptor, HidIdentity, HidPath, fake::FakeBackend};

    fn descriptor() -> HidDescriptor {
        HidDescriptor {
            identity: HidIdentity {
                vendor_id: 0x046d,
                product_id: 0xc354,
            },
            usage_page: 0xff43,
            usage: 0x1a02,
            product: Some("MX Creative Keypad".into()),
            serial: Some("private-serial".into()),
            max_input_report_len: 32,
            max_output_report_len: 4095,
            interface_number: None,
            bus: Some(HidBus::Usb),
            location_id: Some(0x0211_0000),
            path: HidPath::new("iokit:42").unwrap(),
        }
    }

    fn descriptor_bytes() -> Vec<u8> {
        probe::KEYPAD_DESCRIPTOR_HEX
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let nibble = |byte: u8| match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'a'..=b'f' => byte - b'a' + 10,
                    _ => unreachable!(),
                };
                (nibble(pair[0]) << 4) | nibble(pair[1])
            })
            .collect()
    }

    fn reply(software_id: u8, feature_index: u8, function: u8, payload: &[u8]) -> Vec<u8> {
        let mut reply = vec![0x11, 0xff, feature_index, (function << 4) | software_id];
        reply.extend_from_slice(payload);
        reply.resize(20, 0);
        reply
    }

    #[test]
    fn reads_exact_bounded_get_sequence_and_decodes_documented_fields() {
        let backend = FakeBackend::new(vec![descriptor()]);
        backend.state.borrow_mut().report_descriptor = Some(descriptor_bytes());
        backend.state.borrow_mut().inputs.extend([
            reply(1, 0, 0, &[7]),
            reply(2, 7, 0, &[2, 0xaa, 0xbb, 0xcc, 0xdd]),
            reply(3, 7, 1, &[0, b'M', b'K', b'P', 0x01, 0x23, 0x00, 0x45, 1]),
            reply(4, 7, 1, &[1, b'B', b'O', b'T', 0x00, 0x07, 0x00, 0x12, 0]),
        ]);
        let state = backend.state.clone();
        let backend = MxCreativeFirmwareInfoBackend::from_test_backend(Box::new(backend));
        let result = run(&backend, Options::default());
        assert!(result.probe_succeeded, "{result:?}");
        assert_eq!(result.requests_sent, 4);
        assert_eq!(result.firmware_count, Some(2));
        assert_eq!(result.firmwares[0].name.as_deref(), Some("MKP"));
        assert_eq!(result.firmwares[0].version, "01.23");
        assert_eq!(result.firmwares[0].build, "0045");
        assert!(result.firmwares[0].active);
        assert_eq!(result.firmwares[1].entity_name, "bootloader");
        let frames = state.borrow().written.clone();
        assert_eq!(
            frames
                .iter()
                .map(|frame| [frame[2], frame[3] >> 4, frame[4], frame[5]])
                .collect::<Vec<_>>(),
            [[0, 0, 0, 3], [7, 0, 0, 0], [7, 1, 0, 0], [7, 1, 1, 0]]
        );
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("private-serial"));
        assert!(!json.contains("iokit:42"));
        assert!(!json.contains("aabbccdd"));
    }

    #[test]
    fn rejects_unbounded_count_without_querying_firmware_entries() {
        let backend = FakeBackend::new(vec![descriptor()]);
        backend.state.borrow_mut().report_descriptor = Some(descriptor_bytes());
        backend
            .state
            .borrow_mut()
            .inputs
            .extend([reply(1, 0, 0, &[7]), reply(2, 7, 0, &[MAX_FIRMWARES + 1])]);
        let state = backend.state.clone();
        let backend = MxCreativeFirmwareInfoBackend::from_test_backend(Box::new(backend));
        let result = run(&backend, Options::default());
        assert_eq!(
            result.error.as_deref(),
            Some("firmware count exceeds bounded maximum")
        );
        assert_eq!(result.requests_sent, 2);
        assert_eq!(state.borrow().written.len(), 2);
    }

    #[test]
    fn rejects_invalid_bcd_and_non_printable_names() {
        let mut payload = [0; 16];
        payload[1..4].copy_from_slice(b"APP");
        payload[4..8].copy_from_slice(&[0x01, 0x0a, 0, 1]);
        assert_eq!(
            decode_firmware(0, payload).unwrap_err(),
            "invalid firmware revision"
        );
        payload[5] = 0;
        payload[1] = 0xff;
        let opaque = decode_firmware(0, payload).unwrap();
        assert_eq!(opaque.name, None);
        assert_eq!(opaque.name_hex, "ff5050");
        payload[1..4].copy_from_slice(b"U1\0");
        assert_eq!(
            decode_firmware(0, payload).unwrap().name.as_deref(),
            Some("U1")
        );
    }
}
