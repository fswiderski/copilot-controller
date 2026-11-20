use std::{sync::atomic::AtomicBool, time::Duration};

use serde_json::{Value, json};

use crate::{
    devices::mx_creative::single_key_paint::{
        BUDGET, HOLD_MS, KEEPALIVE_FEATURE_ID, KEEPALIVE_MS, KEY1_CYAN_RGB, KEY1_H, KEY1_W, KEY1_X,
        KEY1_Y, OUTPUT_REPORT_ID, OUTPUT_REPORT_LEN, ParentReadinessForm, Result,
    },
    native::windows_worker as worker,
};

use super::Output;

pub(super) const KIND: &str = "mx_creative_single_key_paint";

pub(super) fn parse(args: &[String]) -> std::result::Result<(), String> {
    match args {
        [arg] if arg == "--confirm-write-single-key" => Ok(()),
        _ => Err(
            "single-key paint requires exactly --confirm-write-single-key and no extra args".into(),
        ),
    }
}

pub(super) fn execute() -> std::result::Result<Output, String> {
    let allowlist =
        super::mx_creative_single_key_paint_allowlist().map_err(|_| worker_error("allowlist"))?;
    execute_with(|request, budget| {
        worker::probe(request, budget, &AtomicBool::new(false), &allowlist)
    })
}

fn execute_with(
    launch: impl FnOnce(
        &worker::DiagnosticRequest,
        Duration,
    ) -> std::result::Result<worker::DiagnosticResult, worker::WorkerError>,
) -> std::result::Result<Output, String> {
    let request = worker::DiagnosticRequest {
        kind: KIND.into(),
        payload: Value::Null,
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
    if !payload.is_null() {
        return Err(worker::DiagnosticCallbackError);
    }
    super::health_callback(Value::Null)?;
    if !cfg!(feature = "mx-creative-hid") {
        return serde_json::to_value(failure(
            "unavailable: device-specific HID feature is disabled",
        ))
        .map_err(|_| worker::DiagnosticCallbackError);
    }
    let backend = crate::native::hid::mx_creative_single_key_paint_backend()
        .map_err(|_| worker::DiagnosticCallbackError)?;
    let result = crate::devices::mx_creative::single_key_paint::run(&backend);
    if !valid_result(&result) {
        return Err(worker::DiagnosticCallbackError);
    }
    serde_json::to_value(result).map_err(|_| worker::DiagnosticCallbackError)
}

fn failure(error: &str) -> Result {
    Result {
        schema: 1,
        semantic_read_only: false,
        parent_readiness_form: ParentReadinessForm::default(),
        operator_warning: "Visible confirmation is required before physical execution. Native SetReport success is not visual proof.".into(),
        rollback: "No refresh or cancel is sent. Natural KeepAlive expiry rolls back this one-key experiment.".into(),
        visual_result_may_be_inconclusive: true,
        no_brightness_or_config_writes: true,
        hold_ms: HOLD_MS,
        error: Some(error.into()),
        ..Result::default()
    }
}

pub(super) fn valid_result(result: &Result) -> bool {
    let basics = result.schema == 1
        && !result.semantic_read_only
        && !result.command_ready
        && !result.hardware_qualified
        && result.probe_succeeded == result.error.is_none()
        && result.no_brightness_or_config_writes
        && result.visual_result_may_be_inconclusive
        && result.retries == 0
        && result.hold_ms == HOLD_MS
        && result.elapsed_ms <= 14_000
        && result.error.as_deref().is_none_or(valid_error)
        && valid_readiness(&result.parent_readiness_form)
        && result.operator_warning
            == "Visible confirmation is required before physical execution. Native SetReport success is not visual proof."
        && result.rollback
            == "No refresh or cancel is sent. Natural KeepAlive expiry rolls back this one-key experiment.";
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
        keepalive.feature_id == KEEPALIVE_FEATURE_ID
            && keepalive.feature_index != 0
            && keepalive.minimum_ms >= 500
            && keepalive.maximum_ms <= 10_000
            && keepalive.maximum_ms >= keepalive.minimum_ms
            && (keepalive.minimum_ms..=keepalive.maximum_ms).contains(&KEEPALIVE_MS)
            && keepalive.requested_ms == KEEPALIVE_MS
            && keepalive.activation_accepted
            && keepalive.hidpp_error.is_none()
            && paint.report_id == OUTPUT_REPORT_ID
            && paint.total_report_len == OUTPUT_REPORT_LEN
            && paint.header_len == 20
            && paint.frame_function_swid == 0x21
            && paint.frame_sequence == 0xe0
            && paint.rect.x == KEY1_X
            && paint.rect.y == KEY1_Y
            && paint.rect.width == KEY1_W
            && paint.rect.height == KEY1_H
            && paint.rect_provenance.source
                == "public secondary reverse-engineering report with conservative key-1 grid"
            && paint.rect_provenance.confidence == "medium"
            && paint.color_rgb == KEY1_CYAN_RGB
            && paint.jpeg_len < 4075
            && valid_digest(&paint.jpeg_sha256)
            && !paint.image_ack_expected
            && result.hidpp_reports_sent == 3
            && result.paint_reports_sent == 1
            && result.hid_output_reports_sent == 4
            && result.hid_output_bytes_sent == 20 * 3 + OUTPUT_REPORT_LEN
            && result.hold_wait_completed
            && result.device.is_some()
    } else {
        result.hidpp_reports_sent <= 3
            && result.paint_reports_sent <= 1
            && result.hid_output_reports_sent <= 4
            && result.hid_output_bytes_sent <= 20 * 3 + OUTPUT_REPORT_LEN
            && result
                .keepalive
                .as_ref()
                .is_none_or(|keepalive| keepalive.requested_ms == KEEPALIVE_MS)
            && result
                .paint
                .as_ref()
                .is_none_or(|paint| paint.rect.width == KEY1_W && paint.rect.height == KEY1_H)
    }
}

