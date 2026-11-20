use std::{sync::atomic::AtomicBool, time::Duration};

use serde_json::{Value, json};

use crate::{
    devices::mx_creative::display_info::{
        BrightnessGet, FeatureInfo, FeatureStatus, KEEPALIVE_CHECK_MS, KeepAliveCheck,
        KeepAliveRange, Options, Result,
    },
    native::windows_worker as worker,
};

use super::Output;

pub(super) const KIND: &str = "mx_creative_display_info";

pub(super) fn parse(args: &[String]) -> std::result::Result<Options, String> {
    if args.is_empty() {
        return Ok(Options::default());
    }
    if args.len() != 2 || args[0] != "--hold-keepalive-ms" || args[1] != "1000" {
        return Err("hold check requires exactly --hold-keepalive-ms 1000".into());
    }
    let options = Options {
        keepalive_check_ms: Some(KEEPALIVE_CHECK_MS),
    };
    options.validate().map_err(str::to_owned)?;
    Ok(options)
}

pub(super) fn execute(options: Options) -> std::result::Result<Output, String> {
    let allowlist =
        super::mx_creative_display_info_allowlist().map_err(|_| worker_error("allowlist"))?;
    execute_with(options, move |request, budget| {
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
    options.validate().map_err(worker_error)?;
    let request = worker::DiagnosticRequest {
        kind: KIND.into(),
        payload: serde_json::to_value(&options).map_err(|_| worker_error(()))?,
    };
    let response = launch(&request, options.budget())
        .map_err(|error| json_string(&json!({"status":"worker_error","error":error})))?;
    let result: Result = serde_json::from_value(response.payload)
        .map_err(|_| worker_error(worker::WorkerErrorKind::Protocol))?;
    if !valid_result(&result, &options) {
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
    let object = payload.as_object().ok_or(worker::DiagnosticCallbackError)?;
    if !object.is_empty() && !(object.len() == 1 && object.contains_key("keepalive_check_ms")) {
        return Err(worker::DiagnosticCallbackError);
    }
    let options: Options =
        serde_json::from_value(payload).map_err(|_| worker::DiagnosticCallbackError)?;
    options
        .validate()
        .map_err(|_| worker::DiagnosticCallbackError)?;
    super::health_callback(Value::Null)?;
    if !cfg!(feature = "mx-creative-hid") {
        return serde_json::to_value(failure(
            "unavailable: device-specific HID feature is disabled",
            options.keepalive_check_ms.is_none(),
        ))
        .map_err(|_| worker::DiagnosticCallbackError);
    }
    let backend = crate::native::hid::mx_creative_display_info_backend()
        .map_err(|_| worker::DiagnosticCallbackError)?;
    let result = crate::devices::mx_creative::display_info::run(&backend, options.clone());
    if !valid_result(&result, &options) {
        return Err(worker::DiagnosticCallbackError);
    }
    serde_json::to_value(result).map_err(|_| worker::DiagnosticCallbackError)
}

fn failure(error: &str, semantic_read_only: bool) -> Result {
    Result {
        schema: 1,
        semantic_read_only,
        error: Some(error.into()),
        ..Result::default()
    }
}

pub(super) fn valid_result(result: &Result, options: &Options) -> bool {
    let check_authorized = options.keepalive_check_ms.is_some();
    let basics = result.schema == 1
        && result.semantic_read_only == !check_authorized
        && !result.command_ready
        && !result.hardware_qualified
        && result.probe_succeeded == result.error.is_none()
        && result.requests_sent <= 8
        && result.elapsed_ms
            <= if result.semantic_read_only {
                16_000
            } else {
                18_000
            }
        && result.features.len() <= 4
        && result.error.as_deref().is_none_or(valid_error);
    if !basics {
        return false;
    }
    if let Some(device) = &result.device
        && (device.vendor_id != 0x046d
            || device.product_id != 0xc354
            || device.transport != "usb"
            || device.primary_usage_page != 0xff43
            || device.primary_usage != 0x1a02
            || device.max_input_report_len != 32
            || device.max_output_report_len != 4095
            || device.report_descriptor_sha256
                != "d6faec8e21db00518c67b4446875b6bdb9d85988d05d888641da069fb302bc54")
    {
        return false;
    }
    if result.device.is_none()
        && (result.probe_succeeded
            || !result.features.is_empty()
            || result.keepalive_range.is_some()
            || result.brightness_get.is_some()
            || result.keepalive_check.is_some())
    {
        return false;
    }
    if result
        .features
        .iter()
        .enumerate()
        .any(|(index, feature)| !valid_feature(feature, index))
    {
        return false;
    }
    if (result.probe_succeeded || result.keepalive_range.is_some())
        && !valid_keepalive(result.keepalive_range.as_ref(), result.features.first())
    {
        return false;
    }
    if !valid_brightness_get(
        result.brightness_get.as_ref(),
        result.features.get(1),
        check_authorized,
        result.probe_succeeded,
    ) || !valid_keepalive_check(
        result.keepalive_check.as_ref(),
        result.keepalive_range.as_ref(),
        result.probe_succeeded,
    ) {
        return false;
    }
    if result.probe_succeeded
        && (result.features.len() != 4 || result.requests_sent != expected_requests(result))
    {
        return false;
    }
    if result.probe_succeeded
        && check_authorized
        && (!valid_required_brightness(result) || !valid_required_keepalive_hold(result))
    {
        return false;
    }
    !result.probe_succeeded || result.device.is_some()
}

fn expected_requests(result: &Result) -> u8 {
    let keepalive_range = u8::from(
        result
            .features
            .first()
            .is_some_and(|feature| feature.status == FeatureStatus::Present),
    );
    let brightness_gets = if result.brightness_get.is_some() {
        2
    } else {
        0
    };
    let keepalive_check = u8::from(result.keepalive_check.is_some());
    4 + keepalive_range + brightness_gets + keepalive_check
}

fn valid_brightness_get(
    brightness: Option<&BrightnessGet>,
    feature: Option<&FeatureInfo>,
    check_authorized: bool,
    probe_succeeded: bool,
) -> bool {
    match (brightness, feature) {
        (None, _) if !check_authorized => true,
        (None, Some(feature)) => !probe_succeeded || feature.status != FeatureStatus::Present,
        (None, None) => !probe_succeeded,
        (Some(_), _) if !check_authorized => false,
        (Some(brightness), Some(feature)) => {
            feature.id == 0x8040
                && feature.status == FeatureStatus::Present
                && brightness.feature_id == feature.id
                && brightness.feature_index == feature.feature_index.unwrap_or(0)
                && brightness.feature_index != 0
                && brightness.info_sha256.as_deref().is_none_or(valid_digest)
                && !(brightness.info_sha256.is_some() && brightness.info_hidpp_error.is_some())
                && !(brightness.current.is_some() && brightness.current_hidpp_error.is_some())
                && (!probe_succeeded
                    || (brightness.info_sha256.is_some() || brightness.info_hidpp_error.is_some())
                        && (brightness.current.is_some()
                            || brightness.current_hidpp_error.is_some()))
        }
        (Some(_), None) => false,
    }
}

fn valid_keepalive_check(
    check: Option<&KeepAliveCheck>,
    range: Option<&KeepAliveRange>,
    probe_succeeded: bool,
) -> bool {
    match (check, range) {
        (None, _) => true,
        (Some(check), Some(range)) => {
            check.feature_id == 0x0008
                && check.feature_index == range.feature_index
                && check.requested_ms == KEEPALIVE_CHECK_MS
                && (range.minimum_ms..=range.maximum_ms).contains(&KEEPALIVE_CHECK_MS)
                && (!probe_succeeded
                    || if check.activation_accepted {
                        check.hidpp_error.is_none() && check.idle_wait_completed
                    } else {
                        check.hidpp_error.is_some() && !check.idle_wait_completed
                    })
        }
        (Some(_), None) => false,
    }
}

fn valid_required_brightness(result: &Result) -> bool {
    result
        .features
        .get(1)
        .is_none_or(|feature| feature.status != FeatureStatus::Present)
        || result.brightness_get.is_some()
}

fn valid_required_keepalive_hold(result: &Result) -> bool {
    result
        .keepalive_range
        .as_ref()
        .is_none_or(|range| !(range.minimum_ms..=range.maximum_ms).contains(&KEEPALIVE_CHECK_MS))
        || result.keepalive_check.is_some()
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_feature(feature: &FeatureInfo, position: usize) -> bool {
    use crate::native::hid::MxCreativeDisplayFeature as Feature;
    let Some(expected) = [
        Feature::KeepAlive,
        Feature::Brightness,
        Feature::Display,
        Feature::Controls,
    ]
    .get(position)
    .copied() else {
        return false;
    };
    if feature.id != expected.id() {
        return false;
    }
    match feature.status {
        FeatureStatus::Present => {
            let authorized = match expected {
                Feature::KeepAlive => "range_query_authorized",
                Feature::Brightness => "brightness_get_not_authorized",
                Feature::Display => "geometry_get_not_authorized",
                Feature::Controls => "metadata_get_not_authorized",
            };
            feature.feature_index.is_some_and(|index| index != 0)
                && ((feature.hidpp_error.is_none() && feature.metadata_status == authorized)
                    || (expected == Feature::KeepAlive
                        && feature.hidpp_error.is_some()
                        && feature.metadata_status == "hidpp_error")
                    || (expected == Feature::Brightness
                        && feature.hidpp_error.is_none()
                        && feature.metadata_status == "brightness_get_check_authorized"))
        }
        FeatureStatus::Absent => {
            feature.feature_index.is_none()
                && feature.hidpp_error.is_none_or(|code| matches!(code, 1 | 6))
                && feature.metadata_status == "not_applicable"
        }
        FeatureStatus::Unsupported => {
            feature.feature_index.is_none()
                && feature.hidpp_error == Some(9)
                && feature.metadata_status == "not_applicable"
        }
        FeatureStatus::QueryError => {
            feature.feature_index.is_none()
                && feature
                    .hidpp_error
                    .is_some_and(|code| !matches!(code, 1 | 6 | 9))
                && feature.metadata_status == "query_error"
        }
    }
}

fn valid_keepalive(range: Option<&KeepAliveRange>, feature: Option<&FeatureInfo>) -> bool {
    match (range, feature) {
        (None, Some(feature)) => {
            feature.status != FeatureStatus::Present
                || feature.hidpp_error.is_some()
                || feature.metadata_status == "hidpp_error"
        }
        (None, None) => true,
        (Some(range), Some(feature)) => {
            feature.status == FeatureStatus::Present
                && feature.feature_index == Some(range.feature_index)
                && range.feature_id == 0x0008
                && range.minimum_ms != 0
                && range.maximum_ms >= range.minimum_ms
        }
        (Some(_), None) => false,
    }
}

fn valid_error(error: &str) -> bool {
    matches!(
        error,
        "unavailable: device-specific HID feature is disabled"
            | "keypad requires exactly one collection; found 0"
            | "keypad requires exactly one collection; found 2"
            | "keypad identity or transport mismatch"
            | "keypad report descriptor mismatch"
            | "keypad reopened descriptor mismatch"
            | "keypad product name unavailable"
            | "invalid KeepAlive timeout range"
            | "unexpected KeepAlive response"
            | "unexpected brightness info response"
            | "unexpected brightness GET response"
            | "unexpected KeepAlive check response"
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
            | "Protocol(\"implausible device count\")"
            | "Protocol(\"HID run loop stopped or finished\")"
            | "Protocol(\"unexpected HID run loop result\")"
            | "Protocol(\"read count exceeds buffer\")"
            | "Protocol(\"backend returned unmatched descriptor\")"
            | "Protocol(\"report descriptor length is outside 1..=1024\")"
    ) || valid_os_error(error)
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
    "{\"status\":\"worker_error\",\"error\":\"invalid MX display-info result\"}\n".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::windows_worker::{DiagnosticResult, WorkerIdentity};

    #[test]
    fn command_accepts_only_empty_options() {
        assert_eq!(parse(&[]), Ok(Options::default()));
        assert_eq!(
            parse(&["--hold-keepalive-ms".into(), "1000".into()]),
            Ok(Options {
                keepalive_check_ms: Some(1000)
            })
        );
        assert!(parse(&["--hold-keepalive-ms".into()]).is_err());
        assert!(parse(&["--hold-keepalive-ms".into(), "500".into()]).is_err());
        assert!(parse(&["--hold-keepalive-ms".into(), "01000".into()]).is_err());
        assert!(parse(&["--query".into()]).is_err());
        assert!(parse(&["unexpected".into()]).is_err());
    }

    #[test]
    fn parent_accepts_only_bounded_semantic_result() {
        let result = Result {
            schema: 1,
            semantic_read_only: true,
            probe_succeeded: true,
            command_ready: false,
            hardware_qualified: false,
            device: Some(crate::devices::mx_creative::display_info::DeviceInfo {
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
            features: vec![
                FeatureInfo {
                    id: 0x0008,
                    status: FeatureStatus::Absent,
                    feature_index: None,
                    hidpp_error: None,
                    metadata_status: "not_applicable".into(),
                },
                FeatureInfo {
                    id: 0x8040,
                    status: FeatureStatus::Present,
                    feature_index: Some(8),
                    hidpp_error: None,
                    metadata_status: "brightness_get_not_authorized".into(),
                },
                FeatureInfo {
                    id: 0x19a1,
                    status: FeatureStatus::Absent,
                    feature_index: None,
                    hidpp_error: None,
                    metadata_status: "not_applicable".into(),
                },
                FeatureInfo {
                    id: 0x1b04,
                    status: FeatureStatus::Absent,
                    feature_index: None,
                    hidpp_error: None,
                    metadata_status: "not_applicable".into(),
                },
            ],
            keepalive_range: None,
            brightness_get: None,
            keepalive_check: None,
            requests_sent: 4,
            elapsed_ms: 10,
            error: None,
        };
        assert!(valid_result(&result, &Options::default()));
        let response = DiagnosticResult {
            worker: WorkerIdentity {
                pid: 1,
                creation_time_100ns: 1,
            },
            payload: serde_json::to_value(&result).unwrap(),
        };
        assert!(execute_with(Options::default(), |_, _| Ok(response)).is_ok());
        let mut invalid = result.clone();
        invalid.features[1].metadata_status = "brightness_current".into();
        assert!(!valid_result(&invalid, &Options::default()));

        let mut checked = result;
        let explicit = Options {
            keepalive_check_ms: Some(1000),
        };
        checked.semantic_read_only = false;
        checked.features[1].metadata_status = "brightness_get_check_authorized".into();
        checked.brightness_get = Some(BrightnessGet {
            feature_id: 0x8040,
            feature_index: 8,
            current: Some(42),
            info_sha256: Some("0".repeat(64)),
            info_hidpp_error: None,
            current_hidpp_error: None,
        });
        checked.requests_sent = 6;
        assert!(valid_result(&checked, &explicit));
        let mut missing_hold_authority = checked;
        missing_hold_authority.semantic_read_only = true;
        assert!(!valid_result(&missing_hold_authority, &explicit));

        let unavailable = Result {
            schema: 1,
            semantic_read_only: false,
            error: Some("NotFound".into()),
            ..Result::default()
        };
        assert!(valid_result(&unavailable, &explicit));
    }
}
