use super::{
    ContextMatch, DesktopContext, ErrorKind, ForegroundEvidence, MonotonicStamp, Observation,
    PlatformError, PlatformResult, ProcessEvidence, ProcessIdentity,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const PROBE_MAX_AGE: Duration = Duration::from_secs(12);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRequest {
    pub target: ProcessIdentity,
    pub requested_at: MonotonicStamp,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiRequest {
    pub process: ProcessRequest,
    pub window: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgeError {
    ClockMismatch,
    StaleState,
}

pub fn elapsed_ticks(earlier: MonotonicStamp, later: MonotonicStamp) -> Result<u64, AgeError> {
    if earlier.epoch.is_nil()
        || earlier.epoch != later.epoch
        || earlier.frequency == 0
        || earlier.frequency != later.frequency
        || later.ticks < earlier.ticks
    {
        return Err(AgeError::ClockMismatch);
    }
    Ok(later.ticks - earlier.ticks)
}

pub fn check_age(
    earlier: MonotonicStamp,
    later: MonotonicStamp,
    maximum: Duration,
) -> Result<(), AgeError> {
    let ticks = elapsed_ticks(earlier, later)?;
    if ticks <= 1 {
        return Err(AgeError::ClockMismatch);
    }
    let elapsed_scaled = u128::from(ticks) * 1_000_000_000;
    let bound_scaled = maximum
        .as_nanos()
        .saturating_mul(u128::from(earlier.frequency));
    if elapsed_scaled > bound_scaled {
        return Err(AgeError::StaleState);
    }
    Ok(())
}

pub fn check_probe_interval(
    request: MonotonicStamp,
    start: MonotonicStamp,
    finish: MonotonicStamp,
    received: MonotonicStamp,
) -> Result<(), AgeError> {
    check_age(request, start, PROBE_MAX_AGE)?;
    elapsed_ticks(start, finish)?;
    check_age(finish, received, PROBE_MAX_AGE)?;
    check_age(request, received, PROBE_MAX_AGE)
}

pub fn validate_process_request(
    request: &ProcessRequest,
    now: MonotonicStamp,
) -> PlatformResult<()> {
    if request.target.pid == 0 || request.target.creation_time_100ns == 0 {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "process_request_identity",
        ));
    }
    match check_age(request.requested_at, now, PROBE_MAX_AGE) {
        Ok(()) => Ok(()),
        Err(AgeError::ClockMismatch) => Err(PlatformError::new(
            ErrorKind::InvalidData,
            "process_request_clock",
        )),
        Err(AgeError::StaleState) => Err(PlatformError::new(
            ErrorKind::InvalidData,
            "process_request_expired",
        )),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct TokenContext {
    pub session_id: u32,
    pub integrity_rid: u32,
    pub elevated: bool,
    pub ui_access: bool,
    pub app_container: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SecurityEvidence {
    pub same_user: bool,
    pub same_logon: bool,
    pub observer: TokenContext,
    pub target: TokenContext,
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct TokenIds {
    pub token: (u32, i32),
    pub modified: (u32, i32),
    pub authentication: (u32, i32),
}

#[cfg(any(windows, test))]
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct TokenSnapshot {
    pub ids: TokenIds,
    pub sid: Vec<u8>,
    pub context: TokenContext,
}

#[cfg(any(windows, test))]
pub(crate) fn stable_security(
    observer_first: TokenSnapshot,
    target_first: TokenSnapshot,
    observer_second: TokenSnapshot,
    target_second: TokenSnapshot,
) -> PlatformResult<SecurityEvidence> {
    if observer_first != observer_second || target_first != target_second {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "token_context",
        ));
    }
    Ok(SecurityEvidence {
        same_user: observer_first.sid == target_first.sid,
        same_logon: observer_first.ids.authentication == target_first.ids.authentication,
        observer: observer_first.context,
        target: target_first.context,
    })
}

pub fn match_security(
    expected_session: u32,
    security: &Observation<SecurityEvidence>,
) -> ContextMatch {
    fn acceptable(token: TokenContext, session: u32) -> bool {
        token.session_id == session
            && token.integrity_rid == 0x2000
            && !token.elevated
            && !token.ui_access
            && !token.app_container
    }
    match security {
        Observation::Unavailable(_) => ContextMatch::Unavailable,
        Observation::Observed(value)
            if expected_session != 0
                && value.same_user
                && value.same_logon
                && acceptable(value.observer, expected_session)
                && acceptable(value.target, expected_session) =>
        {
            ContextMatch::Match
        }
        Observation::Observed(_) => ContextMatch::UnsafeContext,
    }
}

pub fn is_drive_absolute_image_path(path: &[u16]) -> bool {
    path.len() >= 4
        && path.len() < 32_768
        && (path[0] as u8).is_ascii_alphabetic()
        && path[0] <= u16::from(u8::MAX)
        && path[1] == u16::from(b':')
        && path[2] == u16::from(b'\\')
        && !path.contains(&0)
}

#[cfg(any(windows, test))]
pub(crate) fn bounded_sid_bytes(buffer: &[u8], offset: usize) -> PlatformResult<&[u8]> {
    const OP: &str = "token_sid";
    if !offset.is_multiple_of(4) || offset.checked_add(8).is_none_or(|end| end > buffer.len()) {
        return Err(PlatformError::new(ErrorKind::InvalidData, OP));
    }
    let count = buffer[offset + 1] as usize;
    if count > 15 {
        return Err(PlatformError::new(ErrorKind::InvalidData, OP));
    }
    let end = offset
        .checked_add(8 + 4 * count)
        .filter(|end| *end <= buffer.len())
        .ok_or_else(|| PlatformError::new(ErrorKind::InvalidData, OP))?;
    Ok(&buffer[offset..end])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Unverified,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ImageFileEvidence {
    pub same_file_as_observer: bool,
    pub path_file_stable_during_probe: bool,
    pub provenance: Provenance,
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct ImageFileRecord {
    pub attributes: u32,
    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
    pub volume_serial: u32,
    pub size_high: u32,
    pub size_low: u32,
    pub link_count: u32,
    pub index_high: u32,
    pub index_low: u32,
}

#[cfg(any(windows, test))]
pub(crate) trait ImageFileBackend {
    type Process;
    type File;
    fn check_running(&mut self, process: &Self::Process) -> PlatformResult<()>;
    fn image_path(&mut self, process: &Self::Process) -> PlatformResult<Vec<u16>>;
    fn open_attributes(&mut self, path: &[u16]) -> PlatformResult<Self::File>;
    fn information(&mut self, file: &Self::File) -> PlatformResult<ImageFileRecord>;
}

#[cfg(any(windows, test))]
pub(crate) fn collect_image_file<B: ImageFileBackend>(
    backend: &mut B,
    observer: &B::Process,
    target: &B::Process,
) -> PlatformResult<ImageFileEvidence> {
    backend.check_running(observer)?;
    backend.check_running(target)?;
    let observer_path = backend.image_path(observer)?;
    let target_path = backend.image_path(target)?;
    if !is_drive_absolute_image_path(&observer_path) || !is_drive_absolute_image_path(&target_path)
    {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "image_file_path",
        ));
    }
    if observer_path != target_path {
        return Err(PlatformError::new(
            ErrorKind::UnsupportedContext,
            "external_image_provenance_unqualified",
        ));
    }
    let observer_file = backend.open_attributes(&observer_path)?;
    let target_file = backend.open_attributes(&target_path)?;
    let observer_first = backend.information(&observer_file)?;
    let target_first = backend.information(&target_file)?;
    for record in [observer_first, target_first] {
        if record.link_count == 0 || (record.index_high == 0 && record.index_low == 0) {
            return Err(PlatformError::new(
                ErrorKind::InvalidData,
                "image_file_identity",
            ));
        }
    }
    let same_file_as_observer = observer_first.volume_serial == target_first.volume_serial
        && observer_first.index_high == target_first.index_high
        && observer_first.index_low == target_first.index_low;
    if backend.image_path(observer)? != observer_path || backend.image_path(target)? != target_path
    {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "image_file_path",
        ));
    }
    if backend.information(&observer_file)? != observer_first
        || backend.information(&target_file)? != target_first
    {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "image_file_identity",
        ));
    }
    backend.check_running(observer)?;
    backend.check_running(target)?;
    Ok(ImageFileEvidence {
        same_file_as_observer,
        path_file_stable_during_probe: true,
        provenance: Provenance::Unverified,
    })
}

