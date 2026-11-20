use std::{
    io::Cursor,
    time::{Duration, Instant},
};

use jpeg_encoder::{ColorType as JpegColorType, Encoder};
use png::ColorType as PngColorType;
use serde::{Deserialize, Serialize};

use super::{
    probe::{
        KEYPAD_DESCRIPTOR_HEX, KEYPAD_DESCRIPTOR_SHA256, KEYPAD_LOCATION_ID, KEYPAD_PID,
        KEYPAD_USAGE, VENDOR_PAGE, VID, report_descriptor_sha256,
    },
    single_key_paint::{
        JPEG_HEADER_LEN, KEY1_H, KEY1_W, MAX_JPEG_LEN, OUTPUT_REPORT_ID, OUTPUT_REPORT_LEN,
        PaintRect, expected_key_rects,
    },
};
use crate::native::hid::{
    HidBus, HidDescriptor, HidIdentity, HidMatcher, MxCreativeSingleKeyPaintBackend,
    MxCreativeSingleKeyPaintDevice, MxCreativeSingleKeyPaintReply,
};

const DISPLAY_USAGE: u16 = 0x1a10;
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
pub const BUDGET: Duration = Duration::from_secs(20);
pub const HOLD_MS: u16 = 5000;
pub const ROLLBACK_WAIT_MS: u16 = 2200;
pub const KEEPALIVE_MS: u16 = 3000;
pub const KEEPALIVE_FEATURE_ID: u16 = 0x0008;
pub const KEEPALIVE_REFRESH_MS: u16 = 2000;
pub const KEYS_TARGETED: u8 = 7;
pub const TOTAL_PAINT_FRAMES: u16 = 7;
pub const EXPECTED_KEEPALIVE_ACTIVATIONS: u16 = 3;
pub const EXPECTED_HIDPP_REPORTS: u16 = 5;
const DISPLAY_WIDTH: u16 = 480;
const DISPLAY_HEIGHT: u16 = 480;
const FRAME_FUNCTION_SWID: u8 = 0x21;
const FRAME_SEQUENCE: u8 = 0xe0;
const DISPLAY_COLLECTION_MARKER_HEX: &str = "0643ff0a101aa101851496fe0f7508150026ff000910920201";
const ASK_USER_ACCEPT_PHRASE: &str = "READY_SEVEN_COLORED_ICONS";
const SOURCE_REPOSITORY: &str = "fes/fesReview-mx-icons";
const SOURCE_COMMIT: &str = "7a339c9";
const SOURCE_SVG_ROOT: &str = "assets/mx-keypad-icons";
const SOURCE_PNG_DIMENSION: usize = 480;
const ICON_JPEG_QUALITY: u8 = 75;

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
    pub refresh_interval_ms: u16,
    pub hidpp_error: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IconEvidence {
    pub key_index: u8,
    pub name: String,
    pub source_svg_path: String,
    pub source_png_sha256: String,
    pub jpeg_len: usize,
    pub jpeg_sha256: String,
    pub jpeg_baseline_jfif: bool,
    pub jpeg_contains_app1: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaintEvidence {
    pub report_id: u8,
    pub total_report_len: usize,
    pub header_len: usize,
    pub frame_function_swid: u8,
    pub frame_sequence: u8,
    pub key_rects: Vec<PaintRect>,
    pub targeted_keys: Vec<u8>,
    pub untouched_keys: Vec<u8>,
    pub source_repository: String,
    pub source_commit: String,
    pub source_svg_root: String,
    pub icons: Vec<IconEvidence>,
    pub total_frames_expected: u16,
    pub total_frames_sent: u16,
    pub hold_ms: u16,
    pub rollback_wait_ms: u16,
    pub hold_wait_completed: bool,
    pub rollback_wait_completed: bool,
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
    pub keys_targeted: u8,
    pub keys_completed: u8,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

impl Default for ParentReadinessForm {
    fn default() -> Self {
        Self {
            required: true,
            tool: "ask_user".into(),
            title: "MX Creative seven-key colored icon readiness".into(),
            prompt: "Confirm physical readiness for seven-key colored icon paint on keys 1..7. Keys 8..9 remain untouched. Observe static icons for about five seconds, then confirm natural KeepAlive rollback near seven seconds from start. Parent must show a visible prompt, then run a 3-2-1 audible countdown before command execution.".into(),
            accept_phrase: ASK_USER_ACCEPT_PHRASE.into(),
            countdown_seconds: 3,
            command: "mx-creative-colored-icon-test --confirm-seven-colored-icons".into(),
        }
    }
}

#[derive(Clone)]
struct IconFrame {
    key_index: u8,
    name: &'static str,
    source_svg_path: &'static str,
    source_png_sha256: String,
    jpeg: Vec<u8>,
    jpeg_sha256: String,
    jpeg_baseline_jfif: bool,
    jpeg_contains_app1: bool,
}

pub(crate) fn expected_icon_names() -> Vec<&'static str> {
    icon_sources().iter().map(|(name, _, _)| *name).collect()
}

pub(crate) fn expected_icon_source_svg_paths() -> Vec<&'static str> {
    icon_sources()
        .iter()
        .map(|(_, source, _)| *source)
        .collect()
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
    let rects = match expected_target_rects_checked() {
        Ok(rects) => rects,
        Err(error) => return finish_failure(result, error, started),
    };
    let icons = match icon_frames() {
        Ok(icons) => icons,
        Err(error) => return finish_failure(result, error, started),
    };

    result.paint = Some(PaintEvidence {
        report_id: OUTPUT_REPORT_ID,
        total_report_len: OUTPUT_REPORT_LEN,
        header_len: JPEG_HEADER_LEN,
        frame_function_swid: FRAME_FUNCTION_SWID,
        frame_sequence: FRAME_SEQUENCE,
        key_rects: rects.clone(),
        targeted_keys: (1..=KEYS_TARGETED).collect(),
        untouched_keys: vec![8, 9],
        source_repository: SOURCE_REPOSITORY.into(),
        source_commit: SOURCE_COMMIT.into(),
        source_svg_root: SOURCE_SVG_ROOT.into(),
        icons: icons
            .iter()
            .map(|icon| IconEvidence {
                key_index: icon.key_index,
                name: icon.name.into(),
                source_svg_path: icon.source_svg_path.into(),
                source_png_sha256: icon.source_png_sha256.clone(),
                jpeg_len: icon.jpeg.len(),
                jpeg_sha256: icon.jpeg_sha256.clone(),
                jpeg_baseline_jfif: icon.jpeg_baseline_jfif,
                jpeg_contains_app1: icon.jpeg_contains_app1,
            })
            .collect(),
        total_frames_expected: TOTAL_PAINT_FRAMES,
        total_frames_sent: 0,
        hold_ms: HOLD_MS,
        rollback_wait_ms: ROLLBACK_WAIT_MS,
        hold_wait_completed: false,
        rollback_wait_completed: false,
    });

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
    if let Err(error) = activate_keepalive(
        &mut device,
        keepalive_index,
        minimum_ms,
        maximum_ms,
        &mut result,
    ) {
        return finish_failure(result, error, started);
    }

    for (index, icon) in icons.iter().enumerate() {
        let paint_report = match frame_paint_report(rects[index], &icon.jpeg) {
            Ok(report) => report,
            Err(error) => return finish_failure(result, error, started),
        };
        if let Err(error) = device.write_display_report(&paint_report) {
            return finish_failure(result, error.to_string(), started);
        }
        result.paint_reports_sent += 1;
        result.hid_output_reports_sent += 1;
        result.hid_output_bytes_sent += OUTPUT_REPORT_LEN;
        result.keys_completed += 1;
        if let Some(paint) = result.paint.as_mut() {
            paint.total_frames_sent = result.paint_reports_sent;
        }
    }

    sleep(Duration::from_millis(u64::from(KEEPALIVE_REFRESH_MS)));
    if let Err(error) = activate_keepalive(
        &mut device,
        keepalive_index,
        minimum_ms,
        maximum_ms,
        &mut result,
    ) {
        return finish_failure(result, error, started);
    }
    sleep(Duration::from_millis(u64::from(KEEPALIVE_REFRESH_MS)));
    if let Err(error) = activate_keepalive(
        &mut device,
        keepalive_index,
        minimum_ms,
        maximum_ms,
        &mut result,
    ) {
        return finish_failure(result, error, started);
    }
    sleep(Duration::from_millis(u64::from(
        HOLD_MS - 2 * KEEPALIVE_REFRESH_MS,
    )));
    if let Some(paint) = result.paint.as_mut() {
        paint.hold_wait_completed = true;
    }

    sleep(Duration::from_millis(u64::from(ROLLBACK_WAIT_MS)));
    if let Some(paint) = result.paint.as_mut() {
        paint.rollback_wait_completed = true;
    }

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
                refresh_interval_ms: KEEPALIVE_REFRESH_MS,
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
                    refresh_interval_ms: KEEPALIVE_REFRESH_MS,
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
        refresh_interval_ms: KEEPALIVE_REFRESH_MS,
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
        refresh_interval_ms: KEEPALIVE_REFRESH_MS,
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

fn expected_target_rects_checked() -> std::result::Result<Vec<PaintRect>, String> {
    let mut rects = expected_key_rects();
    rects.truncate(usize::from(KEYS_TARGETED));
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

fn icon_sources() -> [(&'static str, &'static str, &'static [u8]); KEYS_TARGETED as usize] {
    [
        (
            "play-pause",
            "play-pause.svg",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/mx-keypad-icons-png/play-pause.png"
            )),
        ),
        (
            "first-frame",
            "first-frame.svg",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/mx-keypad-icons-png/first-frame.png"
            )),
        ),
        (
            "last-frame",
            "last-frame.svg",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/mx-keypad-icons-png/last-frame.png"
            )),
        ),
        (
            "back-1s",
            "back-1s.svg",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/mx-keypad-icons-png/back-1s.png"
            )),
        ),
        (
            "forward-1s",
            "forward-1s.svg",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/mx-keypad-icons-png/forward-1s.png"
            )),
        ),
        (
            "screenshot",
            "screenshot.svg",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/mx-keypad-icons-png/screenshot.png"
            )),
        ),
        (
            "dictate",
            "dictate.svg",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/mx-keypad-icons-png/dictate.png"
            )),
        ),
    ]
}

