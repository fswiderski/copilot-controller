use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::probe;
use crate::native::hid::{
    HidBus, MxCreativeDisplayFeature as Feature, MxCreativeDisplayInfoBackend,
    MxCreativeDisplayQuery as Query, MxCreativeDisplayReply as Reply,
};

const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
const FEATURES: [Feature; 4] = [
    Feature::KeepAlive,
    Feature::Brightness,
    Feature::Display,
    Feature::Controls,
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {}

impl Options {
    pub fn validate(&self) -> std::result::Result<(), &'static str> {
        Ok(())
    }

    pub fn budget(&self) -> Duration {
        Duration::from_secs(16)
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
    pub metadata_status: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeepAliveRange {
    pub feature_id: u16,
    pub feature_index: u8,
    pub minimum_ms: u16,
    pub maximum_ms: u16,
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
    pub features: Vec<FeatureInfo>,
    pub keepalive_range: Option<KeepAliveRange>,
    pub requests_sent: u8,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

pub(crate) fn run(backend: &MxCreativeDisplayInfoBackend, options: Options) -> Result {
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
        Ok(bytes) => digest(&bytes),
        Err(error) => return failure(error.to_string()),
    };
    if reopened_hash != probe::KEYPAD_DESCRIPTOR_SHA256 {
        return failure("keypad reopened descriptor mismatch");
    }
    let device = DeviceInfo {
        vendor_id: descriptor.identity.vendor_id,
        product_id: descriptor.identity.product_id,
        transport: "usb".into(),
        primary_usage_page: descriptor.usage_page,
        primary_usage: descriptor.usage,
        max_input_report_len: descriptor.max_input_report_len,
        max_output_report_len: descriptor.max_output_report_len,
        report_descriptor_sha256: reopened_hash,
    };
    let mut result = Result {
        schema: 1,
        semantic_read_only: true,
        device: Some(device),
        ..Result::default()
    };

    for feature in FEATURES {
        match transport.query(Query::Discover(feature), QUERY_TIMEOUT) {
            Ok(reply) => result.features.push(feature_result(feature, reply)),
            Err(error) => {
                result.requests_sent = transport.requests_sent();
                result.error = Some(error.to_string());
                result.elapsed_ms = elapsed(started);
                return result;
            }
        }
    }

    if let Some(keepalive) = result.features.first().filter(|feature| {
        feature.id == Feature::KeepAlive.id() && feature.status == FeatureStatus::Present
    }) {
        let feature_index = keepalive
            .feature_index
            .expect("present KeepAlive features have an index");
        match transport.query(Query::KeepAliveRange { feature_index }, QUERY_TIMEOUT) {
            Ok(Reply::KeepAliveRange {
                minimum_ms,
                maximum_ms,
            }) => {
                if minimum_ms == 0 || maximum_ms < minimum_ms {
                    result.error = Some("invalid KeepAlive timeout range".into());
                } else {
                    result.keepalive_range = Some(KeepAliveRange {
                        feature_id: Feature::KeepAlive.id(),
                        feature_index,
                        minimum_ms,
                        maximum_ms,
                    });
                }
            }
            Ok(Reply::HidppError(code)) => {
                result.features[0].metadata_status = "hidpp_error".into();
                result.features[0].hidpp_error = Some(code);
            }
            Ok(Reply::FeatureIndex(_)) => {
                result.error = Some("unexpected KeepAlive response".into());
            }
            Err(error) => result.error = Some(error.to_string()),
        }
    }

    result.requests_sent = transport.requests_sent();
    result.elapsed_ms = elapsed(started);
    result.probe_succeeded = result.error.is_none();
    result
}

fn feature_result(feature: Feature, reply: Reply) -> FeatureInfo {
    let metadata_status = match feature {
        Feature::KeepAlive => "range_query_authorized",
        Feature::Brightness => "brightness_get_not_authorized",
        Feature::Display => "geometry_get_not_authorized",
        Feature::Controls => "metadata_get_not_authorized",
    };
    match reply {
        Reply::FeatureIndex(0) => FeatureInfo {
            id: feature.id(),
            status: FeatureStatus::Absent,
            feature_index: None,
            hidpp_error: None,
            metadata_status: "not_applicable".into(),
        },
        Reply::FeatureIndex(feature_index) => FeatureInfo {
            id: feature.id(),
            status: FeatureStatus::Present,
            feature_index: Some(feature_index),
            hidpp_error: None,
            metadata_status: metadata_status.into(),
        },
        Reply::HidppError(code @ (0x01 | 0x06 | 0x09)) => FeatureInfo {
            id: feature.id(),
            status: if code == 0x09 {
                FeatureStatus::Unsupported
            } else {
                FeatureStatus::Absent
            },
            feature_index: None,
            hidpp_error: Some(code),
            metadata_status: "not_applicable".into(),
        },
        Reply::HidppError(code) => FeatureInfo {
            id: feature.id(),
            status: FeatureStatus::QueryError,
            feature_index: None,
            hidpp_error: Some(code),
            metadata_status: "query_error".into(),
        },
        Reply::KeepAliveRange { .. } => FeatureInfo {
            id: feature.id(),
            status: FeatureStatus::QueryError,
            feature_index: None,
            hidpp_error: None,
            metadata_status: "unexpected_reply".into(),
        },
    }
}

fn failure(error: impl Into<String>) -> Result {
    Result {
        schema: 1,
        semantic_read_only: true,
        error: Some(error.into()),
        ..Result::default()
    }
}

fn digest(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    output
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

    fn reply(software_id: u8, feature_index: u8, parameters: &[u8]) -> Vec<u8> {
        let mut reply = vec![0x11, 0xff, feature_index, software_id];
        reply.extend_from_slice(parameters);
        reply.resize(20, 0);
        reply
    }

    #[test]
    fn discovers_exact_features_and_only_queries_keepalive_range() {
        let backend = FakeBackend::new(vec![descriptor()]);
        backend.state.borrow_mut().report_descriptor = Some(descriptor_bytes());
        backend.state.borrow_mut().inputs.extend([
            reply(1, 0, &[5]),
            reply(2, 0, &[0]),
            reply(3, 0, &[7]),
            reply(4, 0, &[0]),
            reply(5, 5, &[0x01, 0xf4, 0x27, 0x10]),
        ]);
        let state = backend.state.clone();
        let query_backend = MxCreativeDisplayInfoBackend::from_test_backend(Box::new(backend));
        let result = run(&query_backend, Options::default());
        assert!(result.probe_succeeded, "{result:?}");
        assert_eq!(result.requests_sent, 5);
        assert_eq!(
            result
                .features
                .iter()
                .map(|feature| (feature.id, feature.feature_index))
                .collect::<Vec<_>>(),
            [
                (0x0008, Some(5)),
                (0x8040, None),
                (0x19a1, Some(7)),
                (0x1b04, None)
            ]
        );
        assert_eq!(
            result.keepalive_range,
            Some(KeepAliveRange {
                feature_id: 0x0008,
                feature_index: 5,
                minimum_ms: 500,
                maximum_ms: 10_000,
            })
        );
        let frames = state.borrow().written.clone();
        assert_eq!(
            frames
                .iter()
                .map(|frame| [frame[4], frame[5]])
                .collect::<Vec<_>>(),
            [[0, 8], [0x80, 0x40], [0x19, 0xa1], [0x1b, 0x04], [0, 0]]
        );
        assert!(frames.iter().all(|frame| {
            frame.len() == 20 && frame[0] == 0x11 && frame[1] == 0xff && frame[3] & 0xf0 == 0
        }));
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("private-serial"));
        assert!(!json.contains("iokit:42"));
        assert!(!json.contains("hex"));
    }

    #[test]
    fn absent_keepalive_does_not_send_function_metadata_queries() {
        let backend = FakeBackend::new(vec![descriptor()]);
        backend.state.borrow_mut().report_descriptor = Some(descriptor_bytes());
        backend.state.borrow_mut().inputs.extend([
            reply(1, 0, &[0]),
            reply(2, 0, &[0]),
            reply(3, 0, &[0]),
            reply(4, 0, &[0]),
        ]);
        let state = backend.state.clone();
        let query_backend = MxCreativeDisplayInfoBackend::from_test_backend(Box::new(backend));
        let result = run(&query_backend, Options::default());
        assert!(result.probe_succeeded);
        assert_eq!(result.requests_sent, 4);
        assert!(result.keepalive_range.is_none());
        assert_eq!(state.borrow().written.len(), 4);
    }
}
