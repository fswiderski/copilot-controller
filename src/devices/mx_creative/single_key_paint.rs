use std::time::{Duration, Instant};

use jpeg_encoder::{ColorType, Encoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::probe::{
    KEYPAD_DESCRIPTOR_HEX, KEYPAD_DESCRIPTOR_SHA256, KEYPAD_LOCATION_ID, KEYPAD_PID, KEYPAD_USAGE,
    VENDOR_PAGE, VID, report_descriptor_sha256,
};
use crate::native::hid::{
    HidBus, HidDescriptor, HidIdentity, HidMatcher, MxCreativeSingleKeyPaintBackend,
    MxCreativeSingleKeyPaintDevice, MxCreativeSingleKeyPaintReply,
};

const DISPLAY_USAGE: u16 = 0x1a10;
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
pub const BUDGET: Duration = Duration::from_secs(80);
pub const HOLD_MS: u16 = 3200;
pub const DWELL_MICROS: u32 = 62_500;
pub const KEEPALIVE_MS: u16 = 3000;
pub const KEEPALIVE_FEATURE_ID: u16 = 0x0008;
pub const KEEPALIVE_REFRESH_EVERY_COLORS: u8 = 16;
pub const KEY_COUNT: u8 = 9;
pub const COLORS_PER_KEY: u8 = 64;
pub const TOTAL_PAINT_FRAMES: u16 = 576;
pub const EXPECTED_KEEPALIVE_ACTIVATIONS: u16 = 36;
pub const EXPECTED_HIDPP_REPORTS: u16 = 54;
pub const OUTPUT_REPORT_ID: u8 = 0x14;
pub const OUTPUT_REPORT_LEN: usize = 4095;
pub const JPEG_HEADER_LEN: usize = 20;
pub const MAX_JPEG_LEN: usize = 4074;
const JPEG_QUALITY: u8 = 85;
pub const KEY_GRID_START_X: u16 = 23;
pub const KEY_GRID_START_Y: u16 = 6;
pub const KEY_GRID_STRIDE: u16 = 158;
pub const KEY1_X: u16 = KEY_GRID_START_X;
pub const KEY1_Y: u16 = KEY_GRID_START_Y;
pub const KEY1_W: u16 = 118;
pub const KEY1_H: u16 = 118;
const DISPLAY_WIDTH: u16 = 480;
const DISPLAY_HEIGHT: u16 = 480;
const FRAME_FUNCTION_SWID: u8 = 0x21;
const FRAME_SEQUENCE: u8 = 0xe0;
const DISPLAY_COLLECTION_MARKER_HEX: &str = "0643ff0a101aa101851496fe0f7508150026ff000910920201";
const ASK_USER_ACCEPT_PHRASE: &str = "READY_RAINBOW_NINE_KEYS";
const PALETTE_FIRST_RGB: [u8; 3] = [255, 0, 0];
const PALETTE_LAST_RGB: [u8; 3] = [255, 0, 24];

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
    pub activation_reports_sent: u16,
    pub refresh_every_colors: u8,
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
    pub keys_expected: u8,
    pub keys_completed: u8,
    pub colors_per_key: u8,
    pub total_frames_expected: u16,
    pub total_frames_sent: u16,
    pub dwell_micros: u32,
    pub rollback_wait_ms: u16,
    pub rollback_waits_completed: u8,
    pub key_rects: Vec<PaintRect>,
    pub palette_rgb_sha256: String,
    pub first_color_rgb: [u8; 3],
    pub last_color_rgb: [u8; 3],
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
    pub hidpp_reports_sent: u16,
    pub paint_reports_sent: u16,
    pub hid_output_reports_sent: u16,
    pub hid_output_bytes_sent: usize,
    pub retries: u8,
    pub hold_ms: u16,
    pub hold_wait_completed: bool,
    pub dwell_micros: u32,
    pub keys_expected: u8,
    pub keys_completed: u8,
    pub colors_per_key: u8,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

impl Default for ParentReadinessForm {
    fn default() -> Self {
        Self {
            required: true,
            tool: "ask_user".into(),
            title: "MX Creative 9-key rainbow paint readiness".into(),
            prompt: "Confirm physical readiness for sequential key-1..9 rainbow paint, one key at a time. Observe each key through 64 HSV colors, then confirm natural rollback after KeepAlive expiry before the next key. Parent must show a visible prompt, then run a 3-2-1 audible countdown before command execution.".into(),
            accept_phrase: ASK_USER_ACCEPT_PHRASE.into(),
            countdown_seconds: 3,
            command: "mx-creative-single-key-paint --confirm-rainbow-nine-keys".into(),
        }
    }
}

#[derive(Clone)]
struct PaletteFrame {
    rgb: [u8; 3],
    jpeg: Vec<u8>,
    jpeg_sha256: String,
}

pub(crate) fn run(backend: &MxCreativeSingleKeyPaintBackend) -> Result {
    run_with_sleep(backend, std::thread::sleep)
}

fn run_with_sleep(
    backend: &MxCreativeSingleKeyPaintBackend,
    mut sleep: impl FnMut(Duration),
) -> Result {
    let started = Instant::now();
    let mut result = base_result();
    let selected = match select(backend) {
        Ok(descriptor) => descriptor,
        Err(error) => return finish_failure(result, error, started),
    };

    let key_rects = match expected_key_rects_checked() {
        Ok(rects) => rects,
        Err(error) => return finish_failure(result, error, started),
    };
    let palette = match rainbow_palette_frames() {
        Ok(palette) => palette,
        Err(error) => return finish_failure(result, error, started),
    };

    result.paint = Some(PaintEvidence {
        report_id: OUTPUT_REPORT_ID,
        total_report_len: OUTPUT_REPORT_LEN,
        header_len: JPEG_HEADER_LEN,
        frame_function_swid: FRAME_FUNCTION_SWID,
        frame_sequence: FRAME_SEQUENCE,
        rect: key_rects[0],
        rect_provenance: PublicProvenance {
            source: "public secondary reverse-engineering grid x=23+col*158,y=6+row*158,size=118; key-1 physically confirmed".into(),
            confidence: "mixed".into(),
        },
        color_rgb: PALETTE_FIRST_RGB,
        jpeg_len: 0,
        jpeg_sha256: String::new(),
        image_ack_expected: false,
        keys_expected: KEY_COUNT,
        keys_completed: 0,
        colors_per_key: COLORS_PER_KEY,
        total_frames_expected: TOTAL_PAINT_FRAMES,
        total_frames_sent: 0,
        dwell_micros: DWELL_MICROS,
        rollback_wait_ms: HOLD_MS,
        rollback_waits_completed: 0,
        key_rects: key_rects.clone(),
        palette_rgb_sha256: expected_palette_rgb_sha256(),
        first_color_rgb: PALETTE_FIRST_RGB,
        last_color_rgb: PALETTE_LAST_RGB,
    });

    let mut expected_keepalive_index = None;
    let mut expected_keepalive_range = None;

    for (key_offset, rect) in key_rects.iter().enumerate() {
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

        let (keepalive_index, minimum_ms, maximum_ms) =
            match query_keepalive_context(&mut device, &mut result) {
                Ok(context) => context,
                Err(error) => return finish_failure(result, error, started),
            };
        if let Some(index) = expected_keepalive_index {
            if keepalive_index != index {
                return finish_failure(
                    result,
                    "keepalive feature index changed across key cycles",
                    started,
                );
            }
        } else {
            expected_keepalive_index = Some(keepalive_index);
        }
        if let Some((min, max)) = expected_keepalive_range {
            if minimum_ms != min || maximum_ms != max {
                return finish_failure(
                    result,
                    "keepalive timeout range changed across key cycles",
                    started,
                );
            }
        } else {
            expected_keepalive_range = Some((minimum_ms, maximum_ms));
        }

        if let Err(error) = activate_keepalive(
            &mut device,
            keepalive_index,
            minimum_ms,
            maximum_ms,
            &mut result,
        ) {
            return finish_failure(result, error, started);
        }

        for (color_index, frame) in palette.iter().enumerate() {
            if color_index != 0
                && color_index % usize::from(KEEPALIVE_REFRESH_EVERY_COLORS) == 0
                && let Err(error) = activate_keepalive(
                    &mut device,
                    keepalive_index,
                    minimum_ms,
                    maximum_ms,
                    &mut result,
                )
            {
                return finish_failure(result, error, started);
            }

            let paint_report = match frame_paint_report(*rect, &frame.jpeg) {
                Ok(report) => report,
                Err(error) => return finish_failure(result, error, started),
            };
            if let Err(error) = device.write_display_report(&paint_report) {
                return finish_failure(result, error.to_string(), started);
            }
            result.paint_reports_sent += 1;
            result.hid_output_reports_sent += 1;
            result.hid_output_bytes_sent += OUTPUT_REPORT_LEN;

            if let Some(paint) = result.paint.as_mut() {
                paint.rect = *rect;
                paint.color_rgb = frame.rgb;
                paint.jpeg_len = frame.jpeg.len();
                paint.jpeg_sha256 = frame.jpeg_sha256.clone();
                paint.total_frames_sent = result.paint_reports_sent;
                paint.keys_completed = key_offset as u8;
            }

            sleep(Duration::from_micros(u64::from(DWELL_MICROS)));
        }

        sleep(Duration::from_millis(u64::from(HOLD_MS)));
        result.keys_completed += 1;
        if let Some(paint) = result.paint.as_mut() {
            paint.keys_completed = result.keys_completed;
            paint.rollback_waits_completed = result.keys_completed;
        }
    }

    result.hold_wait_completed = result.keys_completed == KEY_COUNT;
    result.probe_succeeded = true;
    result.elapsed_ms = elapsed(started);
    result
}

fn query_keepalive_context(
    device: &mut MxCreativeSingleKeyPaintDevice,
    result: &mut Result,
) -> std::result::Result<(u8, u16, u16), String> {
    let keepalive_index = match device.query_keepalive_feature(QUERY_TIMEOUT) {
        Ok(MxCreativeSingleKeyPaintReply::FeatureIndex(index)) if index != 0 => index,
        Ok(MxCreativeSingleKeyPaintReply::FeatureIndex(_)) => {
            return Err("keepalive feature absent".into());
        }
        Ok(MxCreativeSingleKeyPaintReply::HidppError(code)) => {
            result.keepalive = Some(KeepAliveEvidence {
                feature_id: KEEPALIVE_FEATURE_ID,
                feature_index: 0,
                minimum_ms: 0,
                maximum_ms: 0,
                requested_ms: KEEPALIVE_MS,
                activation_accepted: false,
                activation_reports_sent: 0,
                refresh_every_colors: KEEPALIVE_REFRESH_EVERY_COLORS,
                hidpp_error: Some(code),
            });
            return Err("keepalive feature query rejected".into());
        }
        Ok(_) => return Err("unexpected keepalive feature response".into()),
        Err(error) => return Err(error.to_string()),
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
                    activation_reports_sent: 0,
                    refresh_every_colors: KEEPALIVE_REFRESH_EVERY_COLORS,
                    hidpp_error: Some(code),
                });
                return Err("keepalive range query rejected".into());
            }
            Ok(_) => return Err("unexpected keepalive range response".into()),
            Err(error) => return Err(error.to_string()),
        };
    result.hidpp_reports_sent += 1;
    result.hid_output_reports_sent += 1;
    result.hid_output_bytes_sent += 20;
    if minimum_ms < 500 || maximum_ms > 10_000 || !(minimum_ms..=maximum_ms).contains(&KEEPALIVE_MS)
    {
        return Err("invalid KeepAlive timeout range".into());
    }

    let activation_reports_sent = result
        .keepalive
        .as_ref()
        .map_or(0, |keepalive| keepalive.activation_reports_sent);
    result.keepalive = Some(KeepAliveEvidence {
        feature_id: KEEPALIVE_FEATURE_ID,
        feature_index: keepalive_index,
        minimum_ms,
        maximum_ms,
        requested_ms: KEEPALIVE_MS,
        activation_accepted: true,
        activation_reports_sent,
        refresh_every_colors: KEEPALIVE_REFRESH_EVERY_COLORS,
        hidpp_error: None,
    });

    Ok((keepalive_index, minimum_ms, maximum_ms))
}

