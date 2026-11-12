//! Bounded, minimized JSON event ingestion and source timestamp parsing.

use std::{fmt, marker::PhantomData};

use serde::{
    Deserialize, Deserializer,
    de::{self, Visitor},
};
use serde_json::value::RawValue;

use crate::domain::SessionId;

use super::strict::Object;

pub const MAX_EVENT_BYTES: usize = 262_144;
const MAX_EXACT_FLOAT_INTEGER: f64 = 9_007_199_254_740_991.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseError {
    TooLarge,
    InvalidRecord,
    InvalidTimestamp,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLarge => "event input exceeds its byte limit",
            Self::InvalidRecord => "invalid event record",
            Self::InvalidTimestamp => "invalid event timestamp",
        })
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
    UserMessage,
    AssistantTurnStart,
    AssistantMessage {
        content_present: bool,
        tool_requests_present: bool,
    },
    AssistantTurnEnd,
    TaskComplete,
    Abort,
    SessionError,
    ToolExecutionStart {
        asks_user: bool,
    },
    ToolExecutionComplete,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Event {
    pub timestamp_ms: u64,
    pub kind: EventKind,
}

/// A fork child is retained only for an exactly matched fork notice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParsedEvent {
    pub event: Event,
    pub fork_child: Option<SessionId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WireKind {
    UserMessage,
    AssistantTurnStart,
    AssistantMessage,
    AssistantTurnEnd,
    TaskComplete,
    Abort,
    SessionError,
    ToolExecutionStart,
    ToolExecutionComplete,
    SessionInfo,
    Unknown,
}

impl<'de> Deserialize<'de> for WireKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KindVisitor;

        impl Visitor<'_> for KindVisitor {
            type Value = WireKind;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an event type string")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(match value {
                    "user.message" => WireKind::UserMessage,
                    "assistant.turn_start" => WireKind::AssistantTurnStart,
                    "assistant.message" => WireKind::AssistantMessage,
                    "assistant.turn_end" => WireKind::AssistantTurnEnd,
                    "session.task_complete" => WireKind::TaskComplete,
                    "abort" => WireKind::Abort,
                    "session.error" => WireKind::SessionError,
                    "tool.execution_start" => WireKind::ToolExecutionStart,
                    "tool.execution_complete" => WireKind::ToolExecutionComplete,
                    "session.info" => WireKind::SessionInfo,
                    _ => WireKind::Unknown,
                })
            }
        }

        deserializer.deserialize_str(KindVisitor)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TimestampMs(pub u64);

impl<'de> Deserialize<'de> for TimestampMs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <&RawValue>::deserialize(deserializer)?;
        parse_raw_timestamp(raw.get())
            .map(TimestampMs)
            .map_err(|_| de::Error::custom("invalid timestamp"))
    }
}

fn parse_raw_timestamp(raw: &str) -> Result<u64, ParseError> {
    if raw.starts_with('"') {
        struct TimestampString;
        impl StringSelection for TimestampString {
            type Output = Option<u64>;

            fn select(value: &str) -> Self::Output {
                parse_iso_timestamp(value).ok()
            }
        }
        return serde_json::from_str::<Selected<TimestampString>>(raw)
            .ok()
            .and_then(|selected| selected.0)
            .ok_or(ParseError::InvalidTimestamp);
    }
    if !matches!(raw.as_bytes().first(), Some(b'-' | b'0'..=b'9')) {
        return Err(ParseError::InvalidTimestamp);
    }
    let coefficient = raw.split(['e', 'E']).next().unwrap_or(raw);
    if raw.starts_with('-') && coefficient.bytes().any(|byte| matches!(byte, b'1'..=b'9')) {
        return Err(ParseError::InvalidTimestamp);
    }
    if !raw.contains(['.', 'e', 'E']) {
        return raw
            .strip_prefix('-')
            .unwrap_or(raw)
            .parse::<u64>()
            .map_err(|_| ParseError::InvalidTimestamp);
    }
    let value = raw
        .parse::<f64>()
        .map_err(|_| ParseError::InvalidTimestamp)?;
    if !value.is_finite() || !(0.0..=MAX_EXACT_FLOAT_INTEGER).contains(&value) {
        return Err(ParseError::InvalidTimestamp);
    }
    Ok(value.trunc() as u64)
}

