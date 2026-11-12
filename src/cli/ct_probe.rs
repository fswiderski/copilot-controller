//! The only CT CLI authority is this static, contained diagnostic route.

use std::{sync::atomic::AtomicBool, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    devices::ct::{
        probe::{self, Evidence, Options, Phase, ProbeFailure, ProbeProgress, ProbeReport},
        protocol::decode_input,
    },
    native::windows_worker as worker,
};

use super::Output;

pub(super) const KIND: &str = "ct_serial_probe";

#[derive(Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Outcome {
    Report { report: Box<ProbeReport> },
    ProbeError { error: ProbeFailure },
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Failure {
    ProbeError { error: OptionFailure },
    WorkerError { error: worker::WorkerError },
}

#[derive(Serialize)]
struct OptionFailure {
    kind: probe::ProbeErrorKind,
    stage: probe::Stage,
    native_code: Option<u32>,
}

fn invalid_options() -> String {
    encode_error(&Failure::ProbeError {
        error: OptionFailure {
            kind: probe::ProbeErrorKind::InvalidOptions,
            stage: probe::Stage::Options,
            native_code: None,
        },
    })
}

fn encode_error(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "CT diagnostic serialization failed".into())
}

pub(super) fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut seen = std::collections::BTreeSet::new();
    let mut args = args.iter();
    while let Some(option) = args.next() {
        if !seen.insert(option.as_str()) {
            return Err(invalid_options());
        }
        if option == "--test-pattern" {
            options.test_pattern = true;
            continue;
        }
        if !matches!(
            option.as_str(),
            "--schema-version"
                | "--total-timeout-ms"
                | "--io-timeout-ms"
                | "--capture-ms"
                | "--max-events"
                | "--max-received-bytes"
        ) {
            return Err(invalid_options());
        }
        let value = args.next().ok_or_else(invalid_options)?;
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid_options());
        }
        let number: u32 = value.parse().map_err(|_| invalid_options())?;
        match option.as_str() {
            "--schema-version" => {
                options.schema_version = number.try_into().map_err(|_| invalid_options())?;
            }
            "--total-timeout-ms" => options.total_timeout_ms = number,
            "--io-timeout-ms" => options.io_timeout_ms = number,
            "--capture-ms" => options.capture_ms = number,
            "--max-events" => {
                options.max_events = number.try_into().map_err(|_| invalid_options())?;
            }
            "--max-received-bytes" => options.max_received_bytes = number,
            _ => unreachable!("option was validated"),
        }
    }
    options.validate().map_err(|_| invalid_options())?;
    Ok(options)
}

fn supervision_budget(options: &Options) -> Duration {
    // DISPLAY_DEVICES §6: session + cancelled operation's 500ms terminal grace,
    // separate <=250ms close + its 500ms grace. Allow a further bounded second
    // for startup, serialization, EOF and process exit, not another I/O budget.
    Duration::from_millis(
        u64::from(options.total_timeout_ms)
            + 500
            + u64::from(options.io_timeout_ms.min(250))
            + 500
            + 1_000,
    )
}

pub(super) fn execute(options: Options) -> Result<Output, String> {
    execute_with(options, |request, budget| {
        let allowlist = super::diagnostic_allowlist().map_err(|_| protocol_error())?;
        worker::probe(request, budget, &AtomicBool::new(false), &allowlist)
    })
}

fn protocol_error() -> worker::WorkerError {
    worker::WorkerError {
        kind: worker::WorkerErrorKind::Protocol,
        worker: None,
        replacement_allowed: true,
    }
}

fn execute_with(
    options: Options,
    launch: impl FnOnce(
        &worker::DiagnosticRequest,
        Duration,
    ) -> Result<worker::DiagnosticResult, worker::WorkerError>,
) -> Result<Output, String> {
    options.validate().map_err(|_| invalid_options())?;
    let request = worker::DiagnosticRequest {
        kind: KIND.into(),
        payload: serde_json::to_value(&options).map_err(|_| invalid_options())?,
    };
    let result = launch(&request, supervision_budget(&options))
        .map_err(|error| encode_error(&Failure::WorkerError { error }))?;
    let outcome: Outcome = serde_json::from_value(result.payload).map_err(|_| {
        encode_error(&Failure::WorkerError {
            error: protocol_error(),
        })
    })?;
    if !valid_outcome(&outcome, &options) {
        return Err(encode_error(&Failure::WorkerError {
            error: protocol_error(),
        }));
    }
    match outcome {
        Outcome::Report { report } => Ok(Output::Json(json!({
            "schema": 1,
            "command_ready": false,
            "worker": result.worker,
            "ct_probe": report,
        }))),
        failure @ Outcome::ProbeError { .. } => Err(encode_error(&failure)),
    }
}

pub(super) fn callback(payload: Value) -> Result<Value, worker::DiagnosticCallbackError> {
    callback_with(
        payload,
        || super::health_callback(Value::Null).map(|_| ()),
        probe::run_native,
    )
}

