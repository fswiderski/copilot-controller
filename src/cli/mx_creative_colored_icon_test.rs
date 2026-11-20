use std::{sync::atomic::AtomicBool, time::Duration};

use serde_json::{Value, json};

use crate::{
    devices::mx_creative::{
        colored_icon_test::{
            BUDGET, EXPECTED_HIDPP_REPORTS, EXPECTED_KEEPALIVE_ACTIVATIONS, HOLD_MS, IconName,
            KEEPALIVE_FEATURE_ID, KEEPALIVE_MS, KEEPALIVE_REFRESH_MS, KEYS_TARGETED, Options,
            ParentReadinessForm, ROLLBACK_WAIT_MS, Result, TOTAL_PAINT_FRAMES, expected_icon_specs,
        },
        single_key_paint::{MAX_JPEG_LEN, OUTPUT_REPORT_ID, OUTPUT_REPORT_LEN, expected_key_rects},
    },
    native::windows_worker as worker,
};

use super::Output;

pub(super) const KIND: &str = "mx_creative_colored_icon_test";

pub(super) fn parse(args: &[String]) -> std::result::Result<Options, String> {
    let [flag, icon_name] = args else {
        return Err(
            "colored icon test requires exactly --confirm-colored-icon <play-pause|first-frame|last-frame|back-1s|forward-1s|screenshot|dictate>"
                .into(),
        );
    };
    if flag != "--confirm-colored-icon" {
        return Err(
            "colored icon test requires exactly --confirm-colored-icon <play-pause|first-frame|last-frame|back-1s|forward-1s|screenshot|dictate>"
                .into(),
        );
    }
    let icon = icon_name.parse::<IconName>()?;
    Ok(Options { icon })
}

pub(super) fn execute(options: Options) -> std::result::Result<Output, String> {
    let allowlist =
        super::mx_creative_single_key_paint_allowlist().map_err(|_| worker_error("allowlist"))?;
    execute_with(options, |request, budget| {
        worker::probe(request, budget, &AtomicBool::new(false), &allowlist)
    })
}

fn execute_with(
    options: Options,
    launch: impl FnOnce(
        &worker::DiagnosticRequest,
        Duration,
    ) -> std::result::Result<worker::DiagnosticResult, worker::WorkerError>,
) -> std::result::Result<Output, String> {
    let request = worker::DiagnosticRequest {
        kind: KIND.into(),
        payload: serde_json::to_value(options).map_err(worker_error)?,
    };
    let response = launch(&request, BUDGET)
        .map_err(|error| json_string(&json!({"status":"worker_error","error":error})))?;
    let result: Result = serde_json::from_value(response.payload)
        .map_err(|_| worker_error(worker::WorkerErrorKind::Protocol))?;
    if !valid_result(&result) {
        return Err(worker_error(worker::WorkerErrorKind::Protocol));
    }
    let payload = json_string(&result);
    if result.probe_succeeded {
        Ok(Output::Json(
            serde_json::from_str(&payload).map_err(|_| worker_error(()))?,
        ))
    } else {
        Err(payload)
    }
}

pub(super) fn callback(
    payload: Value,
) -> std::result::Result<Value, worker::DiagnosticCallbackError> {
    let options: Options =
        serde_json::from_value(payload).map_err(|_| worker::DiagnosticCallbackError)?;
    options.validate();

    super::health_callback(Value::Null)?;
    if !cfg!(feature = "mx-creative-hid") {
        return serde_json::to_value(failure(
            "unavailable: device-specific HID feature is disabled",
            options.icon,
        ))
        .map_err(|_| worker::DiagnosticCallbackError);
    }
    let backend = crate::native::hid::mx_creative_single_key_paint_backend()
        .map_err(|_| worker::DiagnosticCallbackError)?;
    let result = crate::devices::mx_creative::colored_icon_test::run(&backend, options);
    if !valid_result(&result) {
        return Err(worker::DiagnosticCallbackError);
    }
    serde_json::to_value(result).map_err(|_| worker::DiagnosticCallbackError)
}