fn icon_frames() -> std::result::Result<Vec<IconFrame>, String> {
    icon_sources()
        .iter()
        .enumerate()
        .map(|(index, (name, source_svg_path, png_bytes))| {
            let (width, height, rgb) = decode_png_to_rgb(png_bytes)?;
            if width != SOURCE_PNG_DIMENSION || height != SOURCE_PNG_DIMENSION {
                return Err("source PNG dimensions must be exactly 480x480".into());
            }
            let resized = resize_nearest_rgb(
                &rgb,
                width,
                height,
                usize::from(KEY1_W),
                usize::from(KEY1_H),
            )?;
            let jpeg = encode_icon_jpeg(&resized)?;
            let inspected = inspect_jpeg(&jpeg)?;
            if !inspected.has_jfif_app0 {
                return Err("icon JPEG missing baseline JFIF APP0".into());
            }
            if inspected.has_app1 {
                return Err("icon JPEG still contains APP1 metadata".into());
            }
            if inspected.progressive {
                return Err("icon JPEG must remain baseline (non-progressive)".into());
            }
            Ok(IconFrame {
                key_index: u8::try_from(index + 1).expect("key index is bounded"),
                name,
                source_svg_path,
                source_png_sha256: report_descriptor_sha256(png_bytes),
                jpeg_sha256: report_descriptor_sha256(&jpeg),
                jpeg,
                jpeg_baseline_jfif: inspected.has_jfif_app0 && !inspected.progressive,
                jpeg_contains_app1: inspected.has_app1,
            })
        })
        .collect()
}

