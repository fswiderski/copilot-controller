use super::Profile;
use serde::{Deserialize, Serialize};
use std::fmt;

pub const MAX_INPUT_REPORT_LEN: usize = 512;
pub const MIN_HEX_LEN: usize = 20;
pub const MAX_HEX_LEN: usize = MAX_INPUT_REPORT_LEN * 2;

/// One complete report's meaning. An observation is not a release or a toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", from = "StrictDecodedInput")]
pub enum DecodedInput {
    Acknowledgment,
    NoControl,
    ButtonPressObserved { index: u8 },
    EncoderPressObserved { index: u8 },
    EncoderTurn { index: u8, delta: i8 },
    ButtonState { index: u8, pressed: bool },
    EncoderState { index: u8, pressed: bool },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum StrictDecodedInput {
    Acknowledgment {},
    NoControl {},
    ButtonPressObserved { index: u8 },
    EncoderPressObserved { index: u8 },
    EncoderTurn { index: u8, delta: i8 },
    ButtonState { index: u8, pressed: bool },
    EncoderState { index: u8, pressed: bool },
}

impl From<StrictDecodedInput> for DecodedInput {
    fn from(input: StrictDecodedInput) -> Self {
        match input {
            StrictDecodedInput::Acknowledgment {} => Self::Acknowledgment,
            StrictDecodedInput::NoControl {} => Self::NoControl,
            StrictDecodedInput::ButtonPressObserved { index } => {
                Self::ButtonPressObserved { index }
            }
            StrictDecodedInput::EncoderPressObserved { index } => {
                Self::EncoderPressObserved { index }
            }
            StrictDecodedInput::EncoderTurn { index, delta } => Self::EncoderTurn { index, delta },
            StrictDecodedInput::ButtonState { index, pressed } => {
                Self::ButtonState { index, pressed }
            }
            StrictDecodedInput::EncoderState { index, pressed } => {
                Self::EncoderState { index, pressed }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    InvalidLength { profile: Profile, actual: usize },
    InvalidHeader,
    UnknownControl { code: u8 },
    InvalidPushState { code: u8, state: u8 },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::InvalidLength { profile, actual } => write!(
                f,
                "invalid {} input report length {actual}; expected {}..={MAX_INPUT_REPORT_LEN}",
                profile.id(),
                profile.minimum_input_report_len()
            ),
            Self::InvalidHeader => f.write_str("input report does not begin with ACK"),
            Self::UnknownControl { code } => write!(f, "unknown input control code {code:#04x}"),
            Self::InvalidPushState { code, state } => {
                write!(f, "invalid state {state:#04x} for push code {code:#04x}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexError {
    InvalidLength { bytes: usize },
    InvalidDigit { offset: usize },
}

impl fmt::Display for HexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::InvalidLength { bytes } => write!(
                f,
                "hex report must have an even ASCII length in {MIN_HEX_LEN}..={MAX_HEX_LEN}; got {bytes} bytes"
            ),
            Self::InvalidDigit { offset } => {
                write!(f, "invalid ASCII hex digit at byte offset {offset}")
            }
        }
    }
}

impl std::error::Error for HexError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexReportError {
    Hex(HexError),
    Report(DecodeError),
}

impl fmt::Display for HexReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hex(error) => error.fmt(f),
            Self::Report(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for HexReportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Hex(error) => Some(error),
            Self::Report(error) => Some(error),
        }
    }
}

/// Parse ASCII hex without trimming, separators, or a profile length check.
pub fn parse_hex_report(hex: &str) -> Result<Vec<u8>, HexError> {
    let bytes = hex.as_bytes();
    if !(MIN_HEX_LEN..=MAX_HEX_LEN).contains(&bytes.len()) || !bytes.len().is_multiple_of(2) {
        return Err(HexError::InvalidLength { bytes: bytes.len() });
    }

    let mut report = Vec::with_capacity(bytes.len() / 2);
    for (pair_index, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        let high = hex_digit(pair[0]).ok_or(HexError::InvalidDigit {
            offset: pair_index * 2,
        })?;
        let low = hex_digit(pair[1]).ok_or(HexError::InvalidDigit {
            offset: pair_index * 2 + 1,
        })?;
        report.push((high << 4) | low);
    }
    Ok(report)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Parse strict hex first, then apply the selected profile's report rules.
pub fn decode_hex_report(profile: Profile, hex: &str) -> Result<DecodedInput, HexReportError> {
    let report = parse_hex_report(hex).map_err(HexReportError::Hex)?;
    decode_report(profile, &report).map_err(HexReportError::Report)
}

/// Decode exactly one complete HID report, without removing a report ID.
pub fn decode_report(profile: Profile, report: &[u8]) -> Result<DecodedInput, DecodeError> {
    if !(profile.minimum_input_report_len()..=MAX_INPUT_REPORT_LEN).contains(&report.len()) {
        return Err(DecodeError::InvalidLength {
            profile,
            actual: report.len(),
        });
    }
    if !report.starts_with(b"ACK") {
        return Err(DecodeError::InvalidHeader);
    }

    let code = report[9];
    let acknowledgment = report.starts_with(b"ACK\0\0OK");
    if acknowledgment && (profile == Profile::Original || code == 0) {
        return Ok(DecodedInput::Acknowledgment);
    }
    if code == 0 {
        return Ok(DecodedInput::NoControl);
    }

    match code {
        0x01..=0x06 => button(profile, report, code, code - 1),
        0x25 => button(profile, report, code, 6),
        0x30 => button(profile, report, code, 7),
        0x31 => button(profile, report, code, 8),
        0x33 => encoder_press(profile, report, code, 0),
        0x35 => encoder_press(profile, report, code, 1),
        0x34 => encoder_press(profile, report, code, 2),
        0x90 => Ok(DecodedInput::EncoderTurn {
            index: 0,
            delta: -1,
        }),
        0x91 => Ok(DecodedInput::EncoderTurn { index: 0, delta: 1 }),
        0x50 => Ok(DecodedInput::EncoderTurn {
            index: 1,
            delta: -1,
        }),
        0x51 => Ok(DecodedInput::EncoderTurn { index: 1, delta: 1 }),
        0x60 => Ok(DecodedInput::EncoderTurn {
            index: 2,
            delta: -1,
        }),
        0x61 => Ok(DecodedInput::EncoderTurn { index: 2, delta: 1 }),
        _ => Err(DecodeError::UnknownControl { code }),
    }
}

fn push_state(report: &[u8], code: u8) -> Result<bool, DecodeError> {
    match report[10] {
        0 => Ok(false),
        1 => Ok(true),
        state => Err(DecodeError::InvalidPushState { code, state }),
    }
}

fn button(
    profile: Profile,
    report: &[u8],
    code: u8,
    index: u8,
) -> Result<DecodedInput, DecodeError> {
    match profile {
        Profile::Original => Ok(DecodedInput::ButtonPressObserved { index }),
        Profile::Rev2 => Ok(DecodedInput::ButtonState {
            index,
            pressed: push_state(report, code)?,
        }),
    }
}

fn encoder_press(
    profile: Profile,
    report: &[u8],
    code: u8,
    index: u8,
) -> Result<DecodedInput, DecodeError> {
    match profile {
        Profile::Original => Ok(DecodedInput::EncoderPressObserved { index }),
        Profile::Rev2 => Ok(DecodedInput::EncoderState {
            index,
            pressed: push_state(report, code)?,
        }),
    }
}
