use std::fmt;

use serde_json::{Map, Value, value::RawValue};

use super::json::{repr_value, serialize_replay_json};

const SURROGATE_MARKER: char = '\u{fdd0}';
/// Maximum encoded size returned by the replay JSON serializers.
pub const MAX_REPLAY_JSON_BYTES: usize = 1_048_576;
/// Maximum number of nested containers accepted by the replay JSON serializers.
pub const MAX_REPLAY_JSON_DEPTH: usize = 256;

fn append_surrogate(output: &mut String, unit: u16) {
    output.push(SURROGATE_MARKER);
    output.push('s');
    for shift in [12, 8, 4, 0] {
        let digit = u32::from((unit >> shift) & 0x0f);
        output.push(char::from_digit(digit, 16).expect("hex digit"));
    }
}

fn append_scalar(output: &mut String, scalar: char) {
    output.push(scalar);
    if scalar == SURROGATE_MARKER {
        output.push(SURROGATE_MARKER);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonSerializeError {
    /// The output would exceed [`MAX_REPLAY_JSON_BYTES`].
    SizeLimit,
    /// The value contains more than [`MAX_REPLAY_JSON_DEPTH`] nested containers.
    DepthLimit,
}

impl fmt::Display for JsonSerializeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SizeLimit => "JSON replay exceeds the size limit",
            Self::DepthLimit => "JSON replay exceeds the depth limit",
        })
    }
}

impl std::error::Error for JsonSerializeError {}

/// An owned JSON value whose destruction does not recurse through nested data.
/// Equality compares string and key meaning while retaining surrogate provenance for replay.
pub struct JsonValue {
    value: Option<Value>,
    // Parsed values encode surrogate code units in otherwise-valid Rust strings.
    surrogate_tokens: bool,
}

impl JsonValue {
    pub fn from_value(value: Value) -> Self {
        Self {
            value: Some(value),
            surrogate_tokens: false,
        }
    }

    fn from_parsed_value(value: Value) -> Self {
        Self {
            value: Some(value),
            surrogate_tokens: true,
        }
    }

    pub fn as_value(&self) -> &Value {
        self.value.as_ref().expect("JSON value is present")
    }

    fn as_value_mut(&mut self) -> &mut Value {
        self.value.as_mut().expect("JSON value is present")
    }

    pub fn as_object(&self) -> Option<&Map<String, Value>> {
        self.as_value().as_object()
    }

    /// Serializes this value as compact JSON, preserving escaped surrogate code units.
    pub fn to_json(&self) -> Result<String, JsonSerializeError> {
        serialize_replay_json(self.as_value(), self.surrogate_tokens, false)
    }

    /// Serializes this value as two-space-indented JSON, preserving escaped surrogate code units.
    pub fn to_pretty_json(&self) -> Result<String, JsonSerializeError> {
        serialize_replay_json(self.as_value(), self.surrogate_tokens, true)
    }

    pub(crate) fn into_object(self) -> Result<JsonObject, Self> {
        if self.as_object().is_some() {
            Ok(JsonObject(self))
        } else {
            Err(self)
        }
    }
}

fn drop_values(mut values: Vec<Value>) {
    while let Some(value) = values.pop() {
        match value {
            Value::Array(children) => values.extend(children),
            Value::Object(children) => values.extend(children.into_iter().map(|(_, value)| value)),
            _ => {}
        }
    }
}

impl Drop for JsonValue {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            drop_values(vec![value]);
        }
    }
}

impl fmt::Debug for JsonValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.surrogate_tokens {
            f.write_str(&repr_value(self.as_value()))
        } else {
            f.write_str(&super::json::repr_plain_value(self.as_value()))
        }
    }
}

fn encoded_literal_chars(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().flat_map(|character| {
        [
            Some(character),
            (character == SURROGATE_MARKER).then_some(character),
        ]
        .into_iter()
        .flatten()
    })
}

fn equal_values(
    left: &Value,
    right: &Value,
    left_surrogate_tokens: bool,
    right_surrogate_tokens: bool,
) -> bool {
    let cross_provenance = left_surrogate_tokens != right_surrogate_tokens;
    // Across provenance modes, compare the parsed encoding against encoded plain text.
    let (left, right) = if cross_provenance && right_surrogate_tokens {
        (right, left)
    } else {
        (left, right)
    };
    let mut pending = vec![(left, right)];
    while let Some((left, right)) = pending.pop() {
        match (left, right) {
            (Value::Null, Value::Null) => {}
            (Value::Bool(left), Value::Bool(right)) if left == right => {}
            (Value::Number(left), Value::Number(right)) if left == right => {}
            (Value::String(left), Value::String(right)) => {
                let equal = if cross_provenance {
                    left.chars().eq(encoded_literal_chars(right))
                } else {
                    left == right
                };
                if !equal {
                    return false;
                }
            }
            (Value::Array(left), Value::Array(right)) if left.len() == right.len() => {
                pending.extend(left.iter().zip(right));
            }
            (Value::Object(left), Value::Object(right)) if left.len() == right.len() => {
                for (key, right) in right {
                    let encoded = (cross_provenance && key.contains(SURROGATE_MARKER))
                        .then(|| encoded_literal_chars(key).collect::<String>());
                    let Some(left) = left.get(encoded.as_deref().unwrap_or(key)) else {
                        return false;
                    };
                    pending.push((left, right));
                }
            }
            _ => return false,
        }
    }
    true
}