fn decode_png_to_rgb(bytes: &[u8]) -> std::result::Result<(usize, usize, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("PNG decode failed: {error}"))?;
    let mut output = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut output)
        .map_err(|error| format!("PNG frame decode failed: {error}"))?;
    let width = usize::try_from(info.width).map_err(|_| "PNG width overflow")?;
    let height = usize::try_from(info.height).map_err(|_| "PNG height overflow")?;
    if width == 0 || height == 0 {
        return Err("PNG dimensions must be non-zero".into());
    }

    let data = &output[..info.buffer_size()];
    let rgb = match info.color_type {
        PngColorType::Rgb => data.to_vec(),
        PngColorType::Rgba => {
            if data.len() != width * height * 4 {
                return Err("PNG RGBA length mismatch".into());
            }
            let (chunks, remainder) = data.as_chunks::<4>();
            if !remainder.is_empty() {
                return Err("PNG RGBA length mismatch".into());
            }
            let mut rgb = Vec::with_capacity(width * height * 3);
            for chunk in chunks {
                let alpha = u16::from(chunk[3]);
                rgb.push(((u16::from(chunk[0]) * alpha) / 255) as u8);
                rgb.push(((u16::from(chunk[1]) * alpha) / 255) as u8);
                rgb.push(((u16::from(chunk[2]) * alpha) / 255) as u8);
            }
            rgb
        }
        PngColorType::Grayscale => {
            if data.len() != width * height {
                return Err("PNG grayscale length mismatch".into());
            }
            let mut rgb = Vec::with_capacity(width * height * 3);
            for value in data {
                rgb.extend_from_slice(&[*value, *value, *value]);
            }
            rgb
        }
        PngColorType::GrayscaleAlpha => {
            if data.len() != width * height * 2 {
                return Err("PNG grayscale+alpha length mismatch".into());
            }
            let (chunks, remainder) = data.as_chunks::<2>();
            if !remainder.is_empty() {
                return Err("PNG grayscale+alpha length mismatch".into());
            }
            let mut rgb = Vec::with_capacity(width * height * 3);
            for chunk in chunks {
                let alpha = u16::from(chunk[1]);
                let value = ((u16::from(chunk[0]) * alpha) / 255) as u8;
                rgb.extend_from_slice(&[value, value, value]);
            }
            rgb
        }
        PngColorType::Indexed => return Err("PNG palette should be expanded before decode".into()),
    };

    if rgb.len() != width * height * 3 {
        return Err("decoded PNG RGB length mismatch".into());
    }

    Ok((width, height, rgb))
}