/// Parse one JSON timestamp. Errors never contain the supplied input.
pub fn parse_timestamp_json(bytes: &[u8]) -> Result<u64, ParseError> {
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(ParseError::TooLarge);
    }
    serde_json::from_slice::<TimestampMs>(bytes)
        .map(|timestamp| timestamp.0)
        .map_err(|_| ParseError::InvalidTimestamp)
}

/// Parse the specified ASCII ISO-8601 subset, with millisecond truncation.
pub fn parse_iso_timestamp(input: &str) -> Result<u64, ParseError> {
    let bytes = input.as_bytes();
    if !(20..=35).contains(&bytes.len())
        || !bytes.is_ascii()
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(ParseError::InvalidTimestamp);
    }
    fn digits(bytes: &[u8]) -> Option<u32> {
        bytes.iter().try_fold(0_u32, |value, byte| {
            byte.is_ascii_digit()
                .then(|| value * 10 + u32::from(byte - b'0'))
        })
    }

    let year = digits(&bytes[0..4]).ok_or(ParseError::InvalidTimestamp)?;
    let month = digits(&bytes[5..7]).ok_or(ParseError::InvalidTimestamp)?;
    let day = digits(&bytes[8..10]).ok_or(ParseError::InvalidTimestamp)?;
    let hour = digits(&bytes[11..13]).ok_or(ParseError::InvalidTimestamp)?;
    let minute = digits(&bytes[14..16]).ok_or(ParseError::InvalidTimestamp)?;
    let second = digits(&bytes[17..19]).ok_or(ParseError::InvalidTimestamp)?;
    let leap = |year: u32| {
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
    };
    let month_lengths = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day == 0
        || day > month_lengths[(month - 1) as usize]
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(ParseError::InvalidTimestamp);
    }

    let mut index = 19;
    let mut millis = 0_u32;
    if bytes[index] == b'.' {
        index += 1;
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            if index - start < 3 {
                millis = millis * 10 + u32::from(bytes[index] - b'0');
            }
            index += 1;
        }
        let count = index - start;
        if !(1..=9).contains(&count) {
            return Err(ParseError::InvalidTimestamp);
        }
        for _ in count..3 {
            millis *= 10;
        }
    }

    let offset_seconds: i64 = match bytes.get(index..) {
        Some(b"Z") => 0,
        Some(offset)
            if offset.len() == 6 && matches!(offset[0], b'+' | b'-') && offset[3] == b':' =>
        {
            let offset_hour = digits(&offset[1..3]).ok_or(ParseError::InvalidTimestamp)?;
            let offset_minute = digits(&offset[4..6]).ok_or(ParseError::InvalidTimestamp)?;
            if offset_hour > 23 || offset_minute > 59 {
                return Err(ParseError::InvalidTimestamp);
            }
            let seconds = i64::from(offset_hour * 3_600 + offset_minute * 60);
            if offset[0] == b'+' { seconds } else { -seconds }
        }
        _ => return Err(ParseError::InvalidTimestamp),
    };

    let preceding_year = u64::from(year - 1);
    let leap_days = |last: u64| last / 4 - last / 100 + last / 400;
    let days = u64::from(year - 1970) * 365 + leap_days(preceding_year) - leap_days(1969)
        + month_lengths[..(month - 1) as usize]
            .iter()
            .map(|length| u64::from(*length))
            .sum::<u64>()
        + u64::from(day - 1);
    let local_seconds = i64::try_from(days * 86_400).map_err(|_| ParseError::InvalidTimestamp)?
        + i64::from(hour * 3_600 + minute * 60 + second);
    let utc_seconds = local_seconds - offset_seconds;
    let utc_seconds = u64::try_from(utc_seconds).map_err(|_| ParseError::InvalidTimestamp)?;
    Ok(utc_seconds * 1_000 + u64::from(millis))
}

#[derive(Default)]
struct Presence(bool);

impl<'de> Deserialize<'de> for Presence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <&RawValue>::deserialize(deserializer)?;
        let value = raw.get();
        let present = match value.as_bytes()[0] {
            b'n' | b'f' => false,
            b't' => true,
            b'"' => value != "\"\"",
            b'[' | b'{' => value.as_bytes()[1..value.len() - 1]
                .iter()
                .any(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n')),
            _ => value
                .bytes()
                .take_while(|byte| !matches!(byte, b'e' | b'E'))
                .any(|byte| matches!(byte, b'1'..=b'9')),
        };
        Ok(Presence(present))
    }
}

pub(crate) trait StringSelection {
    type Output: Default;
    fn select(value: &str) -> Self::Output;
}

struct SelectedString<S>(PhantomData<S>);

