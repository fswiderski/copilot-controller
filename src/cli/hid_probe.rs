//! Explicit HID diagnostics; native access stays inside the contained worker.

mod result;

use std::{sync::atomic::AtomicBool, time::Duration};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    devices::{akp03::probe as akp03, micro::probe as micro},
    native::windows_worker as worker,
};

use super::Output;

pub(super) const MICRO_KIND: &str = "micro_hid_probe";
pub(super) const AKP03_KIND: &str = "akp03_hid_probe";

#[derive(Clone, Debug)]
pub(super) enum Options {
    Micro(micro::MicroProbeOptions),
    Akp03(akp03::Akp03ProbeOptions),
}

impl Options {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Micro(options) => options.validate(),
            Self::Akp03(options) => options.validate(),
        }
        .map_err(option_error)
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Micro(_) => MICRO_KIND,
            Self::Akp03(_) => AKP03_KIND,
        }
    }

    fn health_check(&self) -> bool {
        match self {
            Self::Micro(options) => options.health_check,
            Self::Akp03(options) => options.health_check,
        }
    }

    fn budget(&self) -> Duration {
        if self.health_check() {
            return Duration::from_secs(3);
        }
        Duration::from_millis(match self {
            Self::Micro(options) => {
                u64::from(options.capture_ms) + 20_000 + if options.read_keymap { 5_000 } else { 0 }
            }
            Self::Akp03(options) => u64::from(options.capture_ms) + 15_000,
        })
    }

    fn payload(&self) -> Result<Value, String> {
        match self {
            Self::Micro(options) => serde_json::to_value(options),
            Self::Akp03(options) => serde_json::to_value(options),
        }
        .map_err(|_| option_error("options serialization failed"))
    }

    fn health_outcome(&self) -> Outcome {
        match self {
            Self::Micro(_) => Outcome::Micro(micro::micro_health_check()),
            Self::Akp03(_) => Outcome::Akp03(akp03::akp03_health_check()),
        }
    }
}

enum Outcome {
    Micro(micro::MicroProbeResult),
    Akp03(akp03::Akp03ProbeResult),
}

impl Serialize for Outcome {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Micro(result) => result.serialize(serializer),
            Self::Akp03(result) => result.serialize(serializer),
        }
    }
}

impl Outcome {
    fn valid(&self, options: &Options) -> bool {
        match (self, options) {
            (Self::Micro(result), Options::Micro(options)) => {
                !result.command_ready
                    && !result.hardware_qualified
                    && result.read_only == (result.report.output_attempts == 0)
                    && result.report.captures.len() <= 128
                    && result.report.inputs.len() <= 128
                    && result.probe_succeeded == result.error.is_none()
                    && Self::valid_limit(
                        result.response_limit.as_ref(),
                        result.probe_succeeded,
                        result.report.capture_truncated,
                        result.report.captures.len(),
                        result.report.inputs.len(),
                        true,
                    )
                    && (!options.health_check || *result == micro::micro_health_check())
            }
            (Self::Akp03(result), Options::Akp03(options)) => {
                !result.command_ready
                    && !result.hardware_qualified
                    && result.read_only == !options.initialize
                    && result.report.captures.len() <= 128
                    && result.probe_succeeded == result.error.is_none()
                    && Self::valid_limit(
                        result.response_limit.as_ref(),
                        result.probe_succeeded,
                        result.report.capture_truncated,
                        result.report.captures.len(),
                        0,
                        false,
                    )
                    && (!options.health_check || *result == akp03::akp03_health_check())
            }
            _ => false,
        }
    }

    fn valid_limit(
        limit: Option<&crate::devices::probe_result::ResponseLimit>,
        succeeded: bool,
        truncated: bool,
        captures: usize,
        inputs: usize,
        micro: bool,
    ) -> bool {
        use crate::devices::probe_result::OmittedField;
        limit.is_none_or(|limit| {
            !succeeded
                && limit
                    .captures_dropped
                    .checked_add(captures)
                    .is_some_and(|count| count <= 128)
                && limit
                    .inputs_dropped
                    .checked_add(inputs)
                    .is_some_and(|count| count <= 128)
                && (limit.captures_dropped == 0 || truncated)
                && (micro || limit.inputs_dropped == 0)
                && limit.omitted_fields.len() <= 7
                && limit
                    .omitted_fields
                    .iter()
                    .enumerate()
                    .all(|(index, field)| {
                        !limit.omitted_fields[..index].contains(field)
                            && (micro
                                || matches!(
                                    field,
                                    OmittedField::Candidate
                                        | OmittedField::Phase
                                        | OmittedField::Error
                                ))
                    })
        })
    }

    fn succeeded(&self) -> bool {
        match self {
            Self::Micro(result) => result.probe_succeeded,
            Self::Akp03(result) => result.probe_succeeded,
        }
    }