/// A held value must own a noninheritable process handle until dropped.
pub trait ProcessProbeBackend {
    type Held;
    fn stamp(&mut self) -> PlatformResult<MonotonicStamp>;
    fn current_pid(&mut self) -> u32;
    fn hold(&mut self, pid: u32) -> PlatformResult<Self::Held>;
    fn evidence(&mut self, process: &Self::Held) -> PlatformResult<ProcessEvidence>;
    fn check_running(&mut self, process: &Self::Held) -> PlatformResult<()>;
    fn security(
        &mut self,
        observer: &Self::Held,
        target: &Self::Held,
    ) -> PlatformResult<SecurityEvidence>;
}

fn hold_target<B: ProcessProbeBackend>(
    backend: &mut B,
    request: &ProcessRequest,
) -> PlatformResult<(B::Held, ProcessEvidence)> {
    let held = backend.hold(request.target.pid)?;
    let evidence = backend.evidence(&held)?;
    if evidence.identity != request.target {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "exact_process_lifetime",
        ));
    }
    Ok((held, evidence))
}

fn hold_observer<B: ProcessProbeBackend>(
    backend: &mut B,
) -> PlatformResult<(B::Held, ProcessEvidence)> {
    let pid = backend.current_pid();
    let held = backend.hold(pid)?;
    let evidence = backend.evidence(&held)?;
    if evidence.identity.pid != pid {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "observer_process_lifetime",
        ));
    }
    Ok((held, evidence))
}