fn resize_nearest_rgb(
    source: &[u8],
    source_width: usize,
    source_height: usize,
    target_width: usize,
    target_height: usize,
) -> std::result::Result<Vec<u8>, String> {
    if source_width == 0 || source_height == 0 || target_width == 0 || target_height == 0 {
        return Err("resize dimensions must be non-zero".into());
    }
    if source.len() != source_width * source_height * 3 {
        return Err("resize source RGB length mismatch".into());
    }
    let mut resized = vec![0u8; target_width * target_height * 3];
    for y in 0..target_height {
        let sy = y * source_height / target_height;
        for x in 0..target_width {
            let sx = x * source_width / target_width;
            let src = (sy * source_width + sx) * 3;
            let dst = (y * target_width + x) * 3;
            resized[dst..dst + 3].copy_from_slice(&source[src..src + 3]);
        }
    }
    Ok(resized)
}

fn encode_icon_jpeg(rgb: &[u8]) -> std::result::Result<Vec<u8>, String> {
    if rgb.len() != usize::from(KEY1_W) * usize::from(KEY1_H) * 3 {
        return Err("icon RGB payload length is invalid".into());
    }
    let mut jpeg = Vec::new();
    Encoder::new(&mut jpeg, ICON_JPEG_QUALITY)
        .encode(rgb, KEY1_W, KEY1_H, JpegColorType::Rgb)
        .map_err(|error| format!("icon JPEG encoding failed: {error}"))?;
    if !jpeg.starts_with(&[0xff, 0xd8]) || !jpeg.ends_with(&[0xff, 0xd9]) {
        return Err("icon JPEG markers are invalid".into());
    }
    if jpeg.len() > MAX_JPEG_LEN {
        return Err("icon JPEG payload length is invalid".into());
    }
    Ok(jpeg)
}

#[derive(Clone, Copy)]
struct JpegInspection {
    has_jfif_app0: bool,
    has_app1: bool,
    progressive: bool,
}

