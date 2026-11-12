use std::collections::VecDeque;
use std::time::Duration;

use serde_json::{Map, Value};

use super::enumeration::{validate_report, validate_write};
use super::error::MicroError;
use super::json::{
    append_json, append_json_string, numeric_equals_u64, python_truthy, python_whitespace,
};
use super::led::{LedEntry, serialize_entries};
use super::value::{JsonObject, JsonValue};
use super::wire::{Decoder, Report, frame_bytes};

pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(4);
pub const INPUT_QUEUE_CAPACITY: usize = 128;
pub const FIRST_REQUEST_ID: u64 = 101;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRequest {
    pub id: u64,
    pub reports: Vec<Report>,
}

#[derive(Debug)]
struct Pending {
    id: u64,
    method: String,
    report_count: usize,
    written: usize,
    deadline: Option<Duration>,
}

/// State machine only: report writes and reads are always performed by the caller.
/// On a reconnect/stop disposition, abandon queued input and call
/// [`Self::discard_connection`] before starting a new connection.
#[derive(Debug)]
pub struct RpcClient {
    next_id: Option<u64>,
    pending: Option<Pending>,
    decoder: Decoder,
    inputs: VecDeque<JsonObject>,
}

impl Default for RpcClient {
    fn default() -> Self {
        Self::new()
    }
}

impl RpcClient {
    pub fn new() -> Self {
        Self {
            next_id: Some(FIRST_REQUEST_ID),
            pending: None,
            decoder: Decoder::new(),
            inputs: VecDeque::new(),
        }
    }

    fn admit(&self, method: &str, params: &Value) -> Result<(), MicroError> {
        if !matches!(
            method,
            "sys.version" | "device.status" | "fs.read" | "v.oai.thstatus"
        ) {
            return Err(MicroError::MethodNotAllowlisted);
        }
        if method == "fs.read" {
            let valid = params.as_object().is_some_and(|object| {
                object.len() == 1
                    && object.get("file").and_then(Value::as_str) == Some("keymap.json")
            });
            if !valid {
                return Err(MicroError::RestrictedFilesystemRead);
            }
        }
        Ok(())
    }

    fn prepare_serialized(
        &mut self,
        method: &str,
        params: &str,
    ) -> Result<PreparedRequest, MicroError> {
        if self.pending.is_some() {
            return Err(MicroError::RpcBusy);
        }
        let id = self.next_id.ok_or(MicroError::RequestIdExhausted)?;
        let mut json = String::from("{\"method\":");
        append_json_string(&mut json, method).map_err(|_| MicroError::Serialization)?;
        json.push_str(",\"params\":");
        json.push_str(params);
        json.push_str(",\"id\":");
        json.push_str(&id.to_string());
        json.push('}');
        let reports = frame_bytes(json.as_bytes());
        self.next_id = id.checked_add(1);
        self.pending = Some(Pending {
            id,
            method: method.to_owned(),
            report_count: reports.len(),
            written: 0,
            deadline: None,
        });
        Ok(PreparedRequest { id, reports })
    }

    pub fn begin(&mut self, method: &str, params: &Value) -> Result<PreparedRequest, MicroError> {
        self.admit(method, params)?;
        let mut json = String::new();
        if method == "v.oai.thstatus" {
            append_led_params(&mut json, params).map_err(|_| MicroError::Serialization)?;
        } else {
            append_json(&mut json, params).map_err(|_| MicroError::Serialization)?;
        }
        self.prepare_serialized(method, &json)
    }

    /// Uses the required `id,c,b,e,s` ordering for typed LED entries.
    pub fn begin_led(&mut self, entries: &[LedEntry; 6]) -> Result<PreparedRequest, MicroError> {
        let params = serialize_entries(entries)?;
        self.prepare_serialized("v.oai.thstatus", &params)
    }