impl PartialEq for JsonValue {
    fn eq(&self, other: &Self) -> bool {
        equal_values(
            self.as_value(),
            other.as_value(),
            self.surrogate_tokens,
            other.surrogate_tokens,
        )
    }
}

impl PartialEq<Value> for JsonValue {
    fn eq(&self, other: &Value) -> bool {
        equal_values(self.as_value(), other, self.surrogate_tokens, false)
    }
}

/// An owned object returned by the wire decoder and notification queue.
#[derive(Debug, PartialEq)]
pub struct JsonObject(JsonValue);

impl JsonObject {
    pub fn from_map(map: Map<String, Value>) -> Self {
        Self(JsonValue::from_value(Value::Object(map)))
    }

    pub fn as_value(&self) -> &Value {
        self.0.as_value()
    }

    pub fn as_map(&self) -> &Map<String, Value> {
        self.0.as_object().expect("JSON object is present")
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_map().get(key)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.as_map().contains_key(key)
    }

    /// Serializes this object as compact JSON, preserving escaped surrogate code units.
    pub fn to_json(&self) -> Result<String, JsonSerializeError> {
        self.0.to_json()
    }

    /// Serializes this object as two-space-indented JSON, preserving escaped surrogate code units.
    pub fn to_pretty_json(&self) -> Result<String, JsonSerializeError> {
        self.0.to_pretty_json()
    }

    pub(crate) fn remove(&mut self, key: &str) -> Option<JsonValue> {
        let surrogate_tokens = self.0.surrogate_tokens;
        self.0
            .as_value_mut()
            .as_object_mut()
            .expect("JSON object is present")
            .remove(key)
            .map(|value| JsonValue {
                value: Some(value),
                surrogate_tokens,
            })
    }
}

enum Frame {
    Array(Vec<Value>),
    Object {
        values: Map<String, Value>,
        key: Option<String>,
    },
}

#[derive(Default)]
struct Frames(Vec<Frame>);

impl Drop for Frames {
    fn drop(&mut self) {
        let mut values = Vec::new();
        for frame in self.0.drain(..) {
            match frame {
                Frame::Array(children) => values.extend(children),
                Frame::Object {
                    values: children, ..
                } => values.extend(children.into_iter().map(|(_, value)| value)),
            }
        }
        drop_values(values);
    }
}

fn attach_value(frames: &mut Frames, root: &mut Option<JsonValue>, value: Value) {
    match frames.0.last_mut() {
        Some(Frame::Array(values)) => values.push(value),
        Some(Frame::Object { values, key }) => {
            let key = key.take().expect("validated JSON object has a key");
            if let Some(previous) = values.insert(key, value) {
                drop_values(vec![previous]);
            }
        }
        None => *root = Some(JsonValue::from_parsed_value(value)),
    }
}

fn hex_unit(bytes: &[u8]) -> Option<u16> {
    let mut unit = 0u16;
    for byte in bytes {
        unit = unit.checked_mul(16)?;
        unit += match byte {
            b'0'..=b'9' => u16::from(byte - b'0'),
            b'a'..=b'f' => u16::from(byte - b'a' + 10),
            b'A'..=b'F' => u16::from(byte - b'A' + 10),
            _ => return None,
        };
    }
    Some(unit)
}

fn sanitize_surrogates(bytes: &[u8]) -> Vec<u8> {
    let mut sanitized = bytes.to_vec();
    let mut at = 0;
    let mut in_string = false;
    while at < bytes.len() {
        match bytes[at] {
            b'"' => {
                in_string = !in_string;
                at += 1;
            }
            b'\\' if in_string && bytes.get(at + 1) == Some(&b'u') => {
                let Some(unit) = bytes.get(at + 2..at + 6).and_then(hex_unit) else {
                    at += 2;
                    continue;
                };
                if (0xd800..=0xdbff).contains(&unit) {
                    let paired_low = bytes
                        .get(at + 6..at + 12)
                        .filter(|escape| escape[..2] == *b"\\u")
                        .and_then(|escape| hex_unit(&escape[2..]))
                        .is_some_and(|low| (0xdc00..=0xdfff).contains(&low));
                    if paired_low {
                        at += 12;
                        continue;
                    }
                }
                if (0xd800..=0xdfff).contains(&unit) {
                    sanitized[at + 2..at + 6].copy_from_slice(b"0000");
                }
                at += 6;
            }
            b'\\' if in_string => at += 2,
            _ => at += 1,
        }
    }
    sanitized
}