fn inspect_jpeg(bytes: &[u8]) -> std::result::Result<JpegInspection, String> {
    if bytes.len() < 4 || bytes[0..2] != [0xff, 0xd8] {
        return Err("JPEG markers are invalid".into());
    }
    let mut cursor = 2usize;
    let mut has_jfif_app0 = false;
    let mut has_app1 = false;
    let mut progressive = false;

    while cursor + 1 < bytes.len() {
        if bytes[cursor] != 0xff {
            cursor += 1;
            continue;
        }
        while cursor < bytes.len() && bytes[cursor] == 0xff {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            break;
        }
        let marker = bytes[cursor];
        cursor += 1;

        match marker {
            0xd8 => continue,
            0xd9 | 0xda => break,
            0xd0..=0xd7 | 0x01 => continue,
            _ => {
                if cursor + 1 >= bytes.len() {
                    return Err("JPEG segment length is invalid".into());
                }
                let length = u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]) as usize;
                cursor += 2;
                if length < 2 || cursor + (length - 2) > bytes.len() {
                    return Err("JPEG segment length is invalid".into());
                }
                let payload = &bytes[cursor..cursor + (length - 2)];
                if marker == 0xe0 && payload.starts_with(b"JFIF\0") {
                    has_jfif_app0 = true;
                }
                if marker == 0xe1 {
                    has_app1 = true;
                }
                if marker == 0xc2 {
                    progressive = true;
                }
                cursor += length - 2;
            }
        }
    }

    Ok(JpegInspection {
        has_jfif_app0,
        has_app1,
        progressive,
    })
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
        rollback: "No refresh or cancel is sent. Keys 8..9 are untouched. KeepAlive expires naturally near seven seconds from start and rolls back volatile key-1..7 frames.".into(),
        visual_result_may_be_inconclusive: true,
        no_brightness_or_config_writes: true,
        keys_targeted: KEYS_TARGETED,
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
    fn icon_assets_are_baseline_jfif_and_under_payload_cap_without_app1() {
        let icons = icon_frames().unwrap();
        assert_eq!(icons.len(), usize::from(KEYS_TARGETED));
        assert_eq!(
            icons.iter().map(|icon| icon.name).collect::<Vec<_>>(),
            expected_icon_names()
        );
        assert_eq!(
            icons
                .iter()
                .map(|icon| icon.source_svg_path)
                .collect::<Vec<_>>(),
            expected_icon_source_svg_paths()
        );
        assert!(
            icons
                .iter()
                .all(|icon| icon.jpeg.len() <= MAX_JPEG_LEN && icon.jpeg_baseline_jfif)
        );
        assert!(icons.iter().all(|icon| !icon.jpeg_contains_app1));
    }

    #[test]
    fn run_paints_only_keys_one_through_seven() {
        let fake = FakeBackend::new(vec![descriptor()]);
        fake.state.borrow_mut().report_descriptor = Some(decode_hex(KEYPAD_DESCRIPTOR_HEX));
        fake.state.borrow_mut().inputs.extend([
            hidpp_reply(0, 0, 1, &[6]),
            hidpp_reply(6, 0, 2, &[0x01, 0xf4, 0x27, 0x10]),
            hidpp_reply(6, 1, 3, &[]),
            hidpp_reply(6, 1, 4, &[]),
            hidpp_reply(6, 1, 5, &[]),
        ]);
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
        assert_eq!(result.keys_completed, KEYS_TARGETED);
        let paint = result.paint.unwrap();
        assert_eq!(paint.targeted_keys, vec![1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(paint.untouched_keys, vec![8, 9]);
        assert_eq!(paint.source_commit, SOURCE_COMMIT);
        assert!(paint.hold_wait_completed);
        assert!(paint.rollback_wait_completed);
        let frames = state.borrow().written.clone();
        assert_eq!(frames.len(), usize::from(result.hid_output_reports_sent));
        let first_paint = &frames[3];
        let last_paint = &frames[9];
        assert_eq!(u16::from_be_bytes([first_paint[9], first_paint[10]]), 23);
        assert_eq!(u16::from_be_bytes([first_paint[11], first_paint[12]]), 6);
        assert_eq!(u16::from_be_bytes([last_paint[9], last_paint[10]]), 23);
        assert_eq!(u16::from_be_bytes([last_paint[11], last_paint[12]]), 322);
    }

    #[test]
    fn short_write_stops_without_retry() {
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
}