    /// Confirms each report in order. Only the final confirmation starts the deadline.
    pub fn confirm_write(
        &mut self,
        count: usize,
        now: Duration,
    ) -> Result<Option<Duration>, MicroError> {
        let pending = self.pending.as_mut().ok_or(MicroError::NoPendingRequest)?;
        if pending.written == pending.report_count {
            return Err(MicroError::RpcBusy);
        }
        validate_write(count)?;
        if pending.written + 1 == pending.report_count {
            let deadline = now
                .checked_add(RESPONSE_TIMEOUT)
                .ok_or(MicroError::ClockOutOfRange)?;
            pending.written += 1;
            pending.deadline = Some(deadline);
            Ok(Some(deadline))
        } else {
            pending.written += 1;
            Ok(None)
        }
    }

    /// Call immediately before each read. A read already started may finish at or
    /// after the deadline; feeding its response does not check the clock again.
    pub fn check_read_deadline(&self, now: Duration) -> Result<(), MicroError> {
        let pending = self.pending.as_ref().ok_or(MicroError::NoPendingRequest)?;
        let deadline = pending.deadline.ok_or(MicroError::WritesIncomplete)?;
        if now < deadline {
            Ok(())
        } else {
            Err(MicroError::ResponseTimeout {
                method: pending.method.clone(),
            })
        }
    }

    pub fn feed_report(
        &mut self,
        report: &[u8],
        now: Duration,
    ) -> Result<Option<JsonValue>, MicroError> {
        let pending = self.pending.as_ref().ok_or(MicroError::NoPendingRequest)?;
        if pending.deadline.is_none() {
            return Err(MicroError::WritesIncomplete);
        }
        if report.is_empty() {
            return Ok(None);
        }
        validate_report(report)?;
        match self.decoder.feed(report, now)? {
            Some(object) => self.feed_object(object),
            None => Ok(None),
        }
    }

    pub fn feed_message(
        &mut self,
        object: Map<String, Value>,
    ) -> Result<Option<JsonValue>, MicroError> {
        self.feed_object(JsonObject::from_map(object))
    }

    /// Parses an unsolicited report while no RPC is pending and queues only input
    /// notifications. This is used by fixed-interval probe drains.
    pub fn feed_observation_report(
        &mut self,
        report: &[u8],
        now: Duration,
    ) -> Result<(), MicroError> {
        let Some(object) = self.decoder.feed(report, now)? else {
            return Ok(());
        };
        let object = object.as_map();
        if matches!(
            object.get("m").and_then(Value::as_str),
            Some("v.oai.hid" | "v.oai.rad" | "kb.radial")
        ) {
            if self.inputs.len() == INPUT_QUEUE_CAPACITY {
                return Err(MicroError::InputOverflow);
            }
            self.inputs.push_back(JsonObject::from_map(object.clone()));
        }
        Ok(())
    }

    fn feed_object(&mut self, mut object: JsonObject) -> Result<Option<JsonValue>, MicroError> {
        let pending = self.pending.as_ref().ok_or(MicroError::NoPendingRequest)?;
        if pending.deadline.is_none() {
            return Err(MicroError::WritesIncomplete);
        }
        let matches = object
            .get("id")
            .is_some_and(|id| numeric_equals_u64(id, pending.id));
        if matches && (object.contains_key("result") || object.contains_key("error")) {
            let pending = self.pending.take().expect("pending request was checked");
            if let Some(error) = object.remove("error") {
                return Err(MicroError::DeviceRejected {
                    method: pending.method,
                    error,
                });
            }
            return Ok(object.remove("result"));
        }
        if matches!(
            object.get("m").and_then(Value::as_str),
            Some("v.oai.hid" | "v.oai.rad" | "kb.radial")
        ) {
            if self.inputs.len() == INPUT_QUEUE_CAPACITY {
                return Err(MicroError::InputOverflow);
            }
            self.inputs.push_back(object);
        }
        Ok(None)
    }

    pub fn pop_input(&mut self) -> Option<JsonObject> {
        self.inputs.pop_front()
    }

    pub fn queued_input_count(&self) -> usize {
        self.inputs.len()
    }

    pub fn next_request_id(&self) -> Option<u64> {
        self.next_id
    }

    /// Abandons only the current RPC exchange while retaining framing, input,
    /// and request-sequence state for a bounded diagnostic cleanup attempt.
    pub fn abandon_pending(&mut self) {
        self.pending = None;
    }