fn activate_keepalive(
    device: &mut MxCreativeSingleKeyPaintDevice,
    keepalive_index: u8,
    minimum_ms: u16,
    maximum_ms: u16,
    result: &mut Result,
) -> std::result::Result<(), String> {
    let activation = match device.activate_keepalive(keepalive_index, KEEPALIVE_MS, QUERY_TIMEOUT) {
        Ok(MxCreativeSingleKeyPaintReply::Acknowledged) => (true, None),
        Ok(MxCreativeSingleKeyPaintReply::HidppError(code)) => (false, Some(code)),
        Ok(_) => return Err("unexpected keepalive activation response".into()),
        Err(error) => return Err(error.to_string()),
    };
    result.hidpp_reports_sent += 1;
    result.hid_output_reports_sent += 1;
    result.hid_output_bytes_sent += 20;

    let mut keepalive = result.keepalive.clone().unwrap_or(KeepAliveEvidence {
        feature_id: KEEPALIVE_FEATURE_ID,
        feature_index: keepalive_index,
        minimum_ms,
        maximum_ms,
        requested_ms: KEEPALIVE_MS,
        activation_accepted: true,
        activation_reports_sent: 0,
        refresh_every_colors: KEEPALIVE_REFRESH_EVERY_COLORS,
        hidpp_error: None,
    });
    keepalive.activation_reports_sent += 1;
    keepalive.activation_accepted = activation.0;
    keepalive.hidpp_error = activation.1;
    result.keepalive = Some(keepalive.clone());

    if keepalive.activation_accepted {
        Ok(())
    } else {
        Err("keepalive activation rejected".into())
    }
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

pub(crate) fn expected_key_rects() -> Vec<PaintRect> {
    let mut rects = Vec::with_capacity(usize::from(KEY_COUNT));
    for index in 0..usize::from(KEY_COUNT) {
        let row = u16::try_from(index / 3).expect("row index is bounded");
        let col = u16::try_from(index % 3).expect("column index is bounded");
        rects.push(PaintRect {
            x: KEY_GRID_START_X + col * KEY_GRID_STRIDE,
            y: KEY_GRID_START_Y + row * KEY_GRID_STRIDE,
            width: KEY1_W,
            height: KEY1_H,
        });
    }
    rects
}

fn expected_key_rects_checked() -> std::result::Result<Vec<PaintRect>, String> {
    let rects = expected_key_rects();
    let valid = rects.iter().all(|rect| {
        rect.width == KEY1_W
            && rect.height == KEY1_H
            && rect.x + rect.width <= DISPLAY_WIDTH
            && rect.y + rect.height <= DISPLAY_HEIGHT
    });
    if valid {
        Ok(rects)
    } else {
        Err("derived key rectangle outside 480x480 bounds".into())
    }
}

fn rainbow_palette_frames() -> std::result::Result<Vec<PaletteFrame>, String> {
    let mut frames = Vec::with_capacity(usize::from(COLORS_PER_KEY));
    let mut unique = std::collections::BTreeSet::new();
    for index in 0..usize::from(COLORS_PER_KEY) {
        let hue = (index as f32) * (360.0 / f32::from(COLORS_PER_KEY));
        let rgb = hsv_to_rgb(hue);
        unique.insert(rgb);
        let jpeg = encode_solid_jpeg(rgb)?;
        frames.push(PaletteFrame {
            rgb,
            jpeg_sha256: report_descriptor_sha256(&jpeg),
            jpeg,
        });
    }
    if unique.len() != usize::from(COLORS_PER_KEY) {
        return Err("rainbow palette generation produced duplicate RGB colors".into());
    }
    Ok(frames)
}

pub(crate) fn expected_palette_rgb_sha256() -> String {
    let mut bytes = Vec::with_capacity(usize::from(COLORS_PER_KEY) * 3);
    for index in 0..usize::from(COLORS_PER_KEY) {
        let hue = (index as f32) * (360.0 / f32::from(COLORS_PER_KEY));
        bytes.extend_from_slice(&hsv_to_rgb(hue));
    }
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

pub(crate) const fn expected_palette_last_rgb() -> [u8; 3] {
    PALETTE_LAST_RGB
}

fn hsv_to_rgb(hue: f32) -> [u8; 3] {
    let sector = (hue / 60.0).rem_euclid(6.0);
    let x = 1.0 - ((sector % 2.0) - 1.0).abs();
    let (r, g, b) = if sector < 1.0 {
        (1.0, x, 0.0)
    } else if sector < 2.0 {
        (x, 1.0, 0.0)
    } else if sector < 3.0 {
        (0.0, 1.0, x)
    } else if sector < 4.0 {
        (0.0, x, 1.0)
    } else if sector < 5.0 {
        (x, 0.0, 1.0)
    } else {
        (1.0, 0.0, x)
    };
    [
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    ]
}

fn encode_solid_jpeg(rgb: [u8; 3]) -> std::result::Result<Vec<u8>, String> {
    let mut frame = vec![0u8; usize::from(KEY1_W) * usize::from(KEY1_H) * 3];
    for pixel in frame.as_chunks_mut::<3>().0 {
        pixel.copy_from_slice(&rgb);
    }
    let mut jpeg = Vec::new();
    Encoder::new(&mut jpeg, JPEG_QUALITY)
        .encode(&frame, KEY1_W, KEY1_H, ColorType::Rgb)
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

fn frame_paint_report(rect: PaintRect, jpeg: &[u8]) -> std::result::Result<Vec<u8>, String> {
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
    report[9..11].copy_from_slice(&rect.x.to_be_bytes());
    report[11..13].copy_from_slice(&rect.y.to_be_bytes());
    report[13..15].copy_from_slice(&rect.width.to_be_bytes());
    report[15..17].copy_from_slice(&rect.height.to_be_bytes());
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
        rollback: "No refresh or cancel is sent. Natural KeepAlive expiry between keys (and after key 9) rolls back each volatile frame.".into(),
        visual_result_may_be_inconclusive: true,
        no_brightness_or_config_writes: true,
        hold_ms: HOLD_MS,
        dwell_micros: DWELL_MICROS,
        keys_expected: KEY_COUNT,
        colors_per_key: COLORS_PER_KEY,
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

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
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
    fn palette_has_64_unique_hues_and_expected_endpoints() {
        let palette = rainbow_palette_frames().unwrap();
        let unique: std::collections::BTreeSet<_> = palette.iter().map(|entry| entry.rgb).collect();
        assert_eq!(palette.len(), usize::from(COLORS_PER_KEY));
        assert_eq!(unique.len(), usize::from(COLORS_PER_KEY));
        assert_eq!(palette[0].rgb, PALETTE_FIRST_RGB);
        assert_eq!(palette[palette.len() - 1].rgb, PALETTE_LAST_RGB);
    }

    #[test]
    fn key_rect_grid_derivation_stays_within_expected_bounds() {
        let rects = expected_key_rects_checked().unwrap();
        assert_eq!(rects.len(), usize::from(KEY_COUNT));
        assert_eq!(
            rects[0],
            PaintRect {
                x: KEY1_X,
                y: KEY1_Y,
                width: KEY1_W,
                height: KEY1_H,
            }
        );
        assert_eq!(
            rects[8],
            PaintRect {
                x: 339,
                y: 322,
                width: KEY1_W,
                height: KEY1_H,
            }
        );
    }

    #[test]
    fn jpeg_and_header_are_exact_and_bounded() {
        let jpeg = encode_solid_jpeg(PALETTE_FIRST_RGB).unwrap();
        assert!(jpeg.starts_with(&[0xff, 0xd8]));
        assert!(jpeg.ends_with(&[0xff, 0xd9]));
        assert!(jpeg.len() < 4075);
        let framed = frame_paint_report(
            PaintRect {
                x: KEY1_X,
                y: KEY1_Y,
                width: KEY1_W,
                height: KEY1_H,
            },
            &jpeg,
        )
        .unwrap();
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
        let result = run_with_sleep(&backend, |_| {});
        assert!(!result.probe_succeeded);
        assert_eq!(
            result.error.as_deref(),
            Some("Protocol(\"MX display query report was not fully written\")")
        );
        assert_eq!(state.borrow().written.len(), 1);
        assert_eq!(result.retries, 0);
    }

    #[test]
    fn run_cycles_nine_keys_with_expected_bounded_counts() {
        let fake = FakeBackend::new(vec![descriptor()]);
        fake.state.borrow_mut().report_descriptor = Some(decode_hex(KEYPAD_DESCRIPTOR_HEX));
        let mut responses = Vec::new();
        for _ in 0..KEY_COUNT {
            responses.extend([
                hidpp_reply(0, 0, 1, &[6]),
                hidpp_reply(6, 0, 2, &[0x01, 0xf4, 0x27, 0x10]),
                hidpp_reply(6, 1, 3, &[]),
                hidpp_reply(6, 1, 4, &[]),
                hidpp_reply(6, 1, 5, &[]),
                hidpp_reply(6, 1, 6, &[]),
            ]);
        }
        fake.state.borrow_mut().inputs.extend(responses);
        let state = fake.state.clone();
        let backend = MxCreativeSingleKeyPaintBackend::from_test_backend(Box::new(fake));
        let result = run_with_sleep(&backend, |_| {});
        assert!(result.probe_succeeded, "{result:?}");
        assert_eq!(result.hidpp_reports_sent, EXPECTED_HIDPP_REPORTS);
        assert_eq!(result.paint_reports_sent, TOTAL_PAINT_FRAMES);
        assert_eq!(
            result.hid_output_reports_sent,
            EXPECTED_HIDPP_REPORTS + TOTAL_PAINT_FRAMES
        );
        assert_eq!(
            result.hid_output_bytes_sent,
            20 * usize::from(EXPECTED_HIDPP_REPORTS)
                + OUTPUT_REPORT_LEN * usize::from(TOTAL_PAINT_FRAMES)
        );
        assert_eq!(result.keys_completed, KEY_COUNT);
        assert!(result.hold_wait_completed);
        let paint = result.paint.as_ref().unwrap();
        assert_eq!(paint.total_frames_sent, TOTAL_PAINT_FRAMES);
        assert_eq!(paint.rollback_waits_completed, KEY_COUNT);
        assert_eq!(paint.key_rects.len(), usize::from(KEY_COUNT));
        assert_eq!(paint.first_color_rgb, PALETTE_FIRST_RGB);
        assert_eq!(paint.last_color_rgb, PALETTE_LAST_RGB);
        let frames = state.borrow().written.clone();
        assert_eq!(frames.len(), usize::from(result.hid_output_reports_sent));
        assert_eq!(frames[0].len(), 20);
        assert_eq!(frames[1].len(), 20);
        assert_eq!(frames[2].len(), 20);
        assert_eq!(frames[3].len(), OUTPUT_REPORT_LEN);
        assert_eq!(frames[3][0], OUTPUT_REPORT_ID);
        assert_eq!(u16::from_be_bytes([frames[3][9], frames[3][10]]), KEY1_X);
        assert_eq!(u16::from_be_bytes([frames[3][11], frames[3][12]]), KEY1_Y);
    }

    #[test]
    fn rejects_wrong_primary_usage_before_any_write() {
        let mut wrong = descriptor();
        wrong.usage = 0x1a09;
        let fake = FakeBackend::new(vec![wrong]);
        fake.state.borrow_mut().report_descriptor = Some(decode_hex(KEYPAD_DESCRIPTOR_HEX));
        let state = fake.state.clone();
        let backend = MxCreativeSingleKeyPaintBackend::from_test_backend(Box::new(fake));
        let result = run_with_sleep(&backend, |_| {});
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
        assert!(
            frame_paint_report(
                PaintRect {
                    x: KEY1_X,
                    y: KEY1_Y,
                    width: KEY1_W,
                    height: KEY1_H,
                },
                &oversized,
            )
            .is_err()
        );
    }
}
