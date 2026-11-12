use std::time::Duration;

use serde_json::Value;

use super::enumeration::REPORT_SIZE;
use super::error::MicroError;
use super::json::append_json_plain;
use super::value::{JsonObject, parse_json};

pub const REPORT_ID: u8 = 6;
pub const LOG_CHANNEL: u8 = 1;
pub const JSON_CHANNEL: u8 = 2;
pub const CHUNK_SIZE: usize = 61;
pub const MAX_BUFFERED_BYTES: usize = 1_048_576;
pub const MAX_MESSAGE_AGE: Duration = Duration::from_secs(5);

pub type Report = [u8; REPORT_SIZE];

pub(crate) fn frame_bytes(bytes: &[u8]) -> Vec<Report> {
    bytes
        .chunks(CHUNK_SIZE)
        .map(|chunk| {
            let mut report = [0; REPORT_SIZE];
            report[0] = REPORT_ID;
            report[1] = JSON_CHANNEL;
            report[2] = chunk.len() as u8;
            report[3..3 + chunk.len()].copy_from_slice(chunk);
            report
        })
        .collect()
}

/// Encodes compact JSON; ordinary object keys are sorted.
pub fn encode_json(value: &Value) -> Result<Vec<Report>, MicroError> {
    let mut json = String::new();
    append_json_plain(&mut json, value).map_err(|_| MicroError::Serialization)?;
    Ok(frame_bytes(json.as_bytes()))
}

#[derive(Debug, Default)]
pub struct Decoder {
    buffer: Vec<u8>,
    started_at: Option<Duration>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn buffered_bytes(&self) -> &[u8] {
        &self.buffer
    }

    pub fn started_at(&self) -> Option<Duration> {
        self.started_at
    }

    /// Accepts short low-level frames when their declared payload is present.
    /// The caller must enforce exact report size at native/replay boundaries.
    pub fn feed(&mut self, packet: &[u8], now: Duration) -> Result<Option<JsonObject>, MicroError> {
        if packet.len() < 3
            || packet[0] != REPORT_ID
            || usize::from(packet[2]) > CHUNK_SIZE
            || packet.len() < 3 + usize::from(packet[2])
        {
            return Err(MicroError::InvalidFrame);
        }
        let payload = &packet[3..3 + usize::from(packet[2])];
        match packet[1] {
            LOG_CHANNEL => return Ok(None),
            JSON_CHANNEL => {}
            _ => return Err(MicroError::UnknownChannel),
        }
        if payload.is_empty()
            || (self.buffer.is_empty() && payload.iter().all(|b| matches!(b, b'\r' | b'\n')))
        {
            return Ok(None);
        }
        let started_at = *self.started_at.get_or_insert(now);
        self.buffer.extend_from_slice(payload);
        if self.buffer.len() > MAX_BUFFERED_BYTES
            || now.saturating_sub(started_at) > MAX_MESSAGE_AGE
        {
            return Err(MicroError::AccumulationLimit);
        }
        let value = match parse_json(&self.buffer) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        self.buffer.clear();
        self.started_at = None;
        value
            .into_object()
            .map(Some)
            .map_err(|_| MicroError::NonObjectMessage)
    }
}
