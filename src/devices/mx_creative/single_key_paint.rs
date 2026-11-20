use std::time::{Duration, Instant};

use jpeg_encoder::{ColorType, Encoder};
use serde::{Deserialize, Serialize};

use super::probe::{
    KEYPAD_DESCRIPTOR_HEX, KEYPAD_DESCRIPTOR_SHA256, KEYPAD_LOCATION_ID, KEYPAD_PID, KEYPAD_USAGE,
    VENDOR_PAGE, VID, report_descriptor_sha256,
};
use crate::native::hid::{
    HidBus, HidDescriptor, HidIdentity, HidMatcher, MxCreativeSingleKeyPaintBackend,
    MxCreativeSingleKeyPaintReply,
};

const DISPLAY_USAGE: u16 = 0x1a10;
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
pub const BUDGET: Duration = Duration::from_secs(14);
pub const HOLD_MS: u16 = 2000;
pub const KEEPALIVE_MS: u16 = 3000;
pub const KEEPALIVE_FEATURE_ID: u16 = 0x0008;
pub const OUTPUT_REPORT_ID: u8 = 0x14;
pub const OUTPUT_REPORT_LEN: usize = 4095;
pub const JPEG_HEADER_LEN: usize = 20;
pub const MAX_JPEG_LEN: usize = 4074;
const JPEG_QUALITY: u8 = 90;
pub const KEY1_X: u16 = 23;
pub const KEY1_Y: u16 = 6;
pub const KEY1_W: u16 = 118;
pub const KEY1_H: u16 = 118;
pub const KEY1_CYAN_RGB: [u8; 3] = [0, 255, 255];
const FRAME_FUNCTION_SWID: u8 = 0x21;
const FRAME_SEQUENCE: u8 = 0xe0;
const DISPLAY_COLLECTION_MARKER_HEX: &str = "0643ff0a101aa101851496fe0f7508150026ff000910920201";
const ASK_USER_ACCEPT_PHRASE: &str = "READY_SINGLE_KEY_PAINT";

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeepAliveEvidence {
    pub feature_id: u16,
    pub feature_index: u8,
    pub minimum_ms: u16,
    pub maximum_ms: u16,
    pub requested_ms: u16,
    pub activation_accepted: bool,
    pub hidpp_error: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaintRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicProvenance {
    pub source: String,
    pub confidence: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaintEvidence {
    pub report_id: u8,
    pub total_report_len: usize,
    pub header_len: usize,
    pub frame_function_swid: u8,
    pub frame_sequence: u8,
    pub rect: PaintRect,
    pub rect_provenance: PublicProvenance,
    pub color_rgb: [u8; 3],
    pub jpeg_len: usize,
    pub jpeg_sha256: String,
    pub image_ack_expected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParentReadinessForm {
    pub required: bool,
    pub tool: String,
    pub title: String,
    pub prompt: String,
    pub accept_phrase: String,
    pub countdown_seconds: u8,
    pub command: String,
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
    pub keepalive: Option<KeepAliveEvidence>,
    pub paint: Option<PaintEvidence>,
    pub parent_readiness_form: ParentReadinessForm,
    pub operator_warning: String,
    pub rollback: String,
    pub visual_result_may_be_inconclusive: bool,
    pub no_brightness_or_config_writes: bool,
    pub hidpp_reports_sent: u8,
    pub paint_reports_sent: u8,
    pub hid_output_reports_sent: u8,
    pub hid_output_bytes_sent: usize,
    pub retries: u8,
    pub hold_ms: u16,
    pub hold_wait_completed: bool,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

impl Default for ParentReadinessForm {
    fn default() -> Self {
        Self {
            required: true,
            tool: "ask_user".into(),
            title: "MX Creative single-key paint readiness".into(),
            prompt: "Confirm physical readiness for one key-1 paint write, watch the display immediately, and acknowledge natural rollback after KeepAlive expiry. Parent must show a visible prompt, then run a 3-2-1 audible countdown before command execution.".into(),
            accept_phrase: ASK_USER_ACCEPT_PHRASE.into(),
            countdown_seconds: 3,
            command: "mx-creative-single-key-paint --confirm-write-single-key".into(),
        }
    }
}

pub(crate) fn run(backend: &MxCreativeSingleKeyPaintBackend) -> Result {
    let started = Instant::now();
    let mut result = base_result();
    let selected = match select(backend) {
        Ok(descriptor) => descriptor,
        Err(error) => return finish_failure(result, error, started),
    };
    let mut device = match backend.open(&selected) {
        Ok(device) => device,
        Err(error) => return finish_failure(result, error.to_string(), started),
    };
    let reopened_hash = match device.report_descriptor() {
        Ok(bytes) => report_descriptor_sha256(&bytes),
        Err(error) => return finish_failure(result, error.to_string(), started),
    };
    if reopened_hash != KEYPAD_DESCRIPTOR_SHA256 {
        return finish_failure(result, "keypad reopened descriptor mismatch", started);
    }
    result.device = Some(DeviceInfo {
        vendor_id: selected.identity.vendor_id,
        product_id: selected.identity.product_id,
        transport: "usb".into(),
        primary_usage_page: selected.usage_page,
        primary_usage: selected.usage,
        max_input_report_len: selected.max_input_report_len,
        max_output_report_len: selected.max_output_report_len,
        report_descriptor_sha256: reopened_hash,
    });

    let keepalive_index = match device.query_keepalive_feature(QUERY_TIMEOUT) {
        Ok(MxCreativeSingleKeyPaintReply::FeatureIndex(index)) if index != 0 => index,
        Ok(MxCreativeSingleKeyPaintReply::FeatureIndex(_)) => {
            return finish_failure(result, "keepalive feature absent", started);
        }
        Ok(MxCreativeSingleKeyPaintReply::HidppError(code)) => {
            result.keepalive = Some(KeepAliveEvidence {
                feature_id: KEEPALIVE_FEATURE_ID,
                feature_index: 0,
                minimum_ms: 0,
                maximum_ms: 0,
                requested_ms: KEEPALIVE_MS,
                activation_accepted: false,
                hidpp_error: Some(code),
            });
            return finish_failure(result, "keepalive feature query rejected", started);
        }
        Ok(_) => return finish_failure(result, "unexpected keepalive feature response", started),
        Err(error) => return finish_failure(result, error.to_string(), started),
    };
    result.hidpp_reports_sent += 1;
    result.hid_output_reports_sent += 1;
    result.hid_output_bytes_sent += 20;

    let (minimum_ms, maximum_ms) =
        match device.query_keepalive_range(keepalive_index, QUERY_TIMEOUT) {
            Ok(MxCreativeSingleKeyPaintReply::KeepAliveRange {
                minimum_ms,
                maximum_ms,
            }) if minimum_ms != 0 && maximum_ms >= minimum_ms => (minimum_ms, maximum_ms),
            Ok(MxCreativeSingleKeyPaintReply::HidppError(code)) => {
                result.keepalive = Some(KeepAliveEvidence {
                    feature_id: KEEPALIVE_FEATURE_ID,
                    feature_index: keepalive_index,
                    minimum_ms: 0,
                    maximum_ms: 0,
                    requested_ms: KEEPALIVE_MS,
                    activation_accepted: false,
                    hidpp_error: Some(code),
                });
                return finish_failure(result, "keepalive range query rejected", started);
            }
            Ok(_) => return finish_failure(result, "unexpected keepalive range response", started),
            Err(error) => return finish_failure(result, error.to_string(), started),
        };
    result.hidpp_reports_sent += 1;
    result.hid_output_reports_sent += 1;
    result.hid_output_bytes_sent += 20;
    if minimum_ms < 500 || maximum_ms > 10_000 || !(minimum_ms..=maximum_ms).contains(&KEEPALIVE_MS)
    {
        return finish_failure(result, "invalid KeepAlive timeout range", started);
    }

    let activation = match device.activate_keepalive(keepalive_index, KEEPALIVE_MS, QUERY_TIMEOUT) {
        Ok(MxCreativeSingleKeyPaintReply::Acknowledged) => KeepAliveEvidence {
            feature_id: KEEPALIVE_FEATURE_ID,
            feature_index: keepalive_index,
            minimum_ms,
            maximum_ms,
            requested_ms: KEEPALIVE_MS,
            activation_accepted: true,
            hidpp_error: None,
        },
        Ok(MxCreativeSingleKeyPaintReply::HidppError(code)) => KeepAliveEvidence {
            feature_id: KEEPALIVE_FEATURE_ID,
            feature_index: keepalive_index,
            minimum_ms,
            maximum_ms,
            requested_ms: KEEPALIVE_MS,
            activation_accepted: false,
            hidpp_error: Some(code),
        },
        Ok(_) => {
            return finish_failure(result, "unexpected keepalive activation response", started);
        }
        Err(error) => return finish_failure(result, error.to_string(), started),
    };
    result.hidpp_reports_sent += 1;
    result.hid_output_reports_sent += 1;
    result.hid_output_bytes_sent += 20;
    if !activation.activation_accepted {
        result.keepalive = Some(activation);
        return finish_failure(result, "keepalive activation rejected", started);
    }
    result.keepalive = Some(activation);

    let jpeg = match encode_cyan_jpeg() {
        Ok(jpeg) => jpeg,
        Err(error) => return finish_failure(result, error, started),
    };
    let paint_report = match frame_paint_report(&jpeg) {
        Ok(frame) => frame,
        Err(error) => return finish_failure(result, error, started),
    };
    let jpeg_sha256 = report_descriptor_sha256(&jpeg);
    if let Err(error) = device.write_display_report(&paint_report) {
        return finish_failure(result, error.to_string(), started);
    }
    result.paint_reports_sent = 1;
    result.hid_output_reports_sent += 1;
    result.hid_output_bytes_sent += OUTPUT_REPORT_LEN;
    result.paint = Some(PaintEvidence {
        report_id: OUTPUT_REPORT_ID,
        total_report_len: OUTPUT_REPORT_LEN,
        header_len: JPEG_HEADER_LEN,
        frame_function_swid: FRAME_FUNCTION_SWID,
        frame_sequence: FRAME_SEQUENCE,
        rect: PaintRect {
            x: KEY1_X,
            y: KEY1_Y,
            width: KEY1_W,
            height: KEY1_H,
        },
        rect_provenance: PublicProvenance {
            source: "public secondary reverse-engineering report with conservative key-1 grid"
                .into(),
            confidence: "medium".into(),
        },
        color_rgb: KEY1_CYAN_RGB,
        jpeg_len: jpeg.len(),
        jpeg_sha256,
        image_ack_expected: false,
    });
    std::thread::sleep(Duration::from_millis(u64::from(HOLD_MS)));
    result.hold_wait_completed = true;
    result.probe_succeeded = true;
    result.elapsed_ms = elapsed(started);
    result
}

fn select(backend: &MxCreativeSingleKeyPaintBackend) -> std::result::Result<HidDescriptor, String> {
    let matcher = HidMatcher::new(HidIdentity {
        vendor_id: VID,
        product_id: KEYPAD_PID,
    });
    let found = backend
        .enumerate(&matcher, 16)
        .map_err(|error| error.to_string())?;
    let display_primary = found
        .iter()
        .filter(|descriptor| {
            descriptor.usage_page == VENDOR_PAGE && descriptor.usage == DISPLAY_USAGE
        })
        .cloned()
        .collect::<Vec<_>>();
    let selected = if display_primary.is_empty() {
        found
            .iter()
            .filter(|descriptor| {
                descriptor.usage_page == VENDOR_PAGE && descriptor.usage == KEYPAD_USAGE
            })
            .cloned()
            .collect::<Vec<_>>()
    } else {
        display_primary
    };
    if selected.len() != 1 {
        return Err(format!(
            "keypad output route requires exactly one qualifying collection; found {}",
            selected.len()
        ));
    }
    let descriptor = selected[0].clone();
    verify_descriptor_identity(&descriptor)?;
    let evidence = backend
        .original_report_descriptor_evidence(&descriptor)
        .map_err(|error| error.to_string())?;
    let expected = decode_hex(KEYPAD_DESCRIPTOR_HEX);
    if evidence.source_path != descriptor.path.as_str() || evidence.bytes != expected {
        return Err("keypad output route descriptor mismatch".into());
    }
    let marker = decode_hex(DISPLAY_COLLECTION_MARKER_HEX);
    if !contains_subsequence(&evidence.bytes, &marker) {
        return Err("keypad display output route absent".into());
    }
    Ok(descriptor)
}

fn verify_descriptor_identity(descriptor: &HidDescriptor) -> std::result::Result<(), String> {
    let exact_primary = matches!(
        (descriptor.usage_page, descriptor.usage),
        (VENDOR_PAGE, KEYPAD_USAGE) | (VENDOR_PAGE, DISPLAY_USAGE)
    );
    if descriptor.identity.vendor_id != VID
        || descriptor.identity.product_id != KEYPAD_PID
        || !exact_primary
        || descriptor.max_input_report_len != 32
        || descriptor.max_output_report_len != OUTPUT_REPORT_LEN
        || descriptor.bus != Some(HidBus::Usb)
        || descriptor.product.as_deref() != Some("MX Creative Keypad")
        || descriptor.location_id != Some(KEYPAD_LOCATION_ID)
    {
        return Err("keypad output route identity or transport mismatch".into());
    }
    Ok(())
}

fn encode_cyan_jpeg() -> std::result::Result<Vec<u8>, String> {
    let mut rgb = vec![0u8; usize::from(KEY1_W) * usize::from(KEY1_H) * 3];
    for pixel in rgb.as_chunks_mut::<3>().0 {
        pixel.copy_from_slice(&KEY1_CYAN_RGB);
    }
    let mut jpeg = Vec::new();
    Encoder::new(&mut jpeg, JPEG_QUALITY)
        .encode(&rgb, KEY1_W, KEY1_H, ColorType::Rgb)
        .map_err(|error| format!("JPEG encoding failed: {error}"))?;
    if !jpeg.starts_with(&[0xff, 0xd8]) || !jpeg.ends_with(&[0xff, 0xd9]) {
        return Err("JPEG markers are invalid".into());
    }
    if jpeg.len() >= 4075 || jpeg.len() > MAX_JPEG_LEN {
        return Err(format!(
            "encoded JPEG length {} exceeds bounded maximum {}",
            jpeg.len(),
            MAX_JPEG_LEN
        ));
    }
    Ok(jpeg)
}

fn frame_paint_report(jpeg: &[u8]) -> std::result::Result<Vec<u8>, String> {
    if jpeg.is_empty() || jpeg.len() > MAX_JPEG_LEN {
        return Err("invalid JPEG payload length".into());
    }
    let mut report = vec![0u8; OUTPUT_REPORT_LEN];
    report[0] = OUTPUT_REPORT_ID;
    report[1] = 0xff;
    report[2] = 0x02;
    report[3] = FRAME_FUNCTION_SWID;
    report[4] = FRAME_SEQUENCE;
    report[5..9].copy_from_slice(&[0x01, 0x00, 0x01, 0x00]);
    report[9..11].copy_from_slice(&KEY1_X.to_be_bytes());
    report[11..13].copy_from_slice(&KEY1_Y.to_be_bytes());
    report[13..15].copy_from_slice(&KEY1_W.to_be_bytes());
    report[15..17].copy_from_slice(&KEY1_H.to_be_bytes());
    report[17] = 0x00;
    report[18..20].copy_from_slice(&(jpeg.len() as u16).to_be_bytes());
    report[20..20 + jpeg.len()].copy_from_slice(jpeg);
    Ok(report)
}

fn contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.windows(needle.len()).any(|slice| slice == needle)
}

fn base_result() -> Result {
    Result {
        schema: 1,
        semantic_read_only: false,
        parent_readiness_form: ParentReadinessForm::default(),
        operator_warning: "Visible confirmation is required before physical execution. Native SetReport success is not visual proof.".into(),
        rollback: "No refresh or cancel is sent. Natural KeepAlive expiry rolls back this one-key experiment.".into(),
        visual_result_may_be_inconclusive: true,
        no_brightness_or_config_writes: true,
        hold_ms: HOLD_MS,
        ..Result::default()
    }
}

fn finish_failure(mut result: Result, error: impl Into<String>, started: Instant) -> Result {
    result.elapsed_ms = elapsed(started);
    result.error = Some(error.into());
    result
}

fn elapsed(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn decode_hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let nibble = |byte: u8| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => unreachable!("hex marker is static"),
            };
            (nibble(pair[0]) << 4) | nibble(pair[1])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::hid::{HidPath, MxCreativeSingleKeyPaintBackend, fake::FakeBackend};

    fn descriptor() -> HidDescriptor {
        HidDescriptor {
            identity: HidIdentity {
                vendor_id: VID,
                product_id: KEYPAD_PID,
            },
            usage_page: VENDOR_PAGE,
            usage: KEYPAD_USAGE,
            product: Some("MX Creative Keypad".into()),
            serial: Some("private-serial".into()),
            max_input_report_len: 32,
            max_output_report_len: OUTPUT_REPORT_LEN,
            interface_number: None,
            bus: Some(HidBus::Usb),
            location_id: Some(KEYPAD_LOCATION_ID),
            path: HidPath::new("iokit:42").unwrap(),
        }
    }

    fn hidpp_reply(feature_index: u8, function: u8, software_id: u8, params: &[u8]) -> Vec<u8> {
        let mut frame = vec![0x11, 0xff, feature_index, (function << 4) | software_id];
        frame.extend_from_slice(params);
        frame.resize(20, 0);
        frame
    }

    #[test]
    fn jpeg_and_header_are_exact_and_bounded() {
        let jpeg = encode_cyan_jpeg().unwrap();
        assert!(jpeg.starts_with(&[0xff, 0xd8]));
        assert!(jpeg.ends_with(&[0xff, 0xd9]));
        assert!(jpeg.len() < 4075);
        let framed = frame_paint_report(&jpeg).unwrap();
        assert_eq!(framed.len(), OUTPUT_REPORT_LEN);
        assert_eq!(&framed[..9], &[0x14, 0xff, 0x02, 0x21, 0xe0, 1, 0, 1, 0]);
        assert_eq!(u16::from_be_bytes([framed[9], framed[10]]), KEY1_X);
        assert_eq!(u16::from_be_bytes([framed[11], framed[12]]), KEY1_Y);
        assert_eq!(u16::from_be_bytes([framed[13], framed[14]]), KEY1_W);
        assert_eq!(u16::from_be_bytes([framed[15], framed[16]]), KEY1_H);
        assert_eq!(
            u16::from_be_bytes([framed[18], framed[19]]) as usize,
            jpeg.len()
        );
        assert_eq!(&framed[20..20 + jpeg.len()], &jpeg);
    }

    #[test]
    fn run_is_one_shot_and_never_retries_short_writes() {
        let fake = FakeBackend::new(vec![descriptor()]);
        fake.state.borrow_mut().report_descriptor = Some(decode_hex(KEYPAD_DESCRIPTOR_HEX));
        fake.state.borrow_mut().inputs.extend([
            hidpp_reply(0, 0, 1, &[6]),
            hidpp_reply(6, 0, 2, &[0x01, 0xf4, 0x27, 0x10]),
            hidpp_reply(6, 1, 3, &[]),
        ]);
        fake.state.borrow_mut().write_count = Some(19);
        let state = fake.state.clone();
        let backend = MxCreativeSingleKeyPaintBackend::from_test_backend(Box::new(fake));
        let result = run(&backend);
        assert!(!result.probe_succeeded);
        assert_eq!(
            result.error.as_deref(),
            Some("Protocol(\"MX display query report was not fully written\")")
        );
        assert_eq!(state.borrow().written.len(), 1);
        assert_eq!(result.retries, 0);
    }

    #[test]
    fn path_and_key_choice_are_fixed_to_key1_only() {
        let fake = FakeBackend::new(vec![descriptor()]);
        fake.state.borrow_mut().report_descriptor = Some(decode_hex(KEYPAD_DESCRIPTOR_HEX));
        fake.state.borrow_mut().inputs.extend([
            hidpp_reply(0, 0, 1, &[6]),
            hidpp_reply(6, 0, 2, &[0x01, 0xf4, 0x27, 0x10]),
            hidpp_reply(6, 1, 3, &[]),
        ]);
        let state = fake.state.clone();
        let backend = MxCreativeSingleKeyPaintBackend::from_test_backend(Box::new(fake));
        let result = run(&backend);
        assert!(result.probe_succeeded, "{result:?}");
        assert_eq!(result.hidpp_reports_sent, 3);
        assert_eq!(result.paint_reports_sent, 1);
        assert_eq!(result.hid_output_reports_sent, 4);
        assert_eq!(result.hid_output_bytes_sent, 20 * 3 + OUTPUT_REPORT_LEN);
        assert!(result.hold_wait_completed);
        let paint = result.paint.as_ref().unwrap();
        assert_eq!(
            paint.rect,
            PaintRect {
                x: KEY1_X,
                y: KEY1_Y,
                width: KEY1_W,
                height: KEY1_H,
            }
        );
        assert_eq!(paint.color_rgb, KEY1_CYAN_RGB);
        let frames = state.borrow().written.clone();
        assert_eq!(frames.len(), 4);
        assert_eq!(frames[0].len(), 20);
        assert_eq!(frames[1].len(), 20);
        assert_eq!(frames[2].len(), 20);
        assert_eq!(frames[3].len(), OUTPUT_REPORT_LEN);
        assert_eq!(frames[3][0], OUTPUT_REPORT_ID);
    }

    #[test]
    fn rejects_wrong_primary_usage_before_any_write() {
        let mut wrong = descriptor();
        wrong.usage = 0x1a09;
        let fake = FakeBackend::new(vec![wrong]);
        fake.state.borrow_mut().report_descriptor = Some(decode_hex(KEYPAD_DESCRIPTOR_HEX));
        let state = fake.state.clone();
        let backend = MxCreativeSingleKeyPaintBackend::from_test_backend(Box::new(fake));
        let result = run(&backend);
        assert!(!result.probe_succeeded);
        assert_eq!(state.borrow().written.len(), 0);
        assert_eq!(
            result.error.as_deref(),
            Some("keypad output route requires exactly one qualifying collection; found 0")
        );
    }

    #[test]
    fn frame_limits_reject_oversize_payload() {
        let oversized = vec![0xff; 4075];
        assert!(frame_paint_report(&oversized).is_err());
    }
}
