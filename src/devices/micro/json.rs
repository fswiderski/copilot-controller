use std::fmt::Write as _;

use serde_json::Value;
use unicode_general_category::{GeneralCategory, get_general_category};

use super::value::{JsonSerializeError, MAX_REPLAY_JSON_BYTES, MAX_REPLAY_JSON_DEPTH};

const SURROGATE_MARKER: char = '\u{fdd0}';

fn take_surrogate_token(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<u16> {
    if chars.peek() != Some(&'s') {
        return None;
    }
    let token: String = chars.clone().take(5).collect();
    if token.len() != 5 {
        return None;
    }
    let unit = u16::from_str_radix(&token[1..], 16).ok()?;
    if !(0xd800..=0xdfff).contains(&unit) {
        return None;
    }
    for _ in 0..5 {
        chars.next();
    }
    Some(unit)
}

pub(crate) fn python_whitespace(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

pub(crate) fn numeric(value: &Value) -> Option<f64> {
    let Value::Number(number) = value else {
        return None;
    };
    let number = number.to_string().parse::<f64>().ok()?;
    number.is_finite().then_some(number)
}

pub(crate) fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.to_string().parse::<f64>().is_ok_and(|n| n != 0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}

pub(crate) fn numeric_equals_u64(value: &Value, expected: u64) -> bool {
    match value {
        Value::Bool(v) => u64::from(*v) == expected,
        Value::Number(number) => {
            let raw = number.to_string();
            if raw.contains(['.', 'e', 'E']) {
                raw.parse::<f64>().is_ok_and(|number| {
                    number.is_finite()
                        && (0.0..18_446_744_073_709_551_616.0).contains(&number)
                        && number as u64 == expected
                        && number.fract() == 0.0
                })
            } else if raw == "-0" {
                expected == 0
            } else {
                raw.parse::<u64>() == Ok(expected)
            }
        }
        _ => false,
    }
}

pub(crate) fn category_c(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::Surrogate
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned
    )
}

fn python_printable(c: char) -> bool {
    c == ' '
        || (!category_c(c)
            && !matches!(
                get_general_category(c),
                GeneralCategory::SpaceSeparator
                    | GeneralCategory::LineSeparator
                    | GeneralCategory::ParagraphSeparator
            ))
}

pub(crate) fn repr_string(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::new();
    result.push(quote);
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == SURROGATE_MARKER
            && let Some(&next) = chars.peek()
        {
            if next == SURROGATE_MARKER {
                chars.next();
            } else if let Some(unit) = take_surrogate_token(&mut chars) {
                write!(result, "\\u{unit:04x}").expect("writing to a string cannot fail");
                continue;
            }
        }
        match c {
            '\\' => result.push_str("\\\\"),
            '\t' => result.push_str("\\t"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            c if c == quote => {
                result.push('\\');
                result.push(c);
            }
            c if python_printable(c) => result.push(c),
            c if (c as u32) <= 0xff => {
                write!(result, "\\x{:02x}", c as u32).expect("writing to a string cannot fail");
            }
            c if (c as u32) <= 0xffff => {
                write!(result, "\\u{:04x}", c as u32).expect("writing to a string cannot fail");
            }
            c => {
                write!(result, "\\U{:08x}", c as u32).expect("writing to a string cannot fail");
            }
        }
    }
    result.push(quote);
    result
}

pub(crate) fn repr_float(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-inf"
        } else {
            "inf"
        }
        .to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_owned();
    }

    let mut buffer = ryu::Buffer::new();
    let raw = buffer.format_finite(value.abs());
    let (mantissa, exponent) = raw
        .split_once('e')
        .map_or((raw, 0), |(m, e)| (m, e.parse::<i32>().unwrap_or(0)));
    let dot = mantissa.find('.').unwrap_or(mantissa.len());
    let mut digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let leading = digits.bytes().take_while(|c| *c == b'0').count();
    let exponent = exponent + dot as i32 - 1 - leading as i32;
    digits.drain(..leading);
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }

    let mut result = if value.is_sign_negative() {
        String::from("-")
    } else {
        String::new()
    };
    if (-4..=15).contains(&exponent) {
        if exponent < 0 {
            result.push_str("0.");
            result.extend(std::iter::repeat_n('0', (-exponent - 1) as usize));
            result.push_str(&digits);
        } else {
            let whole = exponent as usize + 1;
            if digits.len() <= whole {
                result.push_str(&digits);
                result.extend(std::iter::repeat_n('0', whole - digits.len()));
                result.push_str(".0");
            } else {
                result.push_str(&digits[..whole]);
                result.push('.');
                result.push_str(&digits[whole..]);
            }
        }
    } else {
        result.push(digits.as_bytes()[0] as char);
        if digits.len() > 1 {
            result.push('.');
            result.push_str(&digits[1..]);
        }
        write!(
            result,
            "e{}{:02}",
            if exponent < 0 { '-' } else { '+' },
            exponent.unsigned_abs()
        )
        .expect("writing to a string cannot fail");
    }
    result
}

