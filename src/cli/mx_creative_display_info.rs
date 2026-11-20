use std::{sync::atomic::AtomicBool, time::Duration};

use serde_json::{Value, json};

use crate::{
    devices::mx_creative::display_info::{
        FeatureInfo, FeatureStatus, KeepAliveRange, Options, Result,
    },
    native::windows_worker as worker,
};

use super::Output;

pub(super) const KIND: &str = "mx_creative_display_info";

pub(super) fn parse(args: &[String]) -> std::result::Result<Options, String> {
    if !args.is_empty() {
        return Err("mx-creative-display-info accepts no arguments".into());
    }
    Ok(Options::default())
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
    let object = payload.as_object().ok_or(worker::DiagnosticCallbackError)?;
    if !object.is_empty() {
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
        ))
        .map_err(|_| worker::DiagnosticCallbackError);
    }
    let backend = crate::native::hid::mx_creative_display_info_backend()
        .map_err(|_| worker::DiagnosticCallbackError)?;
    let result = crate::devices::mx_creative::display_info::run(&backend, options);
    if !valid_result(&result) {
        return Err(worker::DiagnosticCallbackError);
    }
    serde_json::to_value(result).map_err(|_| worker::DiagnosticCallbackError)
}

fn failure(error: &str) -> Result {
    Result {
        schema: 1,
        semantic_read_only: true,
        error: Some(error.into()),
        ..Result::default()
    }
}

pub(super) fn valid_result(result: &Result) -> bool {
    let basics = result.schema == 1
        && result.semantic_read_only
        && !result.command_ready
        && !result.hardware_qualified
        && result.probe_succeeded == result.error.is_none()
        && result.requests_sent <= 5
        && result.elapsed_ms <= 16_000
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
            || result.keepalive_range.is_some())
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
    if result.probe_succeeded
        && (result.features.len() != 4
            || result.requests_sent
                != 4 + u8::from(
                    result
                        .features
                        .first()
                        .is_some_and(|feature| feature.status == FeatureStatus::Present),
                ))
    {
        return false;
    }
    !result.probe_succeeded || result.device.is_some()
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
                        && feature.metadata_status == "hidpp_error"))
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
            requests_sent: 4,
            elapsed_ms: 10,
            error: None,
        };
        assert!(valid_result(&result));
        let response = DiagnosticResult {
            worker: WorkerIdentity {
                pid: 1,
                creation_time_100ns: 1,
            },
            payload: serde_json::to_value(&result).unwrap(),
        };
        assert!(execute_with(Options::default(), |_, _| Ok(response)).is_ok());
        let mut invalid = result;
        invalid.features[1].metadata_status = "brightness_current".into();
        assert!(!valid_result(&invalid));
    }
}