fn valid_readiness(form: &ParentReadinessForm) -> bool {
    form.required
        && form.tool == "ask_user"
        && form.title == "MX Creative single-key paint readiness"
        && form.accept_phrase == "READY_SINGLE_KEY_PAINT"
        && form.countdown_seconds == 3
        && form.command == "mx-creative-single-key-paint --confirm-write-single-key"
        && !form.prompt.is_empty()
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
            | "invalid JPEG payload length"
            | "JPEG markers are invalid"
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
    ) || error.starts_with("JPEG encoding failed:")
        || error.starts_with("encoded JPEG length ")
        || valid_os_error(error)
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
    "{\"status\":\"worker_error\",\"error\":\"invalid MX single-key paint result\"}\n".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_requires_exact_confirmation_flag() {
        assert!(parse(&["--confirm-write-single-key".into()]).is_ok());
        assert!(parse(&[]).is_err());
        assert!(parse(&["--confirm-write-single-key".into(), "--extra".into()]).is_err());
    }

    #[test]
    fn parent_validator_rejects_forged_counts_and_accepts_bounded_success() {
        let mut success = Result {
            schema: 1,
            semantic_read_only: false,
            probe_succeeded: true,
            command_ready: false,
            hardware_qualified: false,
            device: Some(crate::devices::mx_creative::single_key_paint::DeviceInfo {
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
            keepalive: Some(crate::devices::mx_creative::single_key_paint::KeepAliveEvidence {
                feature_id: KEEPALIVE_FEATURE_ID,
                feature_index: 6,
                minimum_ms: 500,
                maximum_ms: 10_000,
                requested_ms: KEEPALIVE_MS,
                activation_accepted: true,
                hidpp_error: None,
            }),
            paint: Some(crate::devices::mx_creative::single_key_paint::PaintEvidence {
                report_id: OUTPUT_REPORT_ID,
                total_report_len: OUTPUT_REPORT_LEN,
                header_len: 20,
                frame_function_swid: 0x21,
                frame_sequence: 0xe0,
                rect: crate::devices::mx_creative::single_key_paint::PaintRect {
                    x: KEY1_X,
                    y: KEY1_Y,
                    width: KEY1_W,
                    height: KEY1_H,
                },
                rect_provenance: crate::devices::mx_creative::single_key_paint::PublicProvenance {
                    source:
                        "public secondary reverse-engineering report with conservative key-1 grid"
                            .into(),
                    confidence: "medium".into(),
                },
                color_rgb: KEY1_CYAN_RGB,
                jpeg_len: 768,
                jpeg_sha256: "0".repeat(64),
                image_ack_expected: false,
            }),
            parent_readiness_form: ParentReadinessForm::default(),
            operator_warning: "Visible confirmation is required before physical execution. Native SetReport success is not visual proof.".into(),
            rollback:
                "No refresh or cancel is sent. Natural KeepAlive expiry rolls back this one-key experiment.".into(),
            visual_result_may_be_inconclusive: true,
            no_brightness_or_config_writes: true,
            hidpp_reports_sent: 3,
            paint_reports_sent: 1,
            hid_output_reports_sent: 4,
            hid_output_bytes_sent: 20 * 3 + OUTPUT_REPORT_LEN,
            retries: 0,
            hold_ms: HOLD_MS,
            hold_wait_completed: true,
            elapsed_ms: 2500,
            error: None,
        };
        assert!(valid_result(&success));
        success.hid_output_reports_sent = 5;
        assert!(!valid_result(&success));
    }
}