    pub fn read_time_remaining(&self, now: Duration) -> Result<Duration, MicroError> {
        let pending = self.pending.as_ref().ok_or(MicroError::NoPendingRequest)?;
        let deadline = pending.deadline.ok_or(MicroError::WritesIncomplete)?;
        if now >= deadline {
            return Err(MicroError::ResponseTimeout {
                method: pending.method.clone(),
            });
        }
        Ok(deadline - now)
    }

    pub fn decoder(&self) -> &Decoder {
        &self.decoder
    }

    /// Discards the pending call, input queue, and decoder on a connection cut.
    pub fn discard_connection(&mut self) {
        *self = Self::new();
    }
}

fn append_led_params(output: &mut String, params: &Value) -> Result<(), ()> {
    let Some(entries) = params.as_array() else {
        return append_json(output, params);
    };
    const FIELDS: [&str; 5] = ["id", "c", "b", "e", "s"];
    output.push('[');
    for (index, entry) in entries.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        let Some(object) = entry
            .as_object()
            .filter(|object| FIELDS.iter().all(|field| object.contains_key(*field)))
        else {
            append_json(output, entry)?;
            continue;
        };
        output.push('{');
        for (index, key) in FIELDS.into_iter().enumerate() {
            if index != 0 {
                output.push(',');
            }
            append_json_string(output, key)?;
            output.push(':');
            append_json(output, &object[key])?;
        }
        let mut extra: Vec<_> = object
            .iter()
            .filter(|(key, _)| !FIELDS.contains(&key.as_str()))
            .collect();
        extra.sort_unstable_by_key(|(key, _)| *key);
        for (key, value) in extra {
            output.push(',');
            append_json_string(output, key)?;
            output.push(':');
            append_json(output, value)?;
        }
        output.push('}');
    }
    output.push(']');
    Ok(())
}

fn parse_component(component: &str) -> Result<i64, MicroError> {
    let invalid = || MicroError::InvalidFirmwareVersion {
        component: component.to_owned(),
    };
    let text = component.trim_matches(python_whitespace);
    let mut bytes = text.as_bytes();
    let negative = match bytes.first() {
        Some(b'+') => {
            bytes = &bytes[1..];
            false
        }
        Some(b'-') => {
            bytes = &bytes[1..];
            true
        }
        _ => false,
    };
    if bytes.is_empty() {
        return Err(invalid());
    }
    let mut magnitude = 0u64;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte.is_ascii_digit() {
            magnitude = magnitude
                .saturating_mul(10)
                .saturating_add(u64::from(byte - b'0'));
        } else if byte != b'_'
            || index == 0
            || index + 1 == bytes.len()
            || !bytes[index - 1].is_ascii_digit()
            || !bytes[index + 1].is_ascii_digit()
        {
            return Err(invalid());
        }
    }
    if negative {
        if magnitude > i64::MAX as u64 {
            Ok(i64::MIN)
        } else {
            Ok(-(magnitude as i64))
        }
    } else {
        Ok(magnitude.min(i64::MAX as u64) as i64)
    }
}

/// Checks the ordinary RPC firmware gate, returning the original spelling.
pub fn validate_firmware_version(result: &Value) -> Result<String, MicroError> {
    let object = result
        .as_object()
        .ok_or(MicroError::MalformedFirmwareResult)?;
    let version = object.get("version").unwrap_or(&Value::Null);
    if !python_truthy(version) {
        return Err(MicroError::UnsupportedFirmware {
            version: version.clone(),
        });
    }
    let original = version
        .as_str()
        .ok_or(MicroError::MalformedFirmwareResult)?;
    let stripped = original.trim_start_matches('v');
    let mut components = stripped.split('.');
    let major = parse_component(components.next().unwrap_or_default())?;
    let supported = if let Some(minor) = components.next() {
        (major, parse_component(minor)?) >= (0, 6)
    } else {
        major > 0
    };
    if supported {
        Ok(original.to_owned())
    } else {
        Err(MicroError::UnsupportedFirmware {
            version: version.clone(),
        })
    }
}
