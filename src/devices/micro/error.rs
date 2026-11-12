use std::fmt;

use serde_json::Value;

use super::json::{repr_string, str_value};
use super::value::JsonValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    ReconnectAfterThreeSeconds,
    StopWithCleanup,
    CallerMisuse,
}

#[derive(Debug, PartialEq)]
pub enum MicroError {
    InvalidFrame,
    UnknownChannel,
    AccumulationLimit,
    NonObjectMessage,
    MethodNotAllowlisted,
    RestrictedFilesystemRead,
    DeviceRejected { method: String, error: JsonValue },
    ResponseTimeout { method: String },
    InputOverflow,
    UnsupportedFirmware { version: Value },
    InvalidFirmwareVersion { component: String },
    MalformedFirmwareResult,
    ReportSize,
    ShortWrite { count: usize },
    CapabilityFailure,
    CapabilityLayout,
    InterfaceCount { count: usize },
    CompetingOwners { names: Vec<String> },
    InvalidTelemetry,
    MalformedEvent,
    Serialization,
    RpcBusy,
    NoPendingRequest,
    WritesIncomplete,
    RequestIdExhausted,
    ClockOutOfRange,
}

impl MicroError {
    pub fn disposition(&self) -> Disposition {
        match self {
            Self::MalformedEvent | Self::MalformedFirmwareResult | Self::Serialization => {
                Disposition::StopWithCleanup
            }
            Self::RpcBusy
            | Self::NoPendingRequest
            | Self::WritesIncomplete
            | Self::RequestIdExhausted
            | Self::ClockOutOfRange => Disposition::CallerMisuse,
            _ => Disposition::ReconnectAfterThreeSeconds,
        }
    }
}

impl fmt::Display for MicroError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFrame => f.write_str("Invalid HID vendor frame"),
            Self::UnknownChannel => f.write_str("Unknown HID vendor channel"),
            Self::AccumulationLimit => f.write_str("Oversized or incomplete vendor JSON message"),
            Self::NonObjectMessage => f.write_str("Vendor RPC message must be an object"),
            Self::MethodNotAllowlisted => f.write_str("Device method is not allowlisted"),
            Self::RestrictedFilesystemRead => f.write_str("Only reading keymap.json is supported"),
            Self::DeviceRejected { method, error } => {
                write!(
                    f,
                    "Device rejected {method}: {}",
                    str_value(error.as_value())
                )
            }
            Self::ResponseTimeout { method } => write!(f, "No device response to {method}"),
            Self::InputOverflow => f.write_str("Device input queue overflow; reconnect required"),
            Self::UnsupportedFirmware { version } => {
                write!(f, "Unsupported firmware: {}", str_value(version))
            }
            Self::InvalidFirmwareVersion { component } => write!(
                f,
                "invalid literal for int() with base 10: {}",
                repr_string(component)
            ),
            Self::MalformedFirmwareResult => f.write_str("Malformed sys.version result"),
            Self::ReportSize => f.write_str("HID reports must contain exactly 64 bytes"),
            Self::ShortWrite { count } => write!(f, "Short HID write: {count}"),
            Self::CapabilityFailure => f.write_str("HidP_GetCaps failed"),
            Self::CapabilityLayout => f.write_str("Unexpected Micro vendor HID report layout"),
            Self::InterfaceCount { count } => {
                write!(f, "Expected exactly one Micro interface; found {count}")
            }
            Self::CompetingOwners { names } => {
                write!(f, "Competing device owners: {}", names.join(", "))
            }
            Self::InvalidTelemetry => f.write_str("Invalid joystick telemetry"),
            Self::MalformedEvent => f.write_str("Malformed device event"),
            Self::Serialization => f.write_str("Failed to serialize device request"),
            Self::RpcBusy => f.write_str("An ordinary RPC is already pending"),
            Self::NoPendingRequest => f.write_str("No ordinary RPC is pending"),
            Self::WritesIncomplete => f.write_str("Request reports have not all been written"),
            Self::RequestIdExhausted => f.write_str("Ordinary RPC request IDs exhausted"),
            Self::ClockOutOfRange => f.write_str("RPC deadline is outside the clock range"),
        }
    }
}

impl std::error::Error for MicroError {}

/// Python-compatible string conversion used for protocol and health diagnostics.
pub fn render_diagnostic(value: &Value) -> String {
    str_value(value)
}