fn failure(error: &str, icon: IconName) -> Result {
    Result {
        schema: 1,
        semantic_read_only: false,
        parent_readiness_form: ParentReadinessForm::for_icon(icon),
        operator_warning: "Visible confirmation is required before physical execution. Native SetReport success is not visual proof.".into(),
        rollback: "No refresh or cancel is sent. KeepAlive expires naturally near eight seconds from start and rolls back the selected volatile icon frame.".into(),
        visual_result_may_be_inconclusive: true,
        no_brightness_or_config_writes: true,
        keys_targeted: KEYS_TARGETED,
        error: Some(error.into()),
        ..Result::default()
    }
}

pub(super) fn valid_result(result: &Result) -> bool {
    let expected_hidpp_reports = EXPECTED_HIDPP_REPORTS;
    let expected_hid_output_reports = expected_hidpp_reports + TOTAL_PAINT_FRAMES;
    let expected_hid_output_bytes = 20 * usize::from(expected_hidpp_reports)
        + OUTPUT_REPORT_LEN * usize::from(TOTAL_PAINT_FRAMES);

    let basics = result.schema == 1
        && !result.semantic_read_only
        && !result.command_ready
        && !result.hardware_qualified
        && result.probe_succeeded == result.error.is_none()
        && result.no_brightness_or_config_writes
        && result.visual_result_may_be_inconclusive
        && result.retries == 0
        && result.keys_targeted == KEYS_TARGETED
        && result.keys_completed <= KEYS_TARGETED
        && result.elapsed_ms <= 30_000
        && result.error.as_deref().is_none_or(valid_error)
        && valid_readiness(&result.parent_readiness_form)
        && result.operator_warning
            == "Visible confirmation is required before physical execution. Native SetReport success is not visual proof."
        && result.rollback
            == "No refresh or cancel is sent. KeepAlive expires naturally near eight seconds from start and rolls back the selected volatile icon frame.";
    if !basics {
        return false;
    }

    if let Some(device) = &result.device
        && (device.vendor_id != 0x046d
            || device.product_id != 0xc354
            || device.transport != "usb"
            || device.primary_usage_page != 0xff43
            || !matches!(device.primary_usage, 0x1a02 | 0x1a10)
            || device.max_input_report_len != 32
            || device.max_output_report_len != 4095
            || device.report_descriptor_sha256
                != "d6faec8e21db00518c67b4446875b6bdb9d85988d05d888641da069fb302bc54")
    {
        return false;
    }

    if result.probe_succeeded {
        let Some(keepalive) = &result.keepalive else {
            return false;
        };
        let Some(paint) = &result.paint else {
            return false;
        };
        let Some(icon) = paint.icons.first() else {
            return false;
        };
        let Some((expected_key, _expected_name, expected_source_svg_path)) = expected_icon_specs()
            .into_iter()
            .find(|(_, expected_name, _)| *expected_name == icon.name)
        else {
            return false;
        };
        let expected_untouched = (1u8..=9)
            .filter(|key| *key != expected_key)
            .collect::<Vec<_>>();
        let expected_rect = expected_key_rects()[usize::from(expected_key - 1)];

        keepalive.feature_id == KEEPALIVE_FEATURE_ID
            && keepalive.feature_index != 0
            && keepalive.minimum_ms >= 500
            && keepalive.maximum_ms <= 10_000
            && keepalive.maximum_ms >= keepalive.minimum_ms
            && (keepalive.minimum_ms..=keepalive.maximum_ms).contains(&KEEPALIVE_MS)
            && keepalive.requested_ms == KEEPALIVE_MS
            && keepalive.activation_accepted
            && keepalive.activation_reports_sent == EXPECTED_KEEPALIVE_ACTIVATIONS
            && keepalive.refresh_interval_ms == KEEPALIVE_REFRESH_MS
            && keepalive.hidpp_error.is_none()
            && paint.report_id == OUTPUT_REPORT_ID
            && paint.total_report_len == OUTPUT_REPORT_LEN
            && paint.header_len == 20
            && paint.frame_function_swid == 0x21
            && paint.frame_sequence == 0xe0
            && paint.key_rects == vec![expected_rect]
            && paint.targeted_keys == vec![expected_key]
            && paint.untouched_keys == expected_untouched
            && paint.source_repository == "fes/fesReview-mx-icons"
            && paint.source_commit == "7a339c9"
            && paint.source_svg_root == "assets/mx-keypad-icons"
            && paint.icons.len() == 1
            && icon.key_index == expected_key
            && icon.source_svg_path == expected_source_svg_path
            && valid_digest(&icon.source_png_sha256)
            && icon.jpeg_baseline_jfif
            && !icon.jpeg_contains_app1
            && icon.jpeg_len != 0
            && icon.jpeg_len <= MAX_JPEG_LEN
            && valid_digest(&icon.jpeg_sha256)
            && paint.total_frames_expected == TOTAL_PAINT_FRAMES
            && paint.total_frames_sent == TOTAL_PAINT_FRAMES
            && paint.hold_ms == HOLD_MS
            && paint.rollback_wait_ms == ROLLBACK_WAIT_MS
            && paint.hold_wait_completed
            && paint.rollback_wait_completed
            && result.hidpp_reports_sent == expected_hidpp_reports
            && result.paint_reports_sent == TOTAL_PAINT_FRAMES
            && result.hid_output_reports_sent == expected_hid_output_reports
            && result.hid_output_bytes_sent == expected_hid_output_bytes
            && result.keys_completed == KEYS_TARGETED
            && result.device.is_some()
    } else {
        result.hidpp_reports_sent <= expected_hidpp_reports
            && result.paint_reports_sent <= TOTAL_PAINT_FRAMES
            && result.hid_output_reports_sent <= expected_hid_output_reports
            && result.hid_output_bytes_sent <= expected_hid_output_bytes
            && result.keys_completed <= KEYS_TARGETED
            && result
                .keepalive
                .as_ref()
                .is_none_or(|keepalive| keepalive.requested_ms == KEEPALIVE_MS)
            && result.paint.as_ref().is_none_or(|paint| {
                paint.total_frames_expected == TOTAL_PAINT_FRAMES
                    && paint.targeted_keys.len() <= 1
                    && paint.icons.len() <= 1
            })
    }
}