pub trait ExactProcessBackend: ProcessProbeBackend {
    fn image_file(
        &mut self,
        observer: &Self::Held,
        target: &Self::Held,
    ) -> PlatformResult<ImageFileEvidence>;
    fn foreground(&mut self) -> PlatformResult<ForegroundEvidence>;
    fn desktop(&mut self) -> PlatformResult<DesktopContext>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExactProcessReport {
    pub target: ProcessEvidence,
    pub observer: ProcessIdentity,
    pub security: Observation<SecurityEvidence>,
    pub image_file: Observation<ImageFileEvidence>,
    pub foreground: Observation<ForegroundEvidence>,
    pub desktop: Observation<DesktopContext>,
    pub started_at: MonotonicStamp,
    pub finished_at: MonotonicStamp,
    pub read_only: bool,
    pub command_ready: bool,
    pub w2_qualified: bool,
}

pub fn collect_exact_process<B: ExactProcessBackend>(
    backend: &mut B,
    request: &ProcessRequest,
) -> PlatformResult<ExactProcessReport> {
    let started_at = backend.stamp()?;
    validate_process_request(request, started_at)?;
    let (target, target_evidence) = hold_target(backend, request)?;
    let (observer, observer_evidence) = hold_observer(backend)?;
    let security = backend.security(&observer, &target).into();
    let image_file = backend.image_file(&observer, &target).into();
    let foreground = backend.foreground().into();
    let desktop = backend.desktop().into();
    backend.check_running(&observer)?;
    let finished_at = backend.stamp()?;
    elapsed_ticks(started_at, finished_at)
        .map_err(|_| PlatformError::new(ErrorKind::InvalidData, "process_probe_clock"))?;
    validate_process_request(request, finished_at)?;
    backend.check_running(&target)?;
    Ok(ExactProcessReport {
        target: target_evidence,
        observer: observer_evidence.identity,
        security,
        image_file,
        foreground,
        desktop,
        started_at,
        finished_at,
        read_only: true,
        command_ready: false,
        w2_qualified: false,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiNodeMetadata {
    pub control_type: i32,
    pub keyboard_focus: bool,
    pub offscreen: bool,
}

/// Implementations must confine all automation objects to one windowless MTA thread.
pub trait UiBackend: ProcessProbeBackend {
    type Element;
    fn is_root_window(&mut self, window: u64) -> PlatformResult<bool>;
    fn window_owner(&mut self, window: u64) -> PlatformResult<(u32, u32)>;
    fn is_foreground(&mut self, window: u64) -> PlatformResult<bool>;
    fn root_element(&mut self, window: u64) -> PlatformResult<Self::Element>;
    fn root_native_window(&mut self, root: &Self::Element) -> PlatformResult<u64>;
    fn process_id(&mut self, node: &Self::Element) -> PlatformResult<i32>;
    fn metadata(&mut self, node: &Self::Element) -> PlatformResult<UiNodeMetadata>;
    fn first_child(&mut self, node: &Self::Element) -> PlatformResult<Option<Self::Element>>;
    fn next_sibling(&mut self, node: &Self::Element) -> PlatformResult<Option<Self::Element>>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UiStructureReport {
    pub target: ProcessIdentity,
    pub helper: ProcessIdentity,
    pub window: u64,
    pub visited_nodes: u32,
    pub text_nodes: u32,
    pub focused_nodes: u32,
    pub offscreen_nodes: u32,
    pub foreground_at_both_checks: bool,
    pub relationship: &'static str,
    pub started_at: MonotonicStamp,
    pub finished_at: MonotonicStamp,
    pub read_only: bool,
    pub command_ready: bool,
    pub w2_qualified: bool,
}

pub fn collect_ui_structure<B: UiBackend>(
    backend: &mut B,
    request: &UiRequest,
) -> PlatformResult<UiStructureReport> {
    if request.process.target.pid == backend.current_pid() {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "helper_must_not_be_target",
        ));
    }
    let started_at = backend.stamp()?;
    validate_process_request(&request.process, started_at)?;
    let (target, target_evidence) = hold_target(backend, &request.process)?;
    let (helper, helper_evidence) = hold_observer(backend)?;
    let security = backend.security(&helper, &target)?;
    if match_security(
        request.process.target.session_id,
        &Observation::Observed(security),
    ) != ContextMatch::Match
    {
        return Err(PlatformError::new(
            ErrorKind::UnsupportedContext,
            "helper_security_context",
        ));
    }
    validate_process_request(&request.process, backend.stamp()?)?;
    if request.window == 0
        || usize::try_from(request.window).is_err()
        || !backend.is_root_window(request.window)?
    {
        return Err(PlatformError::new(ErrorKind::InvalidData, "helper_window"));
    }
    let owner = backend.window_owner(request.window)?;
    if owner.0 == 0 || owner.1 != request.process.target.pid {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "helper_window_owner",
        ));
    }
    let first_foreground = backend.is_foreground(request.window)?;
    let root = backend.root_element(request.window)?;
    if backend.root_native_window(&root)? != request.window {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "uia_root_window",
        ));
    }

    let mut report = UiStructureReport {
        target: target_evidence.identity,
        helper: helper_evidence.identity,
        window: request.window,
        visited_nodes: 0,
        text_nodes: 0,
        focused_nodes: 0,
        offscreen_nodes: 0,
        foreground_at_both_checks: false,
        relationship: "unproven",
        started_at,
        finished_at: started_at,
        read_only: true,
        command_ready: false,
        w2_qualified: false,
    };
    let mut stack = vec![(root, 0usize)];
    while let Some((node, depth)) = stack.pop() {
        if depth > 16 || report.visited_nodes == 256 {
            return Err(PlatformError::new(
                ErrorKind::LimitExceeded,
                "uia_tree_bound",
            ));
        }
        validate_process_request(&request.process, backend.stamp()?)?;
        let process_id = backend.process_id(&node)?;
        if process_id <= 0 || process_id as u32 != request.process.target.pid {
            return Err(PlatformError::new(
                ErrorKind::ObservationChanged,
                "uia_element_process",
            ));
        }
        let metadata = backend.metadata(&node)?;
        report.visited_nodes += 1;
        report.text_nodes += u32::from(metadata.control_type == 50_020);
        report.focused_nodes += u32::from(metadata.keyboard_focus);
        report.offscreen_nodes += u32::from(metadata.offscreen);
        if depth > 0
            && let Some(sibling) = backend.next_sibling(&node)?
        {
            stack.push((sibling, depth));
        }
        if let Some(child) = backend.first_child(&node)? {
            stack.push((child, depth + 1));
        }
    }
    if backend.window_owner(request.window)? != owner {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "helper_window_owner",
        ));
    }
    backend.check_running(&target)?;
    let last_foreground = backend.is_foreground(request.window)?;
    report.finished_at = backend.stamp()?;
    elapsed_ticks(started_at, report.finished_at)
        .map_err(|_| PlatformError::new(ErrorKind::InvalidData, "uia_probe_clock"))?;
    validate_process_request(&request.process, report.finished_at)?;
    report.foreground_at_both_checks = first_foreground && last_foreground;
    Ok(report)
}

