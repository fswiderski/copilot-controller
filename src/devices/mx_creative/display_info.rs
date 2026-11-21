use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::probe;
use crate::native::hid::{
    HidBus, MxCreativeDisplayFeature as Feature, MxCreativeDisplayInfoBackend,
    MxCreativeDisplayQuery as Query, MxCreativeDisplayReply as Reply,
};

const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
pub const KEEPALIVE_CHECK_MS: u16 = 1000;
const FEATURES: [Feature; 4] = [
    Feature::KeepAlive,
    Feature::Brightness,
    Feature::Display,
    Feature::Controls,
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keepalive_check_ms: Option<u16>,
}

impl Options {
    pub fn validate(&self) -> std::result::Result<(), &'static str> {
        if self
            .keepalive_check_ms
            .is_some_and(|duration| duration != KEEPALIVE_CHECK_MS)
        {
            return Err("keepalive_check_ms must be exactly 1000");
        }
        Ok(())
    }

    pub fn budget(&self) -> Duration {
        if self.keepalive_check_ms.is_some() {
            Duration::from_secs(18)
        } else {
            Duration::from_secs(16)
        }
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
pub struct BrightnessGet {
    pub feature_id: u16,
    pub feature_index: u8,
    pub current: Option<u8>,
    pub info_sha256: Option<String>,
    pub info_hidpp_error: Option<u8>,
    pub current_hidpp_error: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeepAliveCheck {
    pub feature_id: u16,
    pub feature_index: u8,
    pub requested_ms: u16,
    pub activation_accepted: bool,
    pub idle_wait_completed: bool,
    pub hidpp_error: Option<u8>,
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
    pub brightness_get: Option<BrightnessGet>,
    pub keepalive_check: Option<KeepAliveCheck>,
    pub requests_sent: u8,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

pub(crate) fn run(backend: &MxCreativeDisplayInfoBackend, options: Options) -> Result {
    let semantic_read_only = options.keepalive_check_ms.is_none();
    if let Err(error) = options.validate() {
        return failure(error, semantic_read_only);
    }
    let started = Instant::now();
    let descriptor = match probe::select_display_keypad(backend) {
        Ok(descriptor) => descriptor,
        Err(error) => return failure(error, semantic_read_only),
    };
    if descriptor.bus != Some(HidBus::Usb) {
        return failure("keypad identity or transport mismatch", semantic_read_only);
    }
    let mut transport = match backend.open(&descriptor) {
        Ok(transport) => transport,
        Err(error) => return failure(error.to_string(), semantic_read_only),
    };
    let reopened_hash = match transport.report_descriptor() {
        Ok(bytes) => digest(&bytes),
        Err(error) => return failure(error.to_string(), semantic_read_only),
    };
    if reopened_hash != probe::KEYPAD_DESCRIPTOR_SHA256 {
        return failure("keypad reopened descriptor mismatch", semantic_read_only);
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
        semantic_read_only,
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
            Ok(
                Reply::FeatureIndex(_)
                | Reply::BrightnessInfo(_)
                | Reply::BrightnessCurrent(_)
                | Reply::Acknowledged,
            ) => {
                result.error = Some("unexpected KeepAlive response".into());
            }
            Err(error) => result.error = Some(error.to_string()),
        }
    }

    if options.keepalive_check_ms.is_some() {
        run_checked_getters(&mut transport, &mut result);
    }

    result.requests_sent = transport.requests_sent();
    result.elapsed_ms = elapsed(started);
    result.probe_succeeded = result.error.is_none();
    result
}

fn run_checked_getters(
    transport: &mut crate::native::hid::MxCreativeDisplayInfoDevice,
    result: &mut Result,
) {
    if result.error.is_some() {
        return;
    }
    let brightness_index = result
        .features
        .iter()
        .find(|feature| {
            feature.id == Feature::Brightness.id() && feature.status == FeatureStatus::Present
        })
        .and_then(|feature| feature.feature_index);
    if let Some(feature_index) = brightness_index {
        if let Some(feature) = result
            .features
            .iter_mut()
            .find(|feature| feature.id == Feature::Brightness.id())
        {
            feature.metadata_status = "brightness_get_check_authorized".into();
        }
        let mut brightness = BrightnessGet {
            feature_id: Feature::Brightness.id(),
            feature_index,
            current: None,
            info_sha256: None,
            info_hidpp_error: None,
            current_hidpp_error: None,
        };
        match transport.query(Query::BrightnessInfo { feature_index }, QUERY_TIMEOUT) {
            Ok(Reply::BrightnessInfo(payload)) => {
                brightness.info_sha256 = Some(digest(&payload));
            }
            Ok(Reply::HidppError(code)) => brightness.info_hidpp_error = Some(code),
            Ok(_) => result.error = Some("unexpected brightness info response".into()),
            Err(error) => result.error = Some(error.to_string()),
        }
        if result.error.is_none() {
            match transport.query(Query::BrightnessCurrent { feature_index }, QUERY_TIMEOUT) {
                Ok(Reply::BrightnessCurrent(current)) => brightness.current = Some(current),
                Ok(Reply::HidppError(code)) => brightness.current_hidpp_error = Some(code),
                Ok(_) => result.error = Some("unexpected brightness GET response".into()),
                Err(error) => result.error = Some(error.to_string()),
            }
        }
        result.brightness_get = Some(brightness);
    }

    if result.error.is_some() {
        return;
    }
    let keepalive = result
        .keepalive_range
        .as_ref()
        .filter(|range| (range.minimum_ms..=range.maximum_ms).contains(&KEEPALIVE_CHECK_MS));
    let Some(range) = keepalive else {
        return;
    };
    let mut check = KeepAliveCheck {
        feature_id: Feature::KeepAlive.id(),
        feature_index: range.feature_index,
        requested_ms: KEEPALIVE_CHECK_MS,
        activation_accepted: false,
        idle_wait_completed: false,
        hidpp_error: None,
    };
    match transport.query(
        Query::KeepAliveCheck {
            feature_index: range.feature_index,
        },
        QUERY_TIMEOUT,
    ) {
        Ok(Reply::Acknowledged) => {
            check.activation_accepted = true;
            std::thread::sleep(Duration::from_millis(u64::from(KEEPALIVE_CHECK_MS)));
            check.idle_wait_completed = true;
        }
        Ok(Reply::HidppError(code)) => check.hidpp_error = Some(code),
        Ok(_) => result.error = Some("unexpected KeepAlive check response".into()),
        Err(error) => result.error = Some(error.to_string()),
    }
    result.keepalive_check = Some(check);
}

fn feature_result(feature: Feature, reply: Reply) -> FeatureInfo {
    let metadata_status = match feature {
        Feature::DeviceFirmware => "firmware_info_not_authorized",
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
        Reply::KeepAliveRange { .. }
        | Reply::BrightnessInfo(_)
        | Reply::BrightnessCurrent(_)
        | Reply::Acknowledged => FeatureInfo {
            id: feature.id(),
            status: FeatureStatus::QueryError,
            feature_index: None,
            hidpp_error: None,
            metadata_status: "unexpected_reply".into(),
        },
    }
}

fn failure(error: impl Into<String>, semantic_read_only: bool) -> Result {
    Result {
        schema: 1,
        semantic_read_only,
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

    fn function_reply(
        software_id: u8,
        feature_index: u8,
        function: u8,
        parameters: &[u8],
    ) -> Vec<u8> {
        let mut reply = vec![0x11, 0xff, feature_index, (function << 4) | software_id];
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

    #[test]
    fn explicit_check_sends_only_brightness_getters_and_one_keepalive_timeout() {
        let backend = FakeBackend::new(vec![descriptor()]);
        backend.state.borrow_mut().report_descriptor = Some(descriptor_bytes());
        backend.state.borrow_mut().inputs.extend([
            reply(1, 0, &[5]),
            reply(2, 0, &[8]),
            reply(3, 0, &[7]),
            reply(4, 0, &[0]),
            reply(5, 5, &[0x01, 0xf4, 0x27, 0x10]),
            function_reply(6, 8, 0, &[0x64, 0, 1, 2]),
            function_reply(7, 8, 1, &[0x2a]),
            function_reply(8, 5, 1, &[]),
        ]);
        let state = backend.state.clone();
        let query_backend = MxCreativeDisplayInfoBackend::from_test_backend(Box::new(backend));
        let result = run(
            &query_backend,
            Options {
                keepalive_check_ms: Some(KEEPALIVE_CHECK_MS),
            },
        );
        assert!(result.probe_succeeded, "{result:?}");
        assert!(!result.semantic_read_only);
        assert_eq!(result.requests_sent, 8);
        assert_eq!(result.brightness_get.as_ref().unwrap().current, Some(42));
        assert!(
            result
                .brightness_get
                .as_ref()
                .unwrap()
                .info_sha256
                .as_deref()
                .is_some_and(|digest| digest.len() == 64)
        );
        assert_eq!(
            result.keepalive_check,
            Some(KeepAliveCheck {
                feature_id: 0x0008,
                feature_index: 5,
                requested_ms: 1000,
                activation_accepted: true,
                idle_wait_completed: true,
                hidpp_error: None,
            })
        );
        let frames = state.borrow().written.clone();
        assert_eq!(frames.len(), 8);
        assert_eq!(&frames[5][2..6], &[8, 0x06, 0, 0]);
        assert_eq!(&frames[6][2..6], &[8, 0x17, 0, 0]);
        assert_eq!(&frames[7][2..6], &[5, 0x18, 0x03, 0xe8]);
        assert!(frames.iter().all(|frame| {
            frame[0] == 0x11
                && frame[1] == 0xff
                && frame.len() == 20
                && !(frame[2] != 0 && frame[3] >> 4 == 2 && matches!(frame[2], 5 | 8))
        }));
    }
}
