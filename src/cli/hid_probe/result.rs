use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;

use crate::{
    devices::probe_result::{OmittedField, ResponseLimit},
    native::windows_worker::MAX_CALLBACK_PAYLOAD_BYTES,
};

use super::{Outcome, akp03, micro};

// Reserve room for omission metadata and its changing counters while rebuilding.
const REBUILD_BUDGET: usize = MAX_CALLBACK_PAYLOAD_BYTES - 1024;
const ERROR_BYTES: usize = 4096;
const LIMIT_ERROR: &str = "HID diagnostic response limit exceeded; see response_limit";

struct Counter {
    remaining: usize,
    exceeded: bool,
}

impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            self.exceeded = true;
            return Err(io::Error::other("diagnostic response limit"));
        }
        self.remaining -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn fits(value: &impl Serialize, budget: usize) -> Result<(), bool> {
    let mut counter = Counter {
        remaining: budget,
        exceeded: false,
    };
    serde_json::to_writer(&mut counter, value).map_err(|_| !counter.exceeded)
}

fn failure(error: &mut Option<String>, limit: &mut ResponseLimit) {
    *error = Some(match error.take() {
        Some(original) if original.len() <= ERROR_BYTES => format!("{original}; {LIMIT_ERROR}"),
        Some(_) => {
            limit.omitted_fields.push(OmittedField::Error);
            LIMIT_ERROR.into()
        }
        None => LIMIT_ERROR.into(),
    });
}

pub(super) fn bounded_payload(mut outcome: Outcome) -> Result<Value, serde_json::Error> {
    let serialization_failed = match fits(&outcome, MAX_CALLBACK_PAYLOAD_BYTES) {
        Ok(()) => match outcome.payload() {
            Ok(payload) => return Ok(payload),
            Err(_) => true,
        },
        Err(serialization_failed) => serialization_failed,
    };
    match &mut outcome {
        Outcome::Micro(result) => bound_micro(result, serialization_failed),
        Outcome::Akp03(result) => bound_akp03(result, serialization_failed),
    }
    // Fixed scalar fallback remains available if conversion itself fails.
    let serialization_failed = match fits(&outcome, MAX_CALLBACK_PAYLOAD_BYTES) {
        Ok(()) => match outcome.payload() {
            Ok(payload) => return Ok(payload),
            Err(_) => true,
        },
        Err(serialization_failed) => serialization_failed,
    };
    match &mut outcome {
        Outcome::Micro(result) => compact_micro(result, serialization_failed),
        Outcome::Akp03(result) => compact_akp03(result, serialization_failed),
    }
    outcome.payload()
}

fn bound_micro(result: &mut micro::MicroProbeResult, serialization_failed: bool) {
    let status = result.report.device_status.take();
    let keymap = result.report.keymap.take();
    let inputs = std::mem::take(&mut result.report.inputs);
    let captures = std::mem::take(&mut result.report.captures);
    let was_truncated = result.report.capture_truncated;
    let mut limit = ResponseLimit {
        captures_dropped: captures.len(),
        inputs_dropped: inputs.len(),
        serialization_failed,
        ..Default::default()
    };
    failure(&mut result.error, &mut limit);
    result.probe_succeeded = false;
    result.report.capture_truncated |= !captures.is_empty();
    result.response_limit = Some(limit);
    if fits(result, REBUILD_BUDGET).is_err() {
        compact_micro(result, serialization_failed);
    }

    // Preserve complete semantic values before spending the remainder on raw captures.
    for (field, value) in [
        (OmittedField::DeviceStatus, status),
        (OmittedField::Keymap, keymap),
    ] {
        let target = match field {
            OmittedField::DeviceStatus => &mut result.report.device_status,
            _ => &mut result.report.keymap,
        };
        *target = value;
        if fits(result, REBUILD_BUDGET).is_err() {
            match field {
                OmittedField::DeviceStatus => result.report.device_status = None,
                _ => result.report.keymap = None,
            }
            result
                .response_limit
                .as_mut()
                .unwrap()
                .omitted_fields
                .push(field);
        }
    }
    for input in inputs {
        result.report.inputs.push(input);
        result.response_limit.as_mut().unwrap().inputs_dropped -= 1;
        if fits(result, REBUILD_BUDGET).is_err() {
            result.report.inputs.pop();
            result.response_limit.as_mut().unwrap().inputs_dropped += 1;
            break;
        }
    }
    for capture in captures {
        result.report.captures.push(capture);
        result.response_limit.as_mut().unwrap().captures_dropped -= 1;
        if fits(result, REBUILD_BUDGET).is_err() {
            result.report.captures.pop();
            result.response_limit.as_mut().unwrap().captures_dropped += 1;
            break;
        }
    }
    result.report.capture_truncated =
        was_truncated || result.response_limit.as_ref().unwrap().captures_dropped != 0;
}