    fn payload(&self) -> Result<Value, serde_json::Error> {
        match self {
            Self::Micro(result) => serde_json::to_value(result),
            Self::Akp03(result) => serde_json::to_value(result),
        }
    }
}

fn encode(value: &impl Serialize) -> String {
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|_| "HID diagnostic serialization failed".into())
}

fn option_error(error: &str) -> String {
    encode(&json!({"status": "probe_error", "error": error}))
}

fn protocol_error() -> worker::WorkerError {
    worker::WorkerError {
        kind: worker::WorkerErrorKind::Protocol,
        worker: None,
        replacement_allowed: true,
    }
}

fn worker_error(error: worker::WorkerError) -> String {
    encode(&json!({"status": "worker_error", "error": error}))
}

pub(super) fn parse(args: &[String], micro: bool) -> Result<Options, String> {
    let mut options = if micro {
        Options::Micro(micro::MicroProbeOptions {
            capture_ms: 2_000,
            ..Default::default()
        })
    } else {
        Options::Akp03(akp03::Akp03ProbeOptions {
            capture_ms: 2_000,
            ..Default::default()
        })
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut args = args.iter();
    while let Some(option) = args.next() {
        if !seen.insert(option.as_str()) {
            return Err(option_error("duplicate probe option"));
        }
        if option == "--capture-ms" {
            let value = args
                .next()
                .ok_or_else(|| option_error("capture duration is required"))?;
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(option_error(
                    "capture duration must be unsigned ASCII decimal",
                ));
            }
            let capture_ms = value
                .parse()
                .map_err(|_| option_error("capture duration is out of range"))?;
            match &mut options {
                Options::Micro(options) => options.capture_ms = capture_ms,
                Options::Akp03(options) => options.capture_ms = capture_ms,
            }
            continue;
        }
        match (&mut options, option.as_str()) {
            (Options::Micro(options), "--query") => options.query = true,
            (Options::Micro(options), "--read-keymap") => options.read_keymap = true,
            (Options::Micro(options), "--test-pattern") => options.test_pattern = true,
            (Options::Micro(options), "--health-check") => options.health_check = true,
            (Options::Akp03(options), "--initialize") => options.initialize = true,
            (Options::Akp03(options), "--test-pattern") => options.test_pattern = true,
            (Options::Akp03(options), "--health-check") => options.health_check = true,
            _ => return Err(option_error("unknown probe option")),
        }
    }
    options.validate()?;
    Ok(options)
}

pub(super) fn execute(options: Options) -> Result<Output, String> {
    // Health has no native dependency. Where there is no contained worker,
    // return only the pure health result, never an in-process live-I/O fallback.
    #[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
    if options.health_check() {
        options.validate()?;
        return output(options.health_outcome(), &options);
    }
    execute_with(options, |request, budget| {
        let allowlist = match request.kind.as_str() {
            MICRO_KIND => super::micro_probe_allowlist(),
            AKP03_KIND => super::akp03_probe_allowlist(),
            _ => return Err(protocol_error()),
        }
        .map_err(|_| protocol_error())?;
        worker::probe(request, budget, &AtomicBool::new(false), &allowlist)
    })
}

fn execute_with(
    options: Options,
    launch: impl FnOnce(
        &worker::DiagnosticRequest,
        Duration,
    ) -> Result<worker::DiagnosticResult, worker::WorkerError>,
) -> Result<Output, String> {
    options.validate()?;
    let request = worker::DiagnosticRequest {
        kind: options.kind().into(),
        payload: options.payload()?,
    };
    let result = launch(&request, options.budget()).map_err(worker_error)?;
    if !valid_wire_shape(&result.payload, &options) {
        return Err(worker_error(protocol_error()));
    }
    let outcome = match &options {
        Options::Micro(_) => serde_json::from_value(result.payload).map(Outcome::Micro),
        Options::Akp03(_) => serde_json::from_value(result.payload).map(Outcome::Akp03),
    }
    .map_err(|_| worker_error(protocol_error()))?;
    output(outcome, &options)
}

fn valid_wire_shape(payload: &Value, options: &Options) -> bool {
    let nullable_object = |value: &Value| value.is_null() || value.is_object();
    let report = &payload["report"];
    payload.is_object()
        && report.is_object()
        && nullable_object(&payload["response_limit"])
        && nullable_object(&report["candidate"])
        && report["captures"].as_array().is_some_and(|captures| {
            captures.iter().all(|capture| {
                capture.is_object()
                    && (matches!(options, Options::Micro(_))
                        || nullable_object(&capture["decoded"]))
            })
        })
        && (matches!(options, Options::Akp03(_))
            || report["inputs"].as_array().is_some_and(|inputs| {
                inputs.iter().all(|input| {
                    input.is_object()
                        && input["message"].is_object()
                        && nullable_object(&input["joystick"])
                })
            }))
}

