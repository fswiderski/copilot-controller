use std::{sync::atomic::AtomicBool, time::Duration};

use serde_json::{Value, json};

use crate::{
    devices::mx_creative::firmware_info::{
        FeatureInfo, FeatureStatus, FirmwareInfo, MAX_FIRMWARES, Options, Result,
    },
    native::windows_worker as worker,
};

use super::Output;

pub(super) const KIND: &str = "mx_creative_firmware_info";

pub(super) fn parse(args: &[String]) -> std::result::Result<Options, String> {
    if args.is_empty() {
        Ok(Options::default())
    } else {
        Err("mx-creative-firmware-info accepts no options".into())
    }
}

pub(super) fn execute(options: Options) -> std::result::Result<Output, String> {
    let allowlist =
        super::mx_creative_firmware_info_allowlist().map_err(|_| worker_error("allowlist"))?;
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
        payload: serde_json::to_value(options).map_err(|_| worker_error(()))?,
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
    let backend = crate::native::hid::mx_creative_firmware_info_backend()
        .map_err(|_| worker::DiagnosticCallbackError)?;
    let result = crate::devices::mx_creative::firmware_info::run(&backend, options);
    if !valid_result(&result, &Options::default()) {
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

pub(super) fn valid_result(result: &Result, _options: &Options) -> bool {
    let basics = result.schema == 1
        && result.semantic_read_only
        && !result.command_ready
        && !result.hardware_qualified
        && result.probe_succeeded == result.error.is_none()
        && result.requests_sent <= 10
        && result.elapsed_ms <= 22_000
        && result.firmwares.len() <= usize::from(MAX_FIRMWARES)
        && result
            .firmware_count
            .is_none_or(|count| count <= MAX_FIRMWARES)
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
            || result.feature.is_some()
            || result.firmware_count.is_some()
            || !result.firmwares.is_empty())
    {
        return false;
    }
    if result
        .feature
        .as_ref()
        .is_some_and(|feature| !valid_feature(feature))
    {
        return false;
    }
    if result
        .firmwares
        .iter()
        .enumerate()
        .any(|(position, firmware)| !valid_firmware(firmware, position as u8))
    {
        return false;
    }
    if result.probe_succeeded {
        return match result.feature.as_ref() {
            Some(feature) if feature.status == FeatureStatus::Present => {
                result.firmware_count.is_some_and(|count| {
                    result.firmwares.len() == usize::from(count)
                        && result.requests_sent == 2 + count
                })
            }
            Some(feature) => {
                matches!(
                    feature.status,
                    FeatureStatus::Absent | FeatureStatus::Unsupported
                ) && result.firmware_count.is_none()
                    && result.firmwares.is_empty()
                    && result.requests_sent == 1
            }
            None => false,
        };
    }
    if result.device.is_none() {
        return result.feature.is_none()
            && result.firmware_count.is_none()
            && result.firmwares.is_empty()
            && result.requests_sent == 0;
    }
    match result.feature.as_ref() {
        None => {
            result.requests_sent == 1
                && result.firmware_count.is_none()
                && result.firmwares.is_empty()
        }
        Some(FeatureInfo {
            status: FeatureStatus::QueryError,
            ..
        }) => {
            result.requests_sent == 1
                && result.firmware_count.is_none()
                && result.firmwares.is_empty()
        }
        Some(FeatureInfo {
            status: FeatureStatus::Present,
            ..
        }) => match result.firmware_count {
            None => result.requests_sent == 2 && result.firmwares.is_empty(),
            Some(count) => {
                let entries = result.firmwares.len();
                entries < usize::from(count)
                    && (result.requests_sent == 2 + entries as u8
                        || result.requests_sent == 3 + entries as u8)
                    && result.requests_sent <= 2 + count
            }
        },
        Some(_) => false,
    }
}

fn valid_feature(feature: &FeatureInfo) -> bool {
    feature.id == 0x0003
        && match feature.status {
            FeatureStatus::Present => {
                feature.feature_index.is_some_and(|index| index != 0)
                    && feature.hidpp_error.is_none()
            }
            FeatureStatus::Absent => {
                feature.feature_index.is_none()
                    && feature.hidpp_error.is_none_or(|code| matches!(code, 1 | 6))
            }
            FeatureStatus::Unsupported => {
                feature.feature_index.is_none() && feature.hidpp_error == Some(9)
            }
            FeatureStatus::QueryError => {
                feature.feature_index.is_none() && feature.hidpp_error.is_some()
            }
        }
}

fn valid_firmware(firmware: &FirmwareInfo, position: u8) -> bool {
    firmware.index == position
        && firmware
            .name
            .as_deref()
            .is_none_or(|name| (1..=3).contains(&name.len()) && valid_printable(name))
        && firmware.name_hex.len() == 6
        && firmware
            .name_hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && firmware.name.as_ref().is_none_or(|name| {
            let mut expected = name
                .bytes()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            expected.push_str(&"00".repeat(3 - name.len()));
            firmware.name_hex == expected
        })
        && valid_entity_name(firmware.entity_type, &firmware.entity_name)
        && valid_decimal(firmware.version.as_str(), 5, 2)
        && valid_decimal(firmware.build.as_str(), 4, 4)
}

fn valid_printable(value: &str) -> bool {
    value.bytes().all(|byte| (b' '..=b'~').contains(&byte))
}

fn valid_entity_name(entity_type: u8, name: &str) -> bool {
    match entity_type {
        0 => name == "main",
        1 => name == "bootloader",
        2 => name == "hardware",
        _ => name == "other",
    }
}

fn valid_decimal(value: &str, length: usize, digits: usize) -> bool {
    if value.len() != length {
        return false;
    }
    if digits == 2 {
        value
            .split_once('.')
            .is_some_and(|(left, right)| valid_digits(left, 2) && valid_digits(right, 2))
    } else {
        valid_digits(value, digits)
    }
}

fn valid_digits(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_digit())
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
            | "firmware feature discovery failed"
            | "unexpected firmware feature discovery response"
            | "firmware count exceeds bounded maximum"
            | "firmware count query rejected"
            | "unexpected firmware count response"
            | "firmware metadata query rejected"
            | "unexpected firmware metadata response"
            | "invalid firmware name encoding"
            | "invalid firmware major version"
            | "invalid firmware revision"
            | "invalid firmware build"
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
    "{\"status\":\"worker_error\",\"error\":\"invalid MX firmware-info result\"}\n".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_accepts_only_empty_options() {
        assert_eq!(parse(&[]), Ok(Options::default()));
        assert!(parse(&["--query".into()]).is_err());
    }

    #[test]
    fn parent_rejects_unbounded_or_malformed_firmware_data() {
        let result = Result {
            schema: 1,
            semantic_read_only: true,
            probe_succeeded: true,
            device: Some(crate::devices::mx_creative::firmware_info::DeviceInfo {
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
            feature: Some(FeatureInfo {
                id: 0x0003,
                status: FeatureStatus::Present,
                feature_index: Some(4),
                hidpp_error: None,
            }),
            firmware_count: Some(1),
            firmwares: vec![FirmwareInfo {
                index: 0,
                entity_type: 0,
                entity_name: "main".into(),
                name: Some("APP".into()),
                name_hex: "415050".into(),
                version: "01.23".into(),
                build: "0045".into(),
                active: true,
            }],
            requests_sent: 3,
            elapsed_ms: 10,
            ..Result::default()
        };
        assert!(valid_result(&result, &Options::default()));
        let mut invalid = result;
        invalid.firmware_count = Some(MAX_FIRMWARES + 1);
        assert!(!valid_result(&invalid, &Options::default()));
    }
}
