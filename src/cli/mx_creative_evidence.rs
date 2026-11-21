use std::{sync::atomic::AtomicBool, time::Duration};

use serde_json::{Value, json};

use crate::{
    devices::mx_creative::probe::{self, Options, Result},
    native::windows_worker as worker,
};

use super::Output;

pub(super) const KIND: &str = "mx_creative_evidence";

pub(super) fn parse(args: &[String]) -> std::result::Result<Options, String> {
    let mut options = Options { capture_ms: 30_000 };
    let mut seen = std::collections::BTreeSet::new();
    let mut args = args.iter();
    while let Some(option) = args.next() {
        if option != "--capture-ms" || !seen.insert(option.as_str()) {
            return Err(error_json("unknown or duplicate evidence option"));
        }
        let value = args
            .next()
            .ok_or_else(|| error_json("capture duration is required"))?;
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(error_json(
                "capture duration must be unsigned ASCII decimal",
            ));
        }
        options.capture_ms = value
            .parse()
            .map_err(|_| error_json("capture duration is out of range"))?;
    }
    options.validate().map_err(error_json)?;
    Ok(options)
}

pub(super) fn execute(options: Options) -> std::result::Result<Output, String> {
    let allowlist = super::mx_creative_evidence_allowlist().map_err(protocol_error)?;
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
    options.validate().map_err(error_json)?;
    let request = worker::DiagnosticRequest {
        kind: KIND.into(),
        payload: serde_json::to_value(&options).map_err(|_| protocol_error(worker_error()))?,
    };
    let response = launch(&request, options.budget())
        .map_err(|error| json_string(&json!({"status":"worker_error","error":error})))?;
    if !valid_wire_shape(&response.payload, &options) {
        return Err(json_string(
            &json!({"status":"worker_error","error":worker_error()}),
        ));
    }
    let result: Result =
        serde_json::from_value(response.payload).map_err(|_| protocol_error(worker_error()))?;
    if !valid_result(&result, &options) {
        return Err(json_string(
            &json!({"status":"worker_error","error":worker_error()}),
        ));
    }
    let payload = json_string(&result);
    if result.probe_succeeded {
        Ok(Output::Json(
            serde_json::from_str(&payload).map_err(|_| protocol_error(worker_error()))?,
        ))
    } else {
        Err(payload)
    }
}

pub(super) fn callback(
    payload: Value,
) -> std::result::Result<Value, worker::DiagnosticCallbackError> {
    callback_with(payload, || super::health_callback(Value::Null).map(|_| ()))
}

fn callback_with(
    payload: Value,
    contained: impl FnOnce() -> std::result::Result<(), worker::DiagnosticCallbackError>,
) -> std::result::Result<Value, worker::DiagnosticCallbackError> {
    let object = payload.as_object().ok_or(worker::DiagnosticCallbackError)?;
    if object.len() != 1 || !object.contains_key("capture_ms") {
        return Err(worker::DiagnosticCallbackError);
    }
    let options: Options =
        serde_json::from_value(payload).map_err(|_| worker::DiagnosticCallbackError)?;
    options
        .validate()
        .map_err(|_| worker::DiagnosticCallbackError)?;
    contained()?;
    if !cfg!(feature = "mx-creative-hid") {
        return serde_json::to_value(probe_failure(
            "unavailable: device-specific HID feature is disabled",
        ))
        .map_err(|_| worker::DiagnosticCallbackError);
    }
    let backend =
        crate::native::hid::native_backend().map_err(|_| worker::DiagnosticCallbackError)?;
    let result = probe::run(backend.as_ref(), options.clone());
    if !valid_result(&result, &options) {
        return Err(worker::DiagnosticCallbackError);
    }
    serde_json::to_value(result).map_err(|_| worker::DiagnosticCallbackError)
}

fn probe_failure(error: &str) -> Result {
    Result {
        schema: 1,
        read_only: true,
        error: Some(error.into()),
        ..Result::default()
    }
}

fn valid_wire_shape(payload: &Value, options: &Options) -> bool {
    let Ok(result) = serde_json::from_value::<Result>(payload.clone()) else {
        return false;
    };
    valid_result(&result, options)
}