fn render_number(number: &serde_json::Number) -> String {
    let raw = number.to_string();
    if raw.contains(['.', 'e', 'E']) {
        raw.parse::<f64>().map_or(raw, repr_float)
    } else if raw == "-0" {
        "0".to_owned()
    } else {
        raw
    }
}

pub(crate) fn append_json_string(output: &mut String, text: &str) -> Result<(), ()> {
    fn append_chunk(output: &mut String, chunk: &str) -> Result<(), ()> {
        let encoded = serde_json::to_string(chunk).map_err(|_| ())?;
        output.push_str(&encoded[1..encoded.len() - 1]);
        Ok(())
    }

    output.push('"');
    let mut chunk = String::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == SURROGATE_MARKER
            && let Some(&next) = chars.peek()
        {
            if next == SURROGATE_MARKER {
                chars.next();
                chunk.push(SURROGATE_MARKER);
                continue;
            }
            if let Some(unit) = take_surrogate_token(&mut chars) {
                append_chunk(output, &chunk)?;
                chunk.clear();
                write!(output, "\\u{unit:04x}").expect("writing to a string cannot fail");
                continue;
            }
        }
        chunk.push(character);
    }
    append_chunk(output, &chunk)?;
    output.push('"');
    Ok(())
}

enum RenderItem<'a> {
    Value(&'a Value),
    Text(&'static str),
    String(&'a str),
}

fn render(
    output: &mut String,
    value: &Value,
    python: bool,
    surrogate_tokens: bool,
) -> Result<(), ()> {
    let mut pending = vec![RenderItem::Value(value)];
    while let Some(item) = pending.pop() {
        match item {
            RenderItem::Text(text) => output.push_str(text),
            RenderItem::String(text) => {
                if python {
                    output.push_str(&repr_string(text));
                } else if surrogate_tokens {
                    append_json_string(output, text)?;
                } else {
                    output.push_str(&serde_json::to_string(text).map_err(|_| ())?);
                }
            }
            RenderItem::Value(Value::Null) => {
                output.push_str(if python { "None" } else { "null" });
            }
            RenderItem::Value(Value::Bool(value)) => {
                output.push_str(match (*value, python) {
                    (true, true) => "True",
                    (false, true) => "False",
                    (true, false) => "true",
                    (false, false) => "false",
                });
            }
            RenderItem::Value(Value::Number(number)) => {
                if python {
                    output.push_str(&render_number(number));
                } else {
                    let raw = number.to_string();
                    if raw.contains(['.', 'e', 'E']) {
                        let number = raw.parse::<f64>().map_err(|_| ())?;
                        if !number.is_finite() {
                            return Err(());
                        }
                        output.push_str(&repr_float(number));
                    } else if raw == "-0" {
                        output.push('0');
                    } else {
                        output.push_str(&raw);
                    }
                }
            }
            RenderItem::Value(Value::String(text)) => pending.push(RenderItem::String(text)),
            RenderItem::Value(Value::Array(values)) => {
                output.push('[');
                pending.push(RenderItem::Text("]"));
                for (index, value) in values.iter().enumerate().rev() {
                    pending.push(RenderItem::Value(value));
                    if index != 0 {
                        pending.push(RenderItem::Text(if python { ", " } else { "," }));
                    }
                }
            }
            RenderItem::Value(Value::Object(values)) => {
                output.push('{');
                pending.push(RenderItem::Text("}"));
                let mut entries: Vec<_> = values.iter().collect();
                entries.sort_unstable_by_key(|(key, _)| *key);
                for (index, (key, value)) in entries.into_iter().enumerate().rev() {
                    pending.push(RenderItem::Value(value));
                    pending.push(RenderItem::Text(if python { ": " } else { ":" }));
                    pending.push(RenderItem::String(key));
                    if index != 0 {
                        pending.push(RenderItem::Text(if python { ", " } else { "," }));
                    }
                }
            }
        }
    }
    Ok(())
}

enum ReplayEntry<'a> {
    Array(&'a Value),
    Object(&'a str, &'a Value),
}

enum ReplayFrame<'a> {
    Array {
        values: std::slice::Iter<'a, Value>,
        depth: usize,
        first: bool,
    },
    Object {
        values: serde_json::map::Iter<'a>,
        depth: usize,
        first: bool,
    },
}

impl<'a> ReplayFrame<'a> {
    fn next(&mut self) -> Option<(ReplayEntry<'a>, usize, bool)> {
        match self {
            Self::Array {
                values,
                depth,
                first,
            } => {
                let value = values.next()?;
                let was_first = *first;
                *first = false;
                Some((ReplayEntry::Array(value), *depth, was_first))
            }
            Self::Object {
                values,
                depth,
                first,
            } => {
                let (key, value) = values.next()?;
                let was_first = *first;
                *first = false;
                Some((ReplayEntry::Object(key, value), *depth, was_first))
            }
        }
    }

    fn depth(&self) -> usize {
        match self {
            Self::Array { depth, .. } | Self::Object { depth, .. } => *depth,
        }
    }
}

struct ReplayWriter {
    output: String,
}

impl std::fmt::Write for ReplayWriter {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.output.len().saturating_add(text.len()) > MAX_REPLAY_JSON_BYTES {
            return Err(std::fmt::Error);
        }
        self.output.push_str(text);
        Ok(())
    }
}