pub trait ConsoleAttachment {
    type Held;
    fn process_ids(&mut self) -> PlatformResult<Vec<u32>>;
    fn console_window(&mut self) -> PlatformResult<Option<u64>>;
    fn hold(&mut self, pid: u32) -> PlatformResult<Self::Held>;
    fn evidence(&mut self, process: &Self::Held) -> PlatformResult<ProcessEvidence>;
    fn check_running(&mut self, process: &Self::Held) -> PlatformResult<()>;
    /// Detach the owned attachment explicitly; drop must attempt best-effort detach on error.
    fn detach(self) -> PlatformResult<()>;
}

pub trait ConsoleBackend: ProcessProbeBackend {
    type Attachment: ConsoleAttachment<Held = Self::Held>;
    fn existing_console_window(&mut self) -> PlatformResult<Option<u64>>;
    fn attach(&mut self, target_pid: u32) -> PlatformResult<Self::Attachment>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConsoleMembershipReport {
    pub target: ProcessIdentity,
    pub helper: ProcessIdentity,
    pub members: Vec<ProcessIdentity>,
    pub console_window: Option<u64>,
    pub relationship: &'static str,
    pub started_at: MonotonicStamp,
    pub finished_at: MonotonicStamp,
    pub read_only: bool,
    pub command_ready: bool,
    pub w2_qualified: bool,
}

fn sorted_console_ids(mut ids: Vec<u32>) -> PlatformResult<Vec<u32>> {
    if ids.len() > 128 {
        return Err(PlatformError::new(
            ErrorKind::LimitExceeded,
            "console_process_list",
        ));
    }
    if ids.is_empty() {
        return Err(PlatformError::new(
            ErrorKind::NativeFailure,
            "GetConsoleProcessList",
        ));
    }
    ids.sort_unstable();
    if ids[0] == 0 || ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "console_process_list",
        ));
    }
    Ok(ids)
}