fn valid_readiness(form: &ParentReadinessForm) -> bool {
    let Some(icon_name) = form
        .command
        .strip_prefix("mx-creative-colored-icon-test --confirm-colored-icon ")
    else {
        return false;
    };
    let Ok(icon) = icon_name.parse::<IconName>() else {
        return false;
    };

    form.required
        && form.tool == "ask_user"
        && form.title == "MX Creative single colored icon readiness"
        && form.accept_phrase == "READY_COLORED_ICON"
        && form.countdown_seconds == 3
        && form.command
            == format!(
                "mx-creative-colored-icon-test --confirm-colored-icon {}",
                icon.as_str()
            )
        && form.prompt.contains(icon.as_str())
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_error(error: &str) -> bool {
    matches!(
        error,
        "unavailable: device-specific HID feature is disabled"
            | "keypad output route requires exactly one qualifying collection; found 0"
            | "keypad output route requires exactly one qualifying collection; found 2"
            | "keypad output route descriptor mismatch"
            | "keypad display output route absent"
            | "keypad output route identity or transport mismatch"
            | "keypad reopened descriptor mismatch"
            | "keepalive feature absent"
            | "keepalive feature query rejected"
            | "keepalive range query rejected"
            | "invalid KeepAlive timeout range"
            | "unexpected keepalive feature response"
            | "unexpected keepalive range response"
            | "keepalive activation rejected"
            | "unexpected keepalive activation response"
            | "derived key rectangle outside 480x480 bounds"
            | "source PNG dimensions must be exactly 480x480"
            | "icon JPEG payload length is invalid"
            | "icon JPEG markers are invalid"
            | "icon JPEG missing baseline JFIF APP0"
            | "icon JPEG still contains APP1 metadata"
            | "icon JPEG must remain baseline (non-progressive)"
            | "JPEG markers are invalid"
            | "JPEG segment length is invalid"
            | "icon RGB payload length is invalid"
            | "PNG width overflow"
            | "PNG height overflow"
            | "PNG dimensions must be non-zero"
            | "PNG RGBA length mismatch"
            | "PNG grayscale length mismatch"
            | "PNG grayscale+alpha length mismatch"
            | "PNG palette should be expanded before decode"
            | "decoded PNG RGB length mismatch"
            | "resize dimensions must be non-zero"
            | "resize source RGB length mismatch"
            | "invalid JPEG payload length"
            | "HID transport unavailable"
            | "Unavailable(FeatureDisabled)"
            | "Unavailable(UnsupportedPlatform)"
            | "Unavailable(UnsupervisedAccess)"
            | "Unavailable(UnsupervisedWrite)"
            | "NotFound"
            | "Timeout"
            | "TooManyDevices"
            | "EvidenceUnavailable(RawReportDescriptor)"
            | "Protocol(\"MX display query software IDs exhausted\")"
            | "Protocol(\"MX display query request count exhausted\")"
            | "Protocol(\"MX display query report was not fully written\")"
            | "Protocol(\"unexpected MX display response framing\")"
            | "Protocol(\"unexpected MX display response device\")"
            | "Protocol(\"uncorrelated MX display response function\")"
            | "Protocol(\"uncorrelated MX display HID++ error function\")"
            | "Protocol(\"unexpected MX display response software ID\")"
            | "Protocol(\"unexpected MX display HID++ error software ID\")"
            | "Protocol(\"MX single-key paint report was not fully written\")"
            | "Protocol(\"implausible device count\")"
            | "Protocol(\"HID run loop stopped or finished\")"
            | "Protocol(\"unexpected HID run loop result\")"
            | "Protocol(\"read count exceeds buffer\")"
            | "Protocol(\"backend returned unmatched descriptor\")"
            | "Protocol(\"report descriptor length is outside 1..=1024\")"
    ) || valid_os_error(error)
        || error.starts_with("PNG decode failed:")
        || error.starts_with("PNG frame decode failed:")
        || error.starts_with("icon JPEG encoding failed:")
}

fn valid_os_error(error: &str) -> bool {
    let Some(rest) = error.strip_prefix("Os { op: ") else {
        return false;
    };
    let Some((operation, code)) = rest.split_once(", code: ") else {
        return false;
    };
    let Some(code) = code.strip_suffix(" }") else {
        return false;
    };
    matches!(operation, "Enumerate" | "Open" | "Close" | "Write" | "Read")
        && !code.is_empty()
        && code.parse::<i64>().is_ok()
}

fn json_string(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| worker_error(()))
}