impl<'de, S: StringSelection> Visitor<'de> for SelectedString<S> {
    type Value = S::Output;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON string")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(S::select(value))
    }
}

struct Selected<S: StringSelection>(S::Output, PhantomData<S>);

impl<'de, S: StringSelection> Deserialize<'de> for Selected<S> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer
            .deserialize_str(SelectedString::<S>(PhantomData))
            .map(|value| Self(value, PhantomData))
    }
}

pub(crate) fn select_string<'de, D: Deserializer<'de>, S: StringSelection>(
    deserializer: D,
) -> Result<S::Output, D::Error> {
    let raw = <&RawValue>::deserialize(deserializer)?;
    if raw.get().starts_with('"') {
        serde_json::from_str::<Selected<S>>(raw.get())
            .map(|selected| selected.0)
            .map_err(|_| de::Error::custom("invalid string value"))
    } else {
        Ok(S::Output::default())
    }
}

struct AskUser;
impl StringSelection for AskUser {
    type Output = bool;
    fn select(value: &str) -> bool {
        value == "ask_user"
    }
}

struct ForkInfo;
impl StringSelection for ForkInfo {
    type Output = bool;
    fn select(value: &str) -> bool {
        value == "fork"
    }
}

struct ForkMessage;
impl StringSelection for ForkMessage {
    type Output = Option<SessionId>;
    fn select(value: &str) -> Self::Output {
        parse_fork_message(value)
    }
}

/// Match a complete fork notice without returning or retaining its label.
pub fn parse_fork_message(message: &str) -> Option<SessionId> {
    let rest = message
        .strip_prefix("Forked this session into \"")?
        .strip_suffix(").")?;
    let (label, child) = rest.rsplit_once("\" (")?;
    if label.is_empty()
        || label.chars().any(|character| {
            matches!(
                character,
                '\r' | '\n' | '\u{000b}' | '\u{000c}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
            )
        })
    {
        return None;
    }
    SessionId::parse_str(child).ok()
}

#[derive(Default, Deserialize)]
struct EventData {
    #[serde(default)]
    content: Presence,
    #[serde(default, rename = "toolRequests")]
    tool_requests: Presence,
    #[serde(
        default,
        rename = "toolName",
        deserialize_with = "select_string::<_, AskUser>"
    )]
    asks_user: bool,
    #[serde(
        default,
        rename = "infoType",
        deserialize_with = "select_string::<_, ForkInfo>"
    )]
    fork_info: bool,
    #[serde(default, deserialize_with = "select_string::<_, ForkMessage>")]
    message: Option<SessionId>,
}

#[derive(Deserialize)]
struct EventWire {
    #[serde(rename = "type")]
    kind: WireKind,
    timestamp: TimestampMs,
    #[serde(default)]
    data: Option<Object<EventData>>,
}

/// Consume one complete JSON object, retaining only structural facts.
pub fn parse_event(bytes: &[u8]) -> Result<ParsedEvent, ParseError> {
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(ParseError::TooLarge);
    }
    std::str::from_utf8(bytes).map_err(|_| ParseError::InvalidRecord)?;
    let wire: Object<EventWire> =
        serde_json::from_slice(bytes).map_err(|_| ParseError::InvalidRecord)?;
    let wire = wire.0;
    let data = wire.data.map(|data| data.0).unwrap_or_default();
    let kind = match wire.kind {
        WireKind::UserMessage => EventKind::UserMessage,
        WireKind::AssistantTurnStart => EventKind::AssistantTurnStart,
        WireKind::AssistantMessage => EventKind::AssistantMessage {
            content_present: data.content.0,
            tool_requests_present: data.tool_requests.0,
        },
        WireKind::AssistantTurnEnd => EventKind::AssistantTurnEnd,
        WireKind::TaskComplete => EventKind::TaskComplete,
        WireKind::Abort => EventKind::Abort,
        WireKind::SessionError => EventKind::SessionError,
        WireKind::ToolExecutionStart => EventKind::ToolExecutionStart {
            asks_user: data.asks_user,
        },
        WireKind::ToolExecutionComplete => EventKind::ToolExecutionComplete,
        WireKind::SessionInfo | WireKind::Unknown => EventKind::Unknown,
    };
    Ok(ParsedEvent {
        event: Event {
            timestamp_ms: wire.timestamp.0,
            kind,
        },
        fork_child: if wire.kind == WireKind::SessionInfo && data.fork_info {
            data.message
        } else {
            None
        },
    })
}
