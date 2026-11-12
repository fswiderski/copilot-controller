//! Transactional, bounded JSONL polling over an explicit safe read boundary.
//! Native Windows roots must be fixed local drives; unsupported platforms refuse.

use std::{
    fmt,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use super::{
    event::{MAX_EVENT_BYTES, parse_event},
    fork::ForkObservation,
    reducer::Reducer,
};

pub const MAX_POLL_BYTES: usize = 4_194_304;
pub const MAX_FORKS_PER_POLL: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    pub volume: u64,
    pub file: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceSnapshot {
    pub identity: FileIdentity,
    pub length: u64,
    pub modified: i128,
    pub changed: i128,
}

#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
pub(super) fn validate_open_and_named(
    opened: SourceSnapshot,
    named: SourceSnapshot,
) -> Result<SourceSnapshot, TailError> {
    // Both handles may observe a legitimate append at different instants.
    if opened.identity != named.identity
        || opened.length == named.length
            && (opened.modified != named.modified || opened.changed != named.changed)
    {
        Err(TailError::SourceChanged)
    } else {
        Ok(named)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TailError {
    InvalidBudget,
    InvalidPath,
    UnsafePath,
    UnsupportedPlatform,
    Io,
    InvalidRecord,
    OversizedRecord,
    SourceChanged,
}

impl fmt::Display for TailError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidBudget => "tail budget is outside its allowed range",
            Self::InvalidPath => "tail path is invalid",
            Self::UnsafePath => "tail path or file is unsafe",
            Self::UnsupportedPlatform => "native tail boundary is unavailable",
            Self::Io => "tail read failed",
            Self::InvalidRecord => "tail contains an invalid event record",
            Self::OversizedRecord => "tail record exceeds its byte limit",
            Self::SourceChanged => "tail source changed during polling",
        })
    }
}

impl std::error::Error for TailError {}

/// A recheck must verify both the open handle and the named path/ancestors.
pub trait TailFile: Read + Seek {
    fn recheck(&mut self) -> Result<SourceSnapshot, TailError>;
}

/// Injectable for deterministic tests; an injected boundary is not authority.
pub trait TailBoundary {
    type File: TailFile;
    fn open(&self, path: &Path) -> Result<Self::File, TailError>;
}

#[derive(Clone, Default, Eq, PartialEq)]
pub struct TailCursor {
    source: Option<SourceSnapshot>,
    offset: u64,
    partial: Vec<u8>,
    caught_up: bool,
}

impl fmt::Debug for TailCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TailCursor")
            .field("source", &self.source)
            .field("offset", &self.offset)
            .field("partial_bytes", &self.partial.len())
            .field("caught_up", &self.caught_up)
            .finish()
    }
}

impl TailCursor {
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn partial_bytes(&self) -> usize {
        self.partial.len()
    }

    pub fn caught_up(&self) -> bool {
        self.caught_up
    }