fn output(outcome: Outcome, options: &Options) -> Result<Output, String> {
    if !outcome.valid(options) {
        return Err(worker_error(protocol_error()));
    }
    let payload = outcome
        .payload()
        .map_err(|_| worker_error(protocol_error()))?;
    if !outcome.succeeded() {
        return Err(encode(&payload));
    }
    Ok(Output::Json(payload))
}

fn decode_options(payload: Value, micro: bool) -> Result<Options, worker::DiagnosticCallbackError> {
    let fields: &[&str] = if micro {
        &[
            "capture_ms",
            "query",
            "read_keymap",
            "test_pattern",
            "health_check",
        ]
    } else {
        &["capture_ms", "initialize", "test_pattern", "health_check"]
    };
    // Library defaults are programmatic conveniences, not omitted worker fields.
    let object = payload.as_object().ok_or(worker::DiagnosticCallbackError)?;
    if object.len() != fields.len() || !fields.iter().all(|field| object.contains_key(*field)) {
        return Err(worker::DiagnosticCallbackError);
    }

    let options = if micro {
        Options::Micro(
            serde_json::from_value(payload).map_err(|_| worker::DiagnosticCallbackError)?,
        )
    } else {
        Options::Akp03(
            serde_json::from_value(payload).map_err(|_| worker::DiagnosticCallbackError)?,
        )
    };
    options
        .validate()
        .map_err(|_| worker::DiagnosticCallbackError)?;
    Ok(options)
}

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(windows, any(feature = "micro-hid", feature = "akp03-hid"))
))]
pub(crate) fn validate_worker_options(
    payload: Value,
    micro: bool,
) -> Result<Duration, worker::DiagnosticCallbackError> {
    decode_options(payload, micro).map(|options| options.budget())
}

pub(super) fn micro_callback(payload: Value) -> Result<Value, worker::DiagnosticCallbackError> {
    callback(payload, true)
}

pub(super) fn akp03_callback(payload: Value) -> Result<Value, worker::DiagnosticCallbackError> {
    callback(payload, false)
}

fn callback(payload: Value, micro: bool) -> Result<Value, worker::DiagnosticCallbackError> {
    callback_with(
        payload,
        micro,
        || super::health_callback(Value::Null).map(|_| ()),
        native_outcome,
    )
}

fn callback_with(
    payload: Value,
    micro: bool,
    contained: impl FnOnce() -> Result<(), worker::DiagnosticCallbackError>,
    native: impl FnOnce(&Options) -> Outcome,
) -> Result<Value, worker::DiagnosticCallbackError> {
    let options = decode_options(payload, micro)?;
    let outcome = if options.health_check() {
        options.health_outcome()
    } else {
        // Validate containment before even constructing the HID backend.
        contained()?;
        native(&options)
    };
    if !outcome.valid(&options) {
        return Err(worker::DiagnosticCallbackError);
    }
    // Result budgeting is after the entire native run, including LED cleanup.
    result::bounded_payload(outcome).map_err(|_| worker::DiagnosticCallbackError)
}

fn unavailable(options: &Options, error: String) -> Outcome {
    match options {
        Options::Micro(_) => Outcome::Micro(micro::MicroProbeResult {
            read_only: true,
            error: Some(error),
            ..Default::default()
        }),
        Options::Akp03(options) => Outcome::Akp03(akp03::Akp03ProbeResult {
            read_only: !options.initialize,
            error: Some(error),
            ..Default::default()
        }),
    }
}

fn native_outcome(options: &Options) -> Outcome {
    let feature_enabled = match options {
        Options::Micro(_) => cfg!(feature = "micro-hid"),
        Options::Akp03(_) => cfg!(feature = "akp03-hid"),
    };
    if !feature_enabled {
        return unavailable(
            options,
            "unavailable: device-specific HID feature is disabled".into(),
        );
    }
    #[cfg(any(windows, all(target_os = "macos", target_arch = "aarch64")))]
    {
        let backend = match crate::native::hid::native_backend() {
            Ok(backend) => backend,
            Err(error) => return unavailable(options, error.to_string()),
        };
        match options {
            Options::Micro(options) => Outcome::Micro(micro::run_micro_probe_with_backend(
                backend.as_ref(),
                options.clone(),
            )),
            Options::Akp03(options) => {
                Outcome::Akp03(akp03::run_akp03_probe(backend.as_ref(), options.clone()))
            }
        }
    }
    #[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
    unavailable(
        options,
        "unavailable: contained HID worker requires Windows or macOS ARM64".into(),
    )
}

#[cfg(test)]
mod tests;
