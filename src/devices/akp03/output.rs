use std::fmt;

pub const KEY_COUNT: u8 = 6;
pub const REPORT_LEN: usize = 1025;
pub const REPORT_PAYLOAD_LEN: usize = 1024;
pub const MAX_JPEG_LEN: usize = u16::MAX as usize;
pub type OutputReport = [u8; REPORT_LEN];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Initialize,
    Brightness(u8),
    KeepAlive,
    Flush,
    ClearVolatileScene,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketError {
    InvalidKey { key: u8 },
    InvalidBrightness { percent: u8 },
    InvalidJpegLength { actual: usize },
    InvalidJpegMarkers,
}

impl fmt::Display for PacketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::InvalidKey { key } => write!(f, "invalid image key {key}; expected 0..5"),
            Self::InvalidBrightness { percent } => {
                write!(f, "invalid brightness {percent}; expected 0..=100")
            }
            Self::InvalidJpegLength { actual } => {
                write!(
                    f,
                    "invalid JPEG length {actual}; expected 4..={MAX_JPEG_LEN}"
                )
            }
            Self::InvalidJpegMarkers => f.write_str("JPEG must start with ffd8 and end with ffd9"),
        }
    }
}

impl std::error::Error for PacketError {}

fn skeleton(opcode: &[u8]) -> OutputReport {
    let mut report = [0; REPORT_LEN];
    report[1..4].copy_from_slice(b"CRT");
    report[6..6 + opcode.len()].copy_from_slice(opcode);
    report
}

/// Construct one zero-filled command report; it is not sent by this module.
pub fn command_report(command: Command) -> Result<OutputReport, PacketError> {
    let mut report = match command {
        Command::Initialize => skeleton(b"DIS"),
        Command::Brightness(percent) => {
            if percent > 100 {
                return Err(PacketError::InvalidBrightness { percent });
            }
            let mut report = skeleton(b"LIG");
            report[11] = percent;
            report
        }
        Command::KeepAlive => skeleton(b"CONNECT"),
        Command::Flush => skeleton(b"STP"),
        Command::ClearVolatileScene => skeleton(b"CLE"),
    };
    if command == Command::ClearVolatileScene {
        report[12] = 0xff;
    }
    Ok(report)
}

pub(crate) fn validate_key(key: u8) -> Result<(), PacketError> {
    if key < KEY_COUNT {
        Ok(())
    } else {
        Err(PacketError::InvalidKey { key })
    }
}

/// Frame already-encoded JPEG bytes. Only length and boundary markers are
/// checked; callers must trust the JPEG or use the exact-size RGB encoder.
pub fn frame_jpeg(key: u8, jpeg: &[u8]) -> Result<Vec<OutputReport>, PacketError> {
    validate_key(key)?;
    if !(4..=MAX_JPEG_LEN).contains(&jpeg.len()) {
        return Err(PacketError::InvalidJpegLength { actual: jpeg.len() });
    }
    if !jpeg.starts_with(&[0xff, 0xd8]) || !jpeg.ends_with(&[0xff, 0xd9]) {
        return Err(PacketError::InvalidJpegMarkers);
    }

    let mut reports = Vec::with_capacity(jpeg.len().div_ceil(REPORT_PAYLOAD_LEN) + 2);
    let mut start = skeleton(b"BAT");
    let length = (jpeg.len() as u16).to_be_bytes();
    start[11..13].copy_from_slice(&length);
    start[13] = key + 1;
    reports.push(start);

    for chunk in jpeg.chunks(REPORT_PAYLOAD_LEN) {
        let mut report = [0; REPORT_LEN];
        report[1..1 + chunk.len()].copy_from_slice(chunk);
        reports.push(report);
    }

    reports.push(skeleton(b"STP"));
    Ok(reports)
}