    pub fn source_identity(&self) -> Option<FileIdentity> {
        self.source.map(|source| source.identity)
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailReport {
    pub records: usize,
    /// Physical bytes read, including any read-ahead revisited by the next poll.
    pub bytes_read: usize,
    pub caught_up: bool,
    pub reset: bool,
    pub fork_observations: Vec<ForkObservation>,
}

/// A fault resets the cursor and invalidates the caller's reducer atomically.
pub fn poll<B: TailBoundary>(
    boundary: &B,
    path: &Path,
    budget: usize,
    cursor: &mut TailCursor,
    reducer: &mut Reducer,
) -> Result<TailReport, TailError> {
    let result = poll_inner(boundary, path, budget, cursor, reducer);
    match result {
        Ok((next_cursor, next_reducer, report)) => {
            *cursor = next_cursor;
            *reducer = next_reducer;
            Ok(report)
        }
        Err(error) => {
            cursor.reset();
            reducer.invalidate();
            Err(error)
        }
    }
}

fn poll_inner<B: TailBoundary>(
    boundary: &B,
    path: &Path,
    budget: usize,
    cursor: &TailCursor,
    reducer: &Reducer,
) -> Result<(TailCursor, Reducer, TailReport), TailError> {
    if !(1..=MAX_POLL_BYTES).contains(&budget) {
        return Err(TailError::InvalidBudget);
    }
    if !path.is_absolute() {
        return Err(TailError::InvalidPath);
    }
    let mut file = boundary.open(path)?;
    let initial = file.recheck()?;
    let reset = match cursor.source {
        None => true,
        Some(previous) => {
            previous.identity != initial.identity
                || initial.length < previous.length
                || cursor.offset == previous.length && previous.modified != initial.modified
        }
    };
    let mut next_cursor = if reset {
        TailCursor::default()
    } else {
        cursor.clone()
    };
    let mut next_reducer = if reset { Reducer::new() } else { *reducer };
    let positioned = file
        .seek(SeekFrom::Start(next_cursor.offset))
        .map_err(|_| TailError::Io)?;
    if positioned != next_cursor.offset {
        return Err(TailError::Io);
    }

    let mut report = TailReport {
        records: 0,
        bytes_read: 0,
        caught_up: false,
        reset,
        fork_observations: Vec::new(),
    };
    let mut buffer = [0_u8; 8192];
    let mut saw_eof = false;
    while report.bytes_read < budget {
        let limit = (budget - report.bytes_read).min(buffer.len());
        let count = file.read(&mut buffer[..limit]).map_err(|_| TailError::Io)?;
        if count > limit {
            return Err(TailError::Io);
        }
        if count == 0 {
            saw_eof = true;
            break;
        }
        report.bytes_read += count;
        let stop = process_bytes(
            &buffer[..count],
            &mut next_cursor,
            &mut next_reducer,
            &mut report,
        )?;
        if stop {
            break;
        }
    }
    let after = file.recheck()?;
    if after.identity != initial.identity
        || after.length < next_cursor.offset
        || after.length == initial.length
            && (after.modified != initial.modified || after.changed != initial.changed)
    {
        return Err(TailError::SourceChanged);
    }
    if saw_eof && next_cursor.offset < after.length && after.length <= initial.length {
        return Err(TailError::Io);
    }
    next_cursor.caught_up = next_cursor.offset == after.length && next_cursor.partial.is_empty();
    next_cursor.source = Some(after);
    report.caught_up = next_cursor.caught_up;
    Ok((next_cursor, next_reducer, report))
}

fn process_bytes(
    bytes: &[u8],
    cursor: &mut TailCursor,
    reducer: &mut Reducer,
    report: &mut TailReport,
) -> Result<bool, TailError> {
    let mut start = 0;
    while start < bytes.len() {
        let newline = bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| start + index);
        let end = newline.map_or(bytes.len(), |index| index + 1);
        let terminated = newline.is_some();
        let length = cursor
            .partial
            .len()
            .checked_add(end - start)
            .ok_or(TailError::OversizedRecord)?;
        if length > MAX_EVENT_BYTES || !terminated && length == MAX_EVENT_BYTES {
            return Err(TailError::OversizedRecord);
        }
        cursor.partial.extend_from_slice(&bytes[start..end]);
        cursor.offset = cursor
            .offset
            .checked_add((end - start) as u64)
            .ok_or(TailError::SourceChanged)?;
        start = end;
        if !terminated {
            continue;
        }
        let blank = std::str::from_utf8(&cursor.partial)
            .map_err(|_| TailError::InvalidRecord)?
            .trim()
            .is_empty();
        if !blank {
            let parsed = parse_event(&cursor.partial).map_err(|_| TailError::InvalidRecord)?;
            reducer.apply_event(parsed.event);
            report.records += 1;
            if let Some(child) = parsed.fork_child {
                report.fork_observations.push(ForkObservation {
                    child,
                    timestamp_ms: parsed.event.timestamp_ms,
                    next_offset: cursor.offset,
                });
            }
        }
        cursor.partial.clear();
        if report.fork_observations.len() == MAX_FORKS_PER_POLL {
            // Unprocessed read-ahead is revisited from the cursor offset next poll.
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "tail_unix.rs"]
mod native;
#[cfg(windows)]
#[path = "tail_windows.rs"]
mod native;

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
pub use native::NativeBoundary;

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub struct NativeBoundary;

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
impl TailBoundary for NativeBoundary {
    type File = std::fs::File;

    fn open(&self, _: &Path) -> Result<Self::File, TailError> {
        Err(TailError::UnsupportedPlatform)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
impl TailFile for std::fs::File {
    fn recheck(&mut self) -> Result<SourceSnapshot, TailError> {
        Err(TailError::UnsupportedPlatform)
    }
}