fn callback_with(
    payload: Value,
    contained: impl FnOnce() -> Result<(), worker::DiagnosticCallbackError>,
    native: impl FnOnce(Options, &AtomicBool) -> Result<ProbeReport, ProbeFailure>,
) -> Result<Value, worker::DiagnosticCallbackError> {
    if !payload.is_object() {
        return Err(worker::DiagnosticCallbackError);
    }
    let options: Options =
        serde_json::from_value(payload).map_err(|_| worker::DiagnosticCallbackError)?;
    options
        .validate()
        .map_err(|_| worker::DiagnosticCallbackError)?;
    // A hand-invoked hidden worker must not bypass detached, one-process,
    // kill-on-close Job containment by supplying a well-formed CT request.
    contained()?;
    // Native transport, overlapped buffers, serial handle and mutex never leave
    // the one-shot worker. A typed probe failure travels as data, not CLI success.
    let outcome = match native(options.clone(), &AtomicBool::new(false)) {
        Ok(report) => Outcome::Report {
            report: Box::new(report),
        },
        Err(error) => Outcome::ProbeError { error },
    };
    if !valid_outcome(&outcome, &options) {
        return Err(worker::DiagnosticCallbackError);
    }
    serde_json::to_value(outcome).map_err(|_| worker::DiagnosticCallbackError)
}

fn phase_index(phase: Phase) -> usize {
    match phase {
        Phase::Preparing => 0,
        Phase::Discovery => 1,
        Phase::Open => 2,
        Phase::AssociationCheck => 3,
        Phase::Configure => 4,
        Phase::UpgradeWrite => 5,
        Phase::UpgradeResponse => 6,
        Phase::HeaderValidation => 7,
        Phase::IdentityWrite => 8,
        Phase::IdentityResponse => 9,
        Phase::FirmwareWrite => 10,
        Phase::FirmwareResponse => 11,
        Phase::Pattern => 12,
        Phase::Capture => 13,
    }
}

fn valid_progress(progress: &ProbeProgress, options: &Options) -> bool {
    let limit = supervision_budget(options).as_millis() as u32;
    progress.evidence == Evidence::NativeTransportObservation
        && progress.elapsed_ms <= limit
        && progress.received_bytes <= options.max_received_bytes
        && progress.phases.len() <= 14
        && progress
            .phases
            .first()
            .is_some_and(|entry| entry.phase == Phase::Preparing && entry.started_ms == 0)
        && progress
            .phases
            .last()
            .is_some_and(|entry| entry.phase == progress.last_phase)
        && progress
            .phases
            .iter()
            .all(|entry| entry.started_ms.saturating_add(entry.elapsed_ms) <= progress.elapsed_ms)
        && progress.phases.windows(2).all(|pair| {
            phase_index(pair[0].phase) < phase_index(pair[1].phase)
                && pair[0].started_ms <= pair[1].started_ms
        })
        && [progress.port_opened_at_ms, progress.port_configured_at_ms]
            .into_iter()
            .flatten()
            .all(|time| time <= progress.elapsed_ms)
        && [
            (&progress.upgrade_request, probe::UPGRADE.len() as u32),
            (&progress.identity_request, 9),
            (&progress.firmware_request, 9),
            (&progress.close_request, 6),
        ]
        .iter()
        .all(|(request, total)| request.total_bytes == *total && request.written_bytes <= *total)
        && progress
            .handshake_response_start
            .as_deref()
            .is_none_or(|class| {
                matches!(
                    class,
                    "text" | "web_socket_binary_frame" | "web_socket_close_frame" | "other_binary"
                )
            })
}

fn valid_outcome(outcome: &Outcome, options: &Options) -> bool {
    match outcome {
        Outcome::ProbeError { error } => valid_progress(&error.progress, options),
        Outcome::Report { report } => {
            report.schema_version == 1
                && report.evidence == Evidence::NativeTransportObservation
                && report.hardware_qualification == "unqualified"
                && report.physical_input_qualification == "unqualified"
                && report.visual_acceptance == "unqualified"
                && report.http_101
                && report.progress.header_complete
                && report.progress.header_validated
                && report.progress.last_phase == Phase::Capture
                && (1..=64).contains(&report.identity_response_length)
                && report.pattern
                    == if options.test_pattern {
                        "correlated_replies_only"
                    } else {
                        "not_requested"
                    }
                && report.elapsed_ms <= supervision_budget(options).as_millis() as u32
                && report.received_bytes <= options.max_received_bytes
                && report.received_bytes == report.progress.received_bytes
                && report.close == report.progress.session_close
                && report.inputs.len() <= usize::from(options.max_events)
                && report.inputs.iter().all(|sample| {
                    sample.elapsed_ms <= report.elapsed_ms && decode_input(&sample.payload).is_ok()
                })
                && report
                    .inputs
                    .windows(2)
                    .all(|pair| pair[0].elapsed_ms <= pair[1].elapsed_ms)
                && report.serial_settings.as_ref().is_some_and(|settings| {
                    settings.baud_rate == 256_000
                        && settings.data_bits == 8
                        && settings.parity == "none"
                        && settings.stop_bits == 1
                        && settings.binary
                        && settings.flow_control == "none"
                        && !settings.nul_stripping
                })
                && valid_progress(&report.progress, options)
        }
    }
}

#[cfg(test)]
mod tests;
