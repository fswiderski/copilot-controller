use std::{sync::atomic::AtomicBool, time::Duration};

use serde_json::Value;

use crate::{devices::mx_creative::display_route::Result, native::windows_worker as worker};

use super::Output;

pub(super) const KIND: &str = "mx_creative_display_route";
const BUDGET: Duration = Duration::from_secs(8);

pub(super) fn parse(args: &[String]) -> std::result::Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err("mx-creative-display-route accepts no options".into())
    }
}

pub(super) fn execute() -> std::result::Result<Output, String> {
    let allowlist =
        super::mx_creative_display_route_allowlist().map_err(|_| worker_error("allowlist"))?;
    execute_with(|request| worker::probe(request, BUDGET, &AtomicBool::new(false), &allowlist))
}

fn execute_with(
    launch: impl FnOnce(
        &worker::DiagnosticRequest,
    ) -> std::result::Result<worker::DiagnosticResult, worker::WorkerError>,
) -> std::result::Result<Output, String> {
    let request = worker::DiagnosticRequest {
        kind: KIND.into(),
        payload: Value::Null,
    };
    let result: Result = serde_json::from_value(
        launch(&request)
            .map_err(|error| {
                serde_json::to_string(&serde_json::json!({
                    "status": "worker_error",
                    "error": error
                }))
                .unwrap_or_else(|_| "worker error".into())
            })?
            .payload,
    )
    .map_err(|_| worker_error(worker::WorkerErrorKind::Protocol))?;
    if !valid_result(&result) {
        return Err(worker_error(worker::WorkerErrorKind::Protocol));
    }
    let payload = serde_json::to_string_pretty(&result).map_err(|_| worker_error(()))?;
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
    let backend = crate::native::hid::mx_creative_display_route_backend()
        .map_err(|_| worker::DiagnosticCallbackError)?;
    let result = crate::devices::mx_creative::display_route::run(&backend);
    if !valid_result(&result) {
        return Err(worker::DiagnosticCallbackError);
    }
    serde_json::to_value(result).map_err(|_| worker::DiagnosticCallbackError)
}

fn failure(error: &str) -> Result {
    Result {
        schema: 1,
        read_only: true,
        no_paint: true,
        elapsed_ms: 0,
        error: Some(error.into()),
        ..Result::default()
    }
}

fn valid_result(result: &Result) -> bool {
    let basics = result.schema == 1
        && result.probe_succeeded == result.error.is_none()
        && result.read_only
        && result.no_paint
        && !result.command_ready
        && !result.hardware_qualified
        && result.hid_output_reports_sent == 0
        && result.image_payloads_sent == 0
        && result.elapsed_ms <= 8_000
        && result.error.as_deref().is_none_or(valid_error);
    if !basics {
        return false;
    }
    if !result.probe_succeeded {
        return result.candidates.is_empty();
    }
    if result.candidates.len() != 1 {
        return false;
    }
    let candidate = &result.candidates[0];
    candidate.vendor_id == 0x046d
        && candidate.product_id == 0xc354
        && candidate.transport == "usb"
        && candidate.primary_usage_page == 0xff43
        && matches!(candidate.primary_usage, 0x1a02 | 0x1a10)
        && candidate.vendor_collection_usages
            == [
                crate::devices::mx_creative::display_route::UsagePair {
                    usage_page: 0xff43,
                    usage: 0x1a02,
                },
                crate::devices::mx_creative::display_route::UsagePair {
                    usage_page: 0xff43,
                    usage: 0x1a08,
                },
                crate::devices::mx_creative::display_route::UsagePair {
                    usage_page: 0xff43,
                    usage: 0x1a10,
                },
            ]
        && candidate.max_input_report_len == 32
        && candidate.max_output_report_len == 4095
        && candidate.report_descriptor_sha256
            == "d6faec8e21db00518c67b4446875b6bdb9d85988d05d888641da069fb302bc54"
        && candidate.route == "single_iokit_device"
        && candidate.display_collection_present
        && candidate.output_report_id == 20
        && candidate.output_report_len == 4095
        && candidate.opened_without_io
        && candidate.closed_after_open
}

fn valid_error(error: &str) -> bool {
    matches!(
        error,
        "unavailable: device-specific HID feature is disabled"
            | "keypad output route identity or transport mismatch"
            | "keypad output route descriptor mismatch"
            | "keypad output route descriptor parse failed"
            | "keypad display output route absent"
            | "keypad reopened route proof mismatch"
    ) || error.starts_with("Os {")
        || error
            .starts_with("keypad output route requires exactly one qualifying collection; found ")
        || error.starts_with("EvidenceUnavailable(")
        || error == "Unavailable(UnsupportedPlatform)"
        || error == "Unavailable(UnsupervisedAccess)"
        || error == "NotFound"
}

fn worker_error(error: impl std::fmt::Debug) -> String {
    format!("output route proof rejected: {error:?}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::mx_creative::display_route::{Candidate, UsagePair};

    #[test]
    fn parser_accepts_only_exact_command() {
        assert!(parse(&[]).is_ok());
        assert!(parse(&["--paint".into()]).is_err());
    }

    #[test]
    fn result_validator_rejects_zero_but_success_io_claims() {
        let result = Result {
            schema: 1,
            probe_succeeded: true,
            read_only: true,
            no_paint: true,
            candidates: vec![Candidate {
                vendor_id: 0x046d,
                product_id: 0xc354,
                transport: "usb".into(),
                primary_usage_page: 0xff43,
                primary_usage: 0x1a02,
                vendor_collection_usages: vec![
                    UsagePair {
                        usage_page: 0xff43,
                        usage: 0x1a02,
                    },
                    UsagePair {
                        usage_page: 0xff43,
                        usage: 0x1a08,
                    },
                    UsagePair {
                        usage_page: 0xff43,
                        usage: 0x1a10,
                    },
                ],
                max_input_report_len: 32,
                max_output_report_len: 4095,
                report_descriptor_sha256:
                    "d6faec8e21db00518c67b4446875b6bdb9d85988d05d888641da069fb302bc54".into(),
                route: "single_iokit_device".into(),
                display_collection_present: true,
                output_report_id: 20,
                output_report_len: 4095,
                opened_without_io: true,
                closed_after_open: true,
            }],
            hid_output_reports_sent: 1,
            image_payloads_sent: 0,
            ..Result::default()
        };
        assert!(!valid_result(&result));
    }

    #[test]
    fn pinned_fake_run_passes_parent_result_validator() {
        let encoded =
            serde_json::to_value(crate::devices::mx_creative::display_route::pinned_fake_result())
                .unwrap();
        let result: Result = serde_json::from_value(encoded).unwrap();
        assert!(valid_result(&result), "{result:?}");
        assert_eq!(result.candidates[0].vendor_collection_usages.len(), 3);
    }
}