fn valid_result(result: &Result, options: &Options) -> bool {
    let basic = result.schema == 1
        && result.read_only
        && !result.command_ready
        && !result.hardware_qualified
        && result.probe_succeeded == result.error.is_none()
        && result.error.as_deref().is_none_or(valid_error)
        && result.captures.len() <= 64
        && result.ignored_reports <= 600
        && valid_mismatch(result)
        && result.elapsed_ms <= u64::from(options.capture_ms).saturating_add(100)
        && (!result.capture_truncated || result.captures.len() == 64);
    if !basic {
        return false;
    }
    if result.devices.is_empty() {
        return !result.probe_succeeded && result.captures.is_empty();
    }
    if result.devices.len() != 2 {
        return false;
    }
    let valid_devices = result.devices[0].role == "keypad"
        && result.devices[0].product == "MX Creative Keypad"
        && result.devices[0].vendor_id == 0x046d
        && result.devices[0].product_id == 0xc354
        && result.devices[0].transport == "usb"
        && result.devices[0].primary_usage_page == 0xff43
        && result.devices[0].primary_usage == 0x1a02
        && result.devices[0].max_input_report_len == 32
        && result.devices[0].max_output_report_len == 4095
        && result.devices[0].location_id.as_deref() == Some("0x02110000")
        && result.devices[0].report_descriptor_sha256
            == "d6faec8e21db00518c67b4446875b6bdb9d85988d05d888641da069fb302bc54"
        && result.devices[0].report_ids == [4, 5, 17, 19]
        && result.devices[1].role == "dialpad"
        && result.devices[1].product == "MX Dialpad"
        && result.devices[1].vendor_id == 0x046d
        && result.devices[1].product_id == 0xbc00
        && result.devices[1].transport == "bluetooth"
        && result.devices[1].primary_usage_page == 1
        && result.devices[1].primary_usage == 2
        && result.devices[1].max_input_report_len == 20
        && result.devices[1].max_output_report_len == 20
        && result.devices[1].report_descriptor_sha256
            == "49ea87e24b5049ea9e7e01dd750754aad63f37f51ce8191fee1c1ff7ae5d2cac"
        && result.devices[1].report_ids == [2, 17];
    valid_devices
        && result
            .devices
            .iter()
            .all(|device| valid_registry_path(&device.registry_path))
        && result.captures.iter().all(valid_capture)
        && result
            .captures
            .iter()
            .all(|capture| capture.elapsed_ms <= u64::from(options.capture_ms).saturating_add(100))
        && result
            .captures
            .windows(2)
            .all(|captures| captures[0].elapsed_ms <= captures[1].elapsed_ms)
}

fn valid_mismatch(result: &Result) -> bool {
    match (&result.error, &result.descriptor_mismatch) {
        (Some(error), Some(evidence)) if error.ends_with(" report descriptor mismatch") => {
            let (expected_role, expected_page, expected_usage, expected_length, expected_hash) =
                if evidence.role == "keypad" {
                    (
                        "keypad",
                        0xff43,
                        0x1a02,
                        140,
                        "d6faec8e21db00518c67b4446875b6bdb9d85988d05d888641da069fb302bc54",
                    )
                } else if evidence.role == "dialpad" {
                    (
                        "dialpad",
                        1,
                        2,
                        97,
                        "49ea87e24b5049ea9e7e01dd750754aad63f37f51ce8191fee1c1ff7ae5d2cac",
                    )
                } else {
                    return false;
                };
            error.as_str() == format!("{expected_role} report descriptor mismatch")
                && evidence.selected_primary_usage_page == expected_page
                && evidence.selected_primary_usage == expected_usage
                && matches!(evidence.max_input_report_len, 20 | 32)
                && matches!(evidence.max_output_report_len, 20 | 4095)
                && valid_registry_path(&evidence.selected_path)
                && valid_registry_path(&evidence.source_path)
                && evidence.same_service
                && evidence.selected_path == evidence.source_path
                && matches!(
                    evidence.property_type.as_str(),
                    "cfdata"
                        | "missing"
                        | "cfstring"
                        | "cfnumber"
                        | "other"
                        | "cfdata_out_of_bounds"
                        | "cfdata_null"
                        | "key_creation_failed"
                )
                && evidence.actual_length <= 1024
                && evidence.actual_sha256.len() == 64
                && evidence
                    .actual_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                && evidence.expected_length == expected_length
                && evidence.expected_sha256 == expected_hash
        }
        (_, None) => true,
        _ => false,
    }
}