pub fn collect_console_membership<B: ConsoleBackend>(
    backend: &mut B,
    request: &ProcessRequest,
) -> PlatformResult<ConsoleMembershipReport> {
    if request.target.pid == backend.current_pid() {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "helper_must_not_be_target",
        ));
    }
    let started_at = backend.stamp()?;
    validate_process_request(request, started_at)?;
    let (target, target_evidence) = hold_target(backend, request)?;
    let (helper, helper_evidence) = hold_observer(backend)?;
    let security = backend.security(&helper, &target)?;
    if match_security(request.target.session_id, &Observation::Observed(security))
        != ContextMatch::Match
    {
        return Err(PlatformError::new(
            ErrorKind::UnsupportedContext,
            "helper_security_context",
        ));
    }
    validate_process_request(request, backend.stamp()?)?;
    if backend.existing_console_window()?.is_some() {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "helper_already_has_console",
        ));
    }
    let mut attachment = backend.attach(request.target.pid)?;
    let first = sorted_console_ids(attachment.process_ids()?)?;
    if !first.contains(&request.target.pid) || !first.contains(&helper_evidence.identity.pid) {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "console_membership",
        ));
    }
    let console_window = attachment.console_window()?;
    let mut held = Vec::with_capacity(first.len());
    let mut members = Vec::with_capacity(first.len());
    for pid in &first {
        let process = attachment.hold(*pid)?;
        let evidence = attachment.evidence(&process)?;
        if evidence.identity.pid != *pid {
            return Err(PlatformError::new(
                ErrorKind::ObservationChanged,
                "console_member_lifetime",
            ));
        }
        members.push(evidence.identity);
        held.push(process);
    }
    if sorted_console_ids(attachment.process_ids()?)? != first
        || attachment.console_window()? != console_window
    {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "console_membership",
        ));
    }
    for process in &held {
        attachment.check_running(process)?;
    }
    if !members.contains(&target_evidence.identity) || !members.contains(&helper_evidence.identity)
    {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "console_member_lifetime",
        ));
    }
    backend.check_running(&target)?;
    attachment.detach()?;
    let finished_at = backend.stamp()?;
    elapsed_ticks(started_at, finished_at)
        .map_err(|_| PlatformError::new(ErrorKind::InvalidData, "console_probe_clock"))?;
    validate_process_request(request, finished_at)?;
    Ok(ConsoleMembershipReport {
        target: target_evidence.identity,
        helper: helper_evidence.identity,
        members,
        console_window,
        relationship: "unproven",
        started_at,
        finished_at,
        read_only: true,
        command_ready: false,
        w2_qualified: false,
    })
}
