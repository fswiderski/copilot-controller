use super::{
    DesktopContext, ErrorKind, ExecutableClass, ForegroundEvidence, MonotonicStamp, Observation,
    PlatformError, PlatformResult, ProcessEvidence, ProcessIdentity,
};
use serde::Serialize;

/// Deployment-specific executable basenames, never paths or substring patterns.
#[derive(Clone, Copy, Debug)]
pub struct ExecutableNames<'a> {
    cli: &'a str,
    native_host: &'a str,
}

impl<'a> ExecutableNames<'a> {
    pub fn new(cli: &'a str, native_host: &'a str) -> PlatformResult<Self> {
        fn valid(s: &str) -> bool {
            !s.is_empty()
                && s.is_ascii()
                && s.bytes().all(|b| {
                    !b.is_ascii_whitespace() && !b.is_ascii_control() && b != b'/' && b != b'\\'
                })
        }
        if !valid(cli) || !valid(native_host) || cli.eq_ignore_ascii_case(native_host) {
            return Err(PlatformError::new(
                ErrorKind::InvalidData,
                "executable_basenames",
            ));
        }
        Ok(Self { cli, native_host })
    }

    pub fn cli_basename(&self) -> &'a str {
        self.cli
    }

    pub fn native_host_basename(&self) -> &'a str {
        self.native_host
    }

    pub fn classify(&self, basename: &str) -> ExecutableClass {
        let value = basename.as_bytes();
        if value.eq_ignore_ascii_case(self.native_host.as_bytes()) {
            return ExecutableClass::NativeHost;
        }
        let cli = self.cli.as_bytes();
        if value.eq_ignore_ascii_case(cli) {
            return ExecutableClass::Cli;
        }
        if value.len() <= cli.len() + 5 || !value[..cli.len()].eq_ignore_ascii_case(cli) {
            return ExecutableClass::Other;
        }
        let suffix = &value[cli.len()..];
        if suffix.len() < 7 || !suffix[..5].eq_ignore_ascii_case(b".old-") {
            return ExecutableClass::Other;
        }
        let numbers = &suffix[5..];
        let Some(separator) = numbers.iter().position(|b| *b == b'-') else {
            return ExecutableClass::Other;
        };
        if separator == 0
            || separator + 1 == numbers.len()
            || !numbers[..separator].iter().all(u8::is_ascii_digit)
            || !numbers[separator + 1..].iter().all(u8::is_ascii_digit)
        {
            return ExecutableClass::Other;
        }
        ExecutableClass::Cli
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetMatch {
    Match,
    WrongTarget,
    Unavailable,
}

pub fn match_process(
    expected: ProcessIdentity,
    observed: &Observation<ProcessEvidence>,
) -> TargetMatch {
    match observed {
        Observation::Unavailable(_) => TargetMatch::Unavailable,
        Observation::Observed(evidence)
            if expected.pid != 0
                && expected.creation_time_100ns != 0
                && expected == evidence.identity =>
        {
            TargetMatch::Match
        }
        Observation::Observed(_) => TargetMatch::WrongTarget,
    }
}

pub fn match_foreground(
    expected_window: u64,
    expected: ProcessIdentity,
    observed: &Observation<ForegroundEvidence>,
) -> TargetMatch {
    match observed {
        Observation::Unavailable(_) => TargetMatch::Unavailable,
        Observation::Observed(evidence)
            if expected_window != 0
                && expected.pid != 0
                && expected.creation_time_100ns != 0
                && evidence.owner_thread_id != 0
                && evidence.window == expected_window
                && evidence.owner.identity == expected
                && evidence.owner.class == ExecutableClass::NativeHost =>
        {
            TargetMatch::Match
        }
        Observation::Observed(_) => TargetMatch::WrongTarget,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMatch {
    Match,
    UnsafeContext,
    Unavailable,
}

pub fn match_desktop(
    expected_session: u32,
    observed: &Observation<DesktopContext>,
) -> ContextMatch {
    match observed {
        Observation::Unavailable(_) => ContextMatch::Unavailable,
        Observation::Observed(context)
            if expected_session != 0
                && context.process_session_id == expected_session
                && context.active_console_session_id == Some(expected_session)
                && context.active
                && context.local
                && context.interactive_station
                && context.thread_default_desktop
                && context.input_default_desktop =>
        {
            ContextMatch::Match
        }
        Observation::Observed(_) => ContextMatch::UnsafeContext,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProcessCandidate {
    pub snapshot_pid: u32,
    pub snapshot_class: ExecutableClass,
    pub inspection: Observation<ProcessEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProcessInventory {
    pub examined_count: u32,
    pub candidates: Vec<ProcessCandidate>,
}

/// A snapshot backend must return strictly decoded, terminated basenames.
pub trait ProcessInventoryBackend {
    fn next_process(&mut self) -> PlatformResult<Option<(u32, String)>>;
    fn inspect(&mut self, pid: u32) -> PlatformResult<ProcessEvidence>;
}

pub fn collect_process_inventory<B: ProcessInventoryBackend>(
    backend: &mut B,
    names: ExecutableNames<'_>,
) -> PlatformResult<ProcessInventory> {
    let mut examined_count = 0u32;
    let mut candidates = Vec::new();
    while let Some((pid, basename)) = backend.next_process()? {
        if examined_count == 16_384 {
            return Err(PlatformError::new(
                ErrorKind::LimitExceeded,
                "process_enumeration",
            ));
        }
        examined_count += 1;
        let class = names.classify(&basename);
        if class == ExecutableClass::Other {
            continue;
        }
        let inspection = match backend.inspect(pid) {
            Ok(evidence) if evidence.class != class || evidence.identity.pid != pid => Err(
                PlatformError::new(ErrorKind::ObservationChanged, "process_snapshot_identity"),
            ),
            other => other,
        };
        candidates.push(ProcessCandidate {
            snapshot_pid: pid,
            snapshot_class: class,
            inspection: inspection.into(),
        });
    }
    Ok(ProcessInventory {
        examined_count,
        candidates,
    })
}

/// `Held` must keep the original process object open until dropped.
pub trait ForegroundBackend {
    type Held;
    fn foreground_window(&mut self) -> PlatformResult<Option<u64>>;
    fn window_owner(&mut self, window: u64) -> PlatformResult<(u32, u32)>;
    fn hold_process(&mut self, pid: u32) -> PlatformResult<Self::Held>;
    fn inspect_held(&mut self, held: &Self::Held) -> PlatformResult<ProcessEvidence>;
    fn check_running(&mut self, held: &Self::Held) -> PlatformResult<()>;
}

pub fn collect_foreground<B: ForegroundBackend>(
    backend: &mut B,
) -> PlatformResult<ForegroundEvidence> {
    let window = backend
        .foreground_window()?
        .filter(|w| *w != 0)
        .ok_or_else(|| PlatformError::new(ErrorKind::NoForeground, "GetForegroundWindow"))?;
    let (thread, pid) = backend.window_owner(window)?;
    if thread == 0 || pid == 0 {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "foreground_window_owner",
        ));
    }
    let held = backend.hold_process(pid)?;
    let owner = backend.inspect_held(&held)?;
    let later_window = backend.foreground_window()?;
    let later_owner = backend.window_owner(window)?;
    if later_window != Some(window) || later_owner != (thread, pid) || owner.identity.pid != pid {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "foreground_window",
        ));
    }
    backend.check_running(&held)?;
    Ok(ForegroundEvidence {
        window,
        owner_thread_id: thread,
        owner,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopSnapshot {
    pub active_console_session_id: Option<u32>,
    pub connection_state: i32,
    pub client_protocol: u16,
    pub interactive_station: bool,
    pub thread_default_desktop: bool,
    pub input_default_desktop: bool,
}

#[cfg(any(windows, test))]
pub(crate) fn object_name_matches(
    units: &[u16],
    bytes: usize,
    expected: &str,
) -> PlatformResult<bool> {
    const OP: &str = "desktop_object_name";
    if bytes > 512 {
        return Err(PlatformError::new(ErrorKind::LimitExceeded, OP));
    }
    if bytes == 0 || !bytes.is_multiple_of(2) || bytes / 2 > units.len() {
        return Err(PlatformError::new(ErrorKind::InvalidData, OP));
    }
    let units = &units[..bytes / 2];
    let end = units
        .iter()
        .position(|u| *u == 0)
        .ok_or_else(|| PlatformError::new(ErrorKind::InvalidData, OP))?;
    if units[end..].iter().any(|u| *u != 0) {
        return Err(PlatformError::new(ErrorKind::InvalidData, OP));
    }
    let name = String::from_utf16(&units[..end])
        .map_err(|_| PlatformError::new(ErrorKind::InvalidData, OP))?;
    Ok(name.eq_ignore_ascii_case(expected))
}

pub trait DesktopBackend {
    fn stamp(&mut self) -> PlatformResult<MonotonicStamp>;
    fn process_session(&mut self) -> PlatformResult<u32>;
    fn snapshot(&mut self, session: u32) -> PlatformResult<DesktopSnapshot>;
}

pub fn collect_desktop<B: DesktopBackend>(backend: &mut B) -> PlatformResult<DesktopContext> {
    let stamp = backend.stamp()?;
    let session = backend.process_session()?;
    let first = backend.snapshot(session)?;
    let second = backend.snapshot(session)?;
    if first != second {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "desktop_session_context",
        ));
    }
    Ok(DesktopContext {
        process_session_id: session,
        active_console_session_id: first.active_console_session_id,
        active: first.connection_state == 0,
        local: first.client_protocol == 0,
        interactive_station: first.interactive_station,
        thread_default_desktop: first.thread_default_desktop,
        input_default_desktop: first.input_default_desktop,
        stamp,
    })
}