fn valid_error(error: &str) -> bool {
    const EXACT: &[&str] = &[
        "unavailable: device-specific HID feature is disabled",
        "keypad requires exactly one collection; found 0",
        "keypad requires exactly one collection; found 2",
        "dialpad requires exactly one collection; found 0",
        "dialpad requires exactly one collection; found 2",
        "keypad identity or transport mismatch",
        "dialpad identity or transport mismatch",
        "keypad report descriptor mismatch",
        "dialpad report descriptor mismatch",
        "keypad reopened descriptor mismatch",
        "dialpad reopened descriptor mismatch",
        "keypad product name unavailable",
        "dialpad product name unavailable",
        "HID clock did not advance",
        "HID transport unavailable",
        "Unavailable(FeatureDisabled)",
        "Unavailable(UnsupportedPlatform)",
        "Unavailable(UnsupervisedAccess)",
        "Unavailable(UnsupervisedWrite)",
        "TooManyDevices",
        "NotFound",
        "Timeout",
    ];
    EXACT.contains(&error)
        || matches!(
            error,
            "InvalidArgument(Path)"
                | "InvalidArgument(EmptyReport)"
                | "InvalidArgument(ReportTooLarge)"
                | "InvalidArgument(ReportExceedsDevice)"
                | "InvalidArgument(BufferTooLarge)"
                | "InvalidArgument(BufferEmpty)"
                | "InvalidArgument(TimeoutTooLarge)"
                | "InvalidArgument(LimitInvalid)"
                | "EvidenceUnavailable(InterfaceNumber)"
                | "EvidenceUnavailable(BusClassification)"
                | "EvidenceUnavailable(RawReportDescriptor)"
        )
        || matches!(
            error,
            "Protocol(\"implausible device count\")"
                | "Protocol(\"HID run loop stopped or finished\")"
                | "Protocol(\"unexpected HID run loop result\")"
                | "Protocol(\"read count exceeds buffer\")"
                | "Protocol(\"report descriptor length is outside 1..=1024\")"
                | "Protocol(\"backend returned unmatched descriptor\")"
        )
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
        && code.parse::<i64>().is_ok()
}

fn valid_capture(capture: &probe::Capture) -> bool {
    let expected = match (capture.role.as_str(), capture.report_id) {
        ("keypad", 4) => Some((1, 0x80, 2)),
        ("keypad", 5) => Some((0x0c, 1, 2)),
        ("keypad", 17) => Some((0xff43, 2, 20)),
        ("keypad", 19) => Some((0xff43, 8, 32)),
        ("dialpad", 2) => Some((1, 2, 8)),
        ("dialpad", 17) => Some((0xff43, 2, 20)),
        _ => None,
    };
    let Some((page, usage, bytes)) = expected else {
        return false;
    };
    let decoded = capture
        .hex
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let nibble = |byte: u8| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                _ => None,
            };
            Some((nibble(pair[0])? << 4) | nibble(pair[1])?)
        })
        .collect::<Option<Vec<_>>>();
    capture.usage_page == page
        && capture.usage == usage
        && capture.bytes == bytes
        && capture.values.len() + 1 == bytes
        && capture.hex.len() == bytes * 2
        && decoded.is_some_and(|bytes| {
            bytes.first() == Some(&capture.report_id)
                && bytes
                    .get(1..)
                    .is_some_and(|values| values == capture.values)
        })
}

fn valid_registry_path(path: &str) -> bool {
    path.len() < 80
        && path
            .strip_prefix("iokit:")
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
}

fn worker_error() -> worker::WorkerError {
    worker::WorkerError {
        kind: worker::WorkerErrorKind::Protocol,
        worker: None,
        replacement_allowed: true,
    }
}

fn protocol_error(_: impl std::fmt::Display) -> String {
    json_string(&json!({"status":"worker_error","error":worker_error()}))
}

fn error_json(error: impl std::fmt::Display) -> String {
    json_string(&json!({"status":"probe_error","error":error.to_string()}))
}