fn bound_akp03(result: &mut akp03::Akp03ProbeResult, serialization_failed: bool) {
    let captures = std::mem::take(&mut result.report.captures);
    let was_truncated = result.report.capture_truncated;
    let mut limit = ResponseLimit {
        captures_dropped: captures.len(),
        serialization_failed,
        ..Default::default()
    };
    failure(&mut result.error, &mut limit);
    result.probe_succeeded = false;
    result.report.capture_truncated |= !captures.is_empty();
    result.response_limit = Some(limit);
    if fits(result, REBUILD_BUDGET).is_err() {
        compact_akp03(result, serialization_failed);
    }
    for capture in captures {
        result.report.captures.push(capture);
        result.response_limit.as_mut().unwrap().captures_dropped -= 1;
        if fits(result, REBUILD_BUDGET).is_err() {
            result.report.captures.pop();
            result.response_limit.as_mut().unwrap().captures_dropped += 1;
            break;
        }
    }
    result.report.capture_truncated =
        was_truncated || result.response_limit.as_ref().unwrap().captures_dropped != 0;
}

fn compact_micro(result: &mut micro::MicroProbeResult, serialization_failed: bool) {
    let limit = result
        .response_limit
        .get_or_insert_with(ResponseLimit::default);
    limit.serialization_failed |= serialization_failed;
    limit.captures_dropped += result.report.captures.len();
    limit.inputs_dropped += result.report.inputs.len();
    result.report.capture_truncated |= !result.report.captures.is_empty();
    result.report.captures.clear();
    result.report.inputs.clear();
    for (field, present) in [
        (
            OmittedField::DeviceStatus,
            result.report.device_status.take().is_some(),
        ),
        (OmittedField::Keymap, result.report.keymap.take().is_some()),
        (
            OmittedField::Firmware,
            result.report.firmware.take().is_some(),
        ),
        (
            OmittedField::Candidate,
            result.report.candidate.take().is_some(),
        ),
        (OmittedField::Phase, !result.report.phase.is_empty()),
    ] {
        if present && !limit.omitted_fields.contains(&field) {
            limit.omitted_fields.push(field);
        }
    }
    if result
        .report
        .cleanup_error
        .as_ref()
        .is_some_and(|error| error.len() > ERROR_BYTES)
    {
        result.report.cleanup_error = Some("Cleanup error text omitted by response limit".into());
        if !limit.omitted_fields.contains(&OmittedField::CleanupError) {
            limit.omitted_fields.push(OmittedField::CleanupError);
        }
    }
    result.report.phase = "response_limit".into();
    result.probe_succeeded = false;
    compact_error(&mut result.error, limit);
}

fn compact_akp03(result: &mut akp03::Akp03ProbeResult, serialization_failed: bool) {
    let limit = result
        .response_limit
        .get_or_insert_with(ResponseLimit::default);
    limit.serialization_failed |= serialization_failed;
    limit.captures_dropped += result.report.captures.len();
    result.report.capture_truncated |= !result.report.captures.is_empty();
    result.report.captures.clear();
    for (field, present) in [
        (
            OmittedField::Candidate,
            result.report.candidate.take().is_some(),
        ),
        (OmittedField::Phase, !result.report.phase.is_empty()),
    ] {
        if present && !limit.omitted_fields.contains(&field) {
            limit.omitted_fields.push(field);
        }
    }
    result.report.phase = "response_limit".into();
    result.probe_succeeded = false;
    compact_error(&mut result.error, limit);
}

fn compact_error(error: &mut Option<String>, limit: &mut ResponseLimit) {
    if error
        .as_ref()
        .is_some_and(|error| error.len() > ERROR_BYTES + LIMIT_ERROR.len() + 2)
    {
        if !limit.omitted_fields.contains(&OmittedField::Error) {
            limit.omitted_fields.push(OmittedField::Error);
        }
        *error = Some(LIMIT_ERROR.into());
    } else if error.is_none() {
        *error = Some(LIMIT_ERROR.into());
    }
}