impl ReplayWriter {
    fn push_str(&mut self, text: &str) -> Result<(), JsonSerializeError> {
        if self.output.len().saturating_add(text.len()) > MAX_REPLAY_JSON_BYTES {
            return Err(JsonSerializeError::SizeLimit);
        }
        self.output.push_str(text);
        Ok(())
    }

    fn push_char(&mut self, character: char) -> Result<(), JsonSerializeError> {
        if self.output.len().saturating_add(character.len_utf8()) > MAX_REPLAY_JSON_BYTES {
            return Err(JsonSerializeError::SizeLimit);
        }
        self.output.push(character);
        Ok(())
    }

    fn push_indent(&mut self, depth: usize) -> Result<(), JsonSerializeError> {
        for _ in 0..depth {
            self.push_str("  ")?;
        }
        Ok(())
    }

    fn push_unit_escape(&mut self, unit: u16) -> Result<(), JsonSerializeError> {
        self.push_str("\\u")?;
        for shift in [12, 8, 4, 0] {
            let digit = u32::from((unit >> shift) & 0x0f);
            self.push_char(char::from_digit(digit, 16).expect("hex digit"))?;
        }
        Ok(())
    }

    fn push_json_character(&mut self, character: char) -> Result<(), JsonSerializeError> {
        match character {
            '"' => self.push_str("\\\""),
            '\\' => self.push_str("\\\\"),
            '\u{8}' => self.push_str("\\b"),
            '\u{c}' => self.push_str("\\f"),
            '\n' => self.push_str("\\n"),
            '\r' => self.push_str("\\r"),
            '\t' => self.push_str("\\t"),
            character if character <= '\u{1f}' => self.push_unit_escape(character as u16),
            character => self.push_char(character),
        }
    }