fn json_string(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{\"status\":\"serialization_error\"}".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn success() -> Result {
        Result {
            schema: 1,
            probe_succeeded: true,
            read_only: true,
            devices: vec![
                probe::DeviceEvidence {
                    role: "keypad".into(),
                    product: "MX Creative Keypad".into(),
                    vendor_id: 0x046d,
                    product_id: 0xc354,
                    transport: "usb".into(),
                    primary_usage_page: 0xff43,
                    primary_usage: 0x1a02,
                    max_input_report_len: 32,
                    max_output_report_len: 4095,
                    location_id: Some("0x02110000".into()),
                    registry_path: "iokit:1".into(),
                    report_descriptor_sha256:
                        "d6faec8e21db00518c67b4446875b6bdb9d85988d05d888641da069fb302bc54".into(),
                    report_ids: vec![4, 5, 17, 19],
                },
                probe::DeviceEvidence {
                    role: "dialpad".into(),
                    product: "MX Dialpad".into(),
                    vendor_id: 0x046d,
                    product_id: 0xbc00,
                    transport: "bluetooth".into(),
                    primary_usage_page: 1,
                    primary_usage: 2,
                    max_input_report_len: 20,
                    max_output_report_len: 20,
                    location_id: None,
                    registry_path: "iokit:2".into(),
                    report_descriptor_sha256:
                        "49ea87e24b5049ea9e7e01dd750754aad63f37f51ce8191fee1c1ff7ae5d2cac".into(),
                    report_ids: vec![2, 17],
                },
            ],
            ..Result::default()
        }
    }

    fn worker_result(payload: Value) -> worker::DiagnosticResult {
        worker::DiagnosticResult {
            worker: worker::WorkerIdentity {
                pid: 1,
                creation_time_100ns: 2,
            },
            payload,
        }
    }

    #[test]
    fn parser_and_budget_are_finite_and_strict() {
        assert_eq!(parse(&[]).unwrap().capture_ms, 30_000);
        assert_eq!(
            parse(&["--capture-ms".into(), "0".into()])
                .unwrap()
                .budget(),
            Duration::from_secs(5)
        );
        for args in [
            vec!["--capture-ms".into()],
            vec!["--capture-ms".into(), "-1".into()],
            vec!["--capture-ms".into(), "30001".into()],
            vec![
                "--capture-ms".into(),
                "0".into(),
                "--capture-ms".into(),
                "1".into(),
            ],
            vec!["--write".into()],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn valid_success_requires_exact_two_device_evidence() {
        let options = Options { capture_ms: 1_000 };
        assert!(valid_result(&success(), &options));
        let mut missing = success();
        missing.devices.pop();
        assert!(!valid_result(&missing, &options));
        let mut wrong = success();
        wrong.devices[1].product = "private".into();
        assert!(!valid_result(&wrong, &options));
        let mut read_write = success();
        read_write.read_only = false;
        assert!(!valid_result(&read_write, &options));
    }

    #[test]
    fn descriptor_mismatch_evidence_is_bounded_and_requires_same_service() {
        let options = Options { capture_ms: 1_000 };
        let mut result = probe_failure("keypad report descriptor mismatch");
        result.descriptor_mismatch = Some(probe::DescriptorEvidence {
            role: "keypad".into(),
            selected_primary_usage_page: 0xff43,
            selected_primary_usage: 0x1a02,
            max_input_report_len: 32,
            max_output_report_len: 4095,
            location_id: Some(0x0211_0000),
            selected_path: "iokit:1".into(),
            source_path: "iokit:1".into(),
            same_service: true,
            property_type: "cfdata".into(),
            actual_length: 140,
            actual_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .into(),
            expected_length: 140,
            expected_sha256: "d6faec8e21db00518c67b4446875b6bdb9d85988d05d888641da069fb302bc54"
                .into(),
        });
        assert!(valid_result(&result, &options));
        result.descriptor_mismatch.as_mut().unwrap().same_service = false;
        assert!(!valid_result(&result, &options));
        result.descriptor_mismatch.as_mut().unwrap().same_service = true;
        result.descriptor_mismatch.as_mut().unwrap().actual_sha256 = "private".into();
        assert!(!valid_result(&result, &options));
    }

    #[test]
    fn forged_parent_payloads_fail_closed_without_echoing_fields() {
        let options = Options { capture_ms: 1_000 };
        let valid = serde_json::to_value(success()).unwrap();
        let mut cases = vec![
            Value::Null,
            json!({"schema":1}),
            serde_json::to_value(probe_failure("private device path /Users/example")).unwrap(),
            {
                let mut value = valid.clone();
                value["devices"][0]["registry_path"] = json!("private-keyboard");
                value
            },
            {
                let mut value = valid.clone();
                value["captures"] = json!([{"role":"dialpad","report_id":2,"hex":"02"}]);
                value
            },
            {
                let mut value = valid;
                value["command_ready"] = json!(true);
                value
            },
        ];
        for value in cases.drain(..) {
            let error = execute_with(options.clone(), |_, _| Ok(worker_result(value)))
                .err()
                .unwrap();
            let parsed: Value = serde_json::from_str(&error).unwrap();
            assert_eq!(parsed["error"]["kind"], "protocol");
            assert!(!error.contains("private-keyboard"));
        }
    }

    #[test]
    fn callback_rejects_forged_options_before_containment() {
        let mut entered = false;
        let valid = json!({"capture_ms":0});
        assert!(
            callback_with(valid.clone(), || {
                entered = true;
                Err(worker::DiagnosticCallbackError)
            })
            .is_err()
        );
        assert!(entered);
        entered = false;
        assert!(
            callback_with(json!({"capture_ms":0,"write":true}), || {
                entered = true;
                Ok(())
            })
            .is_err()
        );
        assert!(!entered);
    }
}
