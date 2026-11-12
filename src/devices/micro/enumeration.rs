use std::collections::BTreeSet;
use std::time::Duration;

use super::error::MicroError;
use super::json::python_whitespace;

pub const APPLICATION_VID: u16 = 0x303a;
pub const APPLICATION_PID: u16 = 0x8298;
pub const USAGE_PAGE: u16 = 0xff00;
pub const USAGE: u16 = 1;
pub const REPORT_SIZE: usize = 64;
pub const WINDOWS_INTERFACE_NUMBER: u8 = 2;
pub const WINDOWS_DEVICE_CLASS: &str = "{4D1E55B2-F16F-11CF-88CB-001111000030}";
pub const WINDOWS_INTERFACE_PREFIX: &str = r"\\?\HID#VID_303A&PID_8298&MI_02#";
pub const WINDOWS_OWNERSHIP_MUTEX: &str = r"Local\CreatorMicroDevice-303A-8298";
pub const REFERENCE_WRITE_TIMEOUT: Duration = Duration::from_secs(3);
pub const REFERENCE_READ_TIMEOUT: Duration = Duration::from_millis(100);
pub const RECONNECT_DELAY: Duration = Duration::from_secs(3);
pub const DEVICE_STATUS_INTERVAL: Duration = Duration::from_secs(10);
pub const CONFLICT_SCAN_INTERVAL: Duration = Duration::from_secs(5);
pub const STATUS_SNAPSHOT_INTERVAL: Duration = Duration::from_millis(750);

fn prefix_end(text: &str, start: usize) -> Option<usize> {
    let mut end = start;
    let mut actual = text[start..].chars();
    for expected in WINDOWS_INTERFACE_PREFIX.chars() {
        let c = actual.next()?;
        let matches = if expected.eq_ignore_ascii_case(&'i') {
            c.eq_ignore_ascii_case(&'i') || matches!(c, '\u{130}' | '\u{131}')
        } else {
            c.eq_ignore_ascii_case(&expected)
        };
        if !matches {
            return None;
        }
        end += c.len_utf8();
    }
    Some(end)
}

/// Finds all non-overlapping textual paths, preserving lossy-decoded spelling.
pub fn windows_paths(bytes: &[u8]) -> Vec<String> {
    let decoded = String::from_utf8_lossy(bytes);
    let text = decoded.as_ref();
    let mut paths = Vec::new();
    let mut at = 0;
    while at < text.len() {
        if let Some(prefix_end) = prefix_end(text, at)
            && let Some(first) = text[prefix_end..].chars().next()
            && !python_whitespace(first)
        {
            let suffix_end = text[prefix_end..]
                .find(python_whitespace)
                .map_or(text.len(), |offset| prefix_end + offset);
            paths.push(text[at..suffix_end].to_owned());
            at = suffix_end;
        } else {
            at += text[at..]
                .chars()
                .next()
                .expect("at a character boundary")
                .len_utf8();
        }
    }
    paths
}

pub fn select_windows_path(bytes: &[u8]) -> Result<String, MicroError> {
    let mut paths = windows_paths(bytes);
    if paths.len() != 1 {
        return Err(MicroError::InterfaceCount { count: paths.len() });
    }
    Ok(paths.remove(0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub query_status: u32,
    pub usage_page: u16,
    pub usage: u16,
    pub input_report_length: usize,
    pub output_report_length: usize,
}

pub fn validate_capabilities(caps: Capabilities) -> Result<(), MicroError> {
    if caps.query_status != 0x0011_0000 {
        return Err(MicroError::CapabilityFailure);
    }
    if caps.usage_page != USAGE_PAGE
        || caps.usage != USAGE
        || caps.input_report_length != REPORT_SIZE
        || caps.output_report_length != REPORT_SIZE
    {
        return Err(MicroError::CapabilityLayout);
    }
    Ok(())
}

pub fn validate_report(report: &[u8]) -> Result<(), MicroError> {
    if report.len() == REPORT_SIZE {
        Ok(())
    } else {
        Err(MicroError::ReportSize)
    }
}

pub fn validate_write(count: usize) -> Result<(), MicroError> {
    if count == REPORT_SIZE {
        Ok(())
    } else {
        Err(MicroError::ShortWrite { count })
    }
}

fn separator(byte: u8) -> bool {
    matches!(byte, b'\\' | b'/')
}

fn unc_anchor_end(path: &str, server_start: usize) -> Option<usize> {
    let bytes = path.as_bytes();
    if separator(*bytes.get(server_start)?) {
        return None;
    }
    let server_end = server_start
        + bytes[server_start..]
            .iter()
            .position(|byte| separator(*byte))?;
    let share_start = server_end + 1;
    if separator(*bytes.get(share_start)?) {
        return None;
    }
    Some(
        bytes[share_start..]
            .iter()
            .position(|byte| separator(*byte))
            .map_or(bytes.len(), |offset| share_start + offset),
    )
}

fn windows_basename(path: &str) -> Option<&str> {
    let bytes = path.as_bytes();
    let drive = |offset: usize| {
        bytes.get(offset).is_some_and(u8::is_ascii_alphabetic)
            && bytes.get(offset + 1) == Some(&b':')
    };
    let has_unc_prefix =
        bytes.len() >= 3 && separator(bytes[0]) && separator(bytes[1]) && !separator(bytes[2]);
    let start = if has_unc_prefix {
        let extended = bytes.get(2) == Some(&b'?') && bytes.get(3).is_some_and(|b| separator(*b));
        if extended && drive(4) {
            6
        } else if extended
            && bytes.get(4..7) == Some(b"UNC")
            && bytes.get(7).is_some_and(|b| separator(*b))
        {
            unc_anchor_end(path, 8)
                .or_else(|| unc_anchor_end(path, 2))
                .unwrap_or(0)
        } else {
            unc_anchor_end(path, 2).unwrap_or(0)
        }
    } else if drive(0) {
        2
    } else {
        0
    };
    path[start..]
        .split(['\\', '/'])
        .rfind(|part| !part.is_empty() && *part != ".")
}

pub fn conflicting_processes<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    paths
        .into_iter()
        .filter_map(windows_basename)
        .map(str::to_lowercase)
        .filter(|name| name == "chatgpt.exe" || name == "input.exe")
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn validate_no_competing_processes<'a>(
    paths: impl IntoIterator<Item = &'a str>,
) -> Result<(), MicroError> {
    let names = conflicting_processes(paths);
    if names.is_empty() {
        Ok(())
    } else {
        Err(MicroError::CompetingOwners { names })
    }
}