fn worker_error(_: impl std::fmt::Debug) -> String {
    "{\"status\":\"worker_error\",\"error\":\"invalid MX colored-icon-test result\"}\n".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_requires_exact_confirmation_flag() {
        assert!(parse(&["--confirm-colored-icon".into(), "play-pause".into()]).is_ok());
        assert!(parse(&[]).is_err());
        assert!(parse(&["--confirm-colored-icon".into()]).is_err());
        assert!(parse(&["--confirm-colored-icon".into(), "unknown".into()]).is_err());
        assert!(
            parse(&[
                "--confirm-colored-icon".into(),
                "play-pause".into(),
                "--extra".into()
            ])
            .is_err()
        );
    }

    #[test]
    fn parent_validator_rejects_forged_counts_and_accepts_bounded_success() {
        let success = Result {
            schema: 1,
            semantic_read_only: false,
            probe_succeeded: true,
            command_ready: false,
            hardware_qualified: false,
            device: Some(crate::devices::mx_creative::colored_icon_test::DeviceInfo {
                vendor_id: 0x046d,
                product_id: 0xc354,
                transport: "usb".into(),
                primary_usage_page: 0xff43,
                primary_usage: 0x1a02,
                max_input_report_len: 32,
                max_output_report_len: 4095,
                report_descriptor_sha256:
                    "d6faec8e21db00518c67b4446875b6bdb9d85988d05d888641da069fb302bc54".into(),
            }),
            keepalive: Some(crate::devices::mx_creative::colored_icon_test::KeepAliveEvidence {
                feature_id: KEEPALIVE_FEATURE_ID,
                feature_index: 6,
                minimum_ms: 500,
                maximum_ms: 10_000,
                requested_ms: KEEPALIVE_MS,
                activation_accepted: true,
                activation_reports_sent: EXPECTED_KEEPALIVE_ACTIVATIONS,
                refresh_interval_ms: KEEPALIVE_REFRESH_MS,
                hidpp_error: None,
            }),
            paint: Some(crate::devices::mx_creative::colored_icon_test::PaintEvidence {
                report_id: OUTPUT_REPORT_ID,
                total_report_len: OUTPUT_REPORT_LEN,
                header_len: 20,
                frame_function_swid: 0x21,
                frame_sequence: 0xe0,
                key_rects: vec![expected_key_rects()[0]],
                targeted_keys: vec![1],
                untouched_keys: vec![2, 3, 4, 5, 6, 7, 8, 9],
                source_repository: "fes/fesReview-mx-icons".into(),
                source_commit: "7a339c9".into(),
                source_svg_root: "assets/mx-keypad-icons".into(),
                icons: vec![crate::devices::mx_creative::colored_icon_test::IconEvidence {
                    key_index: 1,
                    name: "play-pause".into(),
                    source_svg_path: "play-pause.svg".into(),
                    source_png_sha256: "0".repeat(64),
                    jpeg_len: 3000,
                    jpeg_sha256: "0".repeat(64),
                    jpeg_baseline_jfif: true,
                    jpeg_contains_app1: false,
                }],
                total_frames_expected: TOTAL_PAINT_FRAMES,
                total_frames_sent: TOTAL_PAINT_FRAMES,
                hold_ms: HOLD_MS,
                rollback_wait_ms: ROLLBACK_WAIT_MS,
                hold_wait_completed: true,
                rollback_wait_completed: true,
            }),
            parent_readiness_form: ParentReadinessForm::for_icon(IconName::PlayPause),
            operator_warning: "Visible confirmation is required before physical execution. Native SetReport success is not visual proof.".into(),
            rollback:
                "No refresh or cancel is sent. KeepAlive expires naturally near eight seconds from start and rolls back the selected volatile icon frame.".into(),
            visual_result_may_be_inconclusive: true,
            no_brightness_or_config_writes: true,
            hidpp_reports_sent: EXPECTED_HIDPP_REPORTS,
            paint_reports_sent: TOTAL_PAINT_FRAMES,
            hid_output_reports_sent: EXPECTED_HIDPP_REPORTS + TOTAL_PAINT_FRAMES,
            hid_output_bytes_sent: 20 * usize::from(EXPECTED_HIDPP_REPORTS)
                + OUTPUT_REPORT_LEN * usize::from(TOTAL_PAINT_FRAMES),
            retries: 0,
            keys_targeted: KEYS_TARGETED,
            keys_completed: KEYS_TARGETED,
            elapsed_ms: 20_000,
            error: None,
        };
        assert!(valid_result(&success));
        let mut invalid = success.clone();
        invalid.paint_reports_sent += 1;
        assert!(!valid_result(&invalid));
    }
}