    fn push_json_string(
        &mut self,
        text: &str,
        surrogate_tokens: bool,
    ) -> Result<(), JsonSerializeError> {
        self.push_char('"')?;
        let mut chars = text.chars().peekable();
        while let Some(character) = chars.next() {
            if surrogate_tokens && character == SURROGATE_MARKER {
                if chars.peek() == Some(&SURROGATE_MARKER) {
                    chars.next();
                } else if let Some(unit) = take_surrogate_token(&mut chars) {
                    self.push_unit_escape(unit)?;
                    continue;
                }
            }
            self.push_json_character(character)?;
        }
        self.push_char('"')
    }
}

pub(crate) fn serialize_replay_json(
    value: &Value,
    surrogate_tokens: bool,
    pretty: bool,
) -> Result<String, JsonSerializeError> {
    let mut writer = ReplayWriter {
        output: String::new(),
    };
    let mut pending = Vec::new();
    let mut current = Some((value, 0));
    loop {
        if let Some((value, depth)) = current.take() {
            match value {
                Value::Null => writer.push_str("null")?,
                Value::Bool(value) => {
                    writer.push_str(if *value { "true" } else { "false" })?;
                }
                Value::Number(number) => {
                    write!(&mut writer, "{number}").map_err(|_| JsonSerializeError::SizeLimit)?;
                }
                Value::String(text) => {
                    writer.push_json_string(text, surrogate_tokens)?;
                }
                Value::Array(values) => {
                    if depth >= MAX_REPLAY_JSON_DEPTH {
                        return Err(JsonSerializeError::DepthLimit);
                    }
                    writer.push_char('[')?;
                    if values.is_empty() {
                        writer.push_char(']')?;
                    } else {
                        if pretty {
                            writer.push_char('\n')?;
                        }
                        pending
                            .try_reserve(1)
                            .map_err(|_| JsonSerializeError::SizeLimit)?;
                        pending.push(ReplayFrame::Array {
                            values: values.iter(),
                            depth,
                            first: true,
                        });
                    }
                }
                Value::Object(values) => {
                    if depth >= MAX_REPLAY_JSON_DEPTH {
                        return Err(JsonSerializeError::DepthLimit);
                    }
                    writer.push_char('{')?;
                    if values.is_empty() {
                        writer.push_char('}')?;
                    } else {
                        if pretty {
                            writer.push_char('\n')?;
                        }
                        pending
                            .try_reserve(1)
                            .map_err(|_| JsonSerializeError::SizeLimit)?;
                        pending.push(ReplayFrame::Object {
                            values: values.iter(),
                            depth,
                            first: true,
                        });
                    }
                }
            }
            continue;
        }

        if let Some((entry, depth, first)) = pending.last_mut().and_then(ReplayFrame::next) {
            if !first {
                writer.push_char(',')?;
                if pretty {
                    writer.push_char('\n')?;
                }
            }
            if pretty {
                writer.push_indent(depth + 1)?;
            }
            match entry {
                ReplayEntry::Array(value) => current = Some((value, depth + 1)),
                ReplayEntry::Object(key, value) => {
                    writer.push_json_string(key, surrogate_tokens)?;
                    writer.push_str(if pretty { ": " } else { ":" })?;
                    current = Some((value, depth + 1));
                }
            }
            continue;
        }

        let Some(frame) = pending.pop() else {
            break;
        };
        if pretty {
            writer.push_char('\n')?;
            writer.push_indent(frame.depth())?;
        }
        writer.push_char(match frame {
            ReplayFrame::Array { .. } => ']',
            ReplayFrame::Object { .. } => '}',
        })?;
    }
    Ok(writer.output)
}

pub(crate) fn repr_value(value: &Value) -> String {
    repr_value_with_tokens(value, true)
}

pub(crate) fn repr_plain_value(value: &Value) -> String {
    repr_value_with_tokens(value, false)
}

fn repr_value_with_tokens(value: &Value, surrogate_tokens: bool) -> String {
    let mut result = String::new();
    render(&mut result, value, true, surrogate_tokens).expect("Python rendering cannot fail");
    result
}

pub(crate) fn str_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => repr_value(value),
    }
}

pub(crate) fn append_json(output: &mut String, value: &Value) -> Result<(), ()> {
    render(output, value, false, true)
}

pub(crate) fn append_json_plain(output: &mut String, value: &Value) -> Result<(), ()> {
    render(output, value, false, false)
}