fn decode_json_string(bytes: &[u8]) -> Result<String, JsonParseError> {
    let mut result = String::new();
    let mut at = 1;
    while at + 1 < bytes.len() {
        if bytes[at] == b'\\' {
            at += 1;
            match bytes[at] {
                b'"' => result.push('"'),
                b'\\' => result.push('\\'),
                b'/' => result.push('/'),
                b'b' => result.push('\u{8}'),
                b'f' => result.push('\u{c}'),
                b'n' => result.push('\n'),
                b'r' => result.push('\r'),
                b't' => result.push('\t'),
                b'u' => {
                    let unit = hex_unit(&bytes[at + 1..at + 5]).ok_or(JsonParseError)?;
                    at += 4;
                    if (0xd800..=0xdbff).contains(&unit)
                        && bytes.get(at + 1..at + 7).is_some_and(|escape| {
                            escape[..2] == *b"\\u"
                                && hex_unit(&escape[2..])
                                    .is_some_and(|low| (0xdc00..=0xdfff).contains(&low))
                        })
                    {
                        let low = hex_unit(&bytes[at + 3..at + 7]).ok_or(JsonParseError)?;
                        let scalar = 0x10000
                            + ((u32::from(unit) - 0xd800) << 10)
                            + (u32::from(low) - 0xdc00);
                        result.push(char::from_u32(scalar).ok_or(JsonParseError)?);
                        at += 6;
                    } else if (0xd800..=0xdfff).contains(&unit) {
                        append_surrogate(&mut result, unit);
                    } else {
                        append_scalar(
                            &mut result,
                            char::from_u32(u32::from(unit)).ok_or(JsonParseError)?,
                        );
                    }
                }
                _ => return Err(JsonParseError),
            }
            at += 1;
        } else {
            let width = match bytes[at] {
                0x00..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(JsonParseError),
            };
            let text = std::str::from_utf8(bytes.get(at..at + width).ok_or(JsonParseError)?)
                .map_err(|_| JsonParseError)?;
            let character = text.chars().next().ok_or(JsonParseError)?;
            append_scalar(&mut result, character);
            at += width;
        }
    }
    Ok(result)
}

/// Parses the complete JSON document without a nesting limit or recursive drop.
/// Escaped unpaired UTF-16 code units use an escaped internal string marker.
pub fn parse_json(bytes: &[u8]) -> Result<JsonValue, JsonParseError> {
    let input = bytes;
    let sanitized = sanitize_surrogates(bytes);
    let raw: &RawValue = serde_json::from_slice(&sanitized).map_err(|_| JsonParseError)?;
    let raw_offset = raw.get().as_ptr() as usize - sanitized.as_ptr() as usize;
    let bytes = raw.get().as_bytes();
    let mut frames = Frames::default();
    let mut root = None;
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b' ' | b'\r' | b'\n' | b'\t' | b',' | b':' => at += 1,
            b'[' => {
                frames.0.push(Frame::Array(Vec::new()));
                at += 1;
            }
            b'{' => {
                frames.0.push(Frame::Object {
                    values: Map::new(),
                    key: None,
                });
                at += 1;
            }
            b']' => {
                let Some(Frame::Array(values)) = frames.0.pop() else {
                    return Err(JsonParseError);
                };
                attach_value(&mut frames, &mut root, Value::Array(values));
                at += 1;
            }
            b'}' => {
                let Some(Frame::Object { values, key: None }) = frames.0.pop() else {
                    return Err(JsonParseError);
                };
                attach_value(&mut frames, &mut root, Value::Object(values));
                at += 1;
            }
            b'"' => {
                let start = at;
                at += 1;
                while at < bytes.len() {
                    match bytes[at] {
                        b'\\' => at += 2,
                        b'"' => {
                            at += 1;
                            break;
                        }
                        _ => at += 1,
                    }
                }
                let text = decode_json_string(&input[raw_offset + start..raw_offset + at])?;
                match frames.0.last_mut() {
                    Some(Frame::Object { key, .. }) if key.is_none() => *key = Some(text),
                    _ => attach_value(&mut frames, &mut root, Value::String(text)),
                }
            }
            _ => {
                let start = at;
                while at < bytes.len()
                    && !matches!(bytes[at], b' ' | b'\r' | b'\n' | b'\t' | b',' | b']' | b'}')
                {
                    at += 1;
                }
                let scalar: Value =
                    serde_json::from_slice(&bytes[start..at]).map_err(|_| JsonParseError)?;
                attach_value(&mut frames, &mut root, scalar);
            }
        }
    }
    root.ok_or(JsonParseError)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonParseError;

impl fmt::Display for JsonParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invalid JSON")
    }
}

impl std::error::Error for JsonParseError {}
