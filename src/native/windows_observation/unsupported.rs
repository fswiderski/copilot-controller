use super::*;
use uuid::Uuid;

fn unavailable<T>() -> Observation<T> {
    Observation::Unavailable(PlatformError::new(
        ErrorKind::UnsupportedPlatform,
        "windows_platform",
    ))
}

pub fn observe_process(
    _pid: u32,
    _epoch: Uuid,
    _names: ExecutableNames<'_>,
) -> Observation<ProcessEvidence> {
    unavailable()
}

pub fn observe_current_process(
    _epoch: Uuid,
    _names: ExecutableNames<'_>,
) -> Observation<ProcessEvidence> {
    unavailable()
}

pub fn observe_process_inventory(
    _epoch: Uuid,
    _names: ExecutableNames<'_>,
) -> Observation<ProcessInventory> {
    unavailable()
}

pub fn observe_foreground(
    _epoch: Uuid,
    _names: ExecutableNames<'_>,
) -> Observation<ForegroundEvidence> {
    unavailable()
}

pub fn observe_desktop(_epoch: Uuid) -> Observation<DesktopContext> {
    unavailable()
}

pub fn observe_serial() -> Observation<SerialSelection> {
    unavailable()
}

pub fn observe_exact_process(
    _request: &ProcessRequest,
    _names: ExecutableNames<'_>,
) -> Observation<ExactProcessReport> {
    unavailable()
}

pub fn observe_ui_structure(
    _request: &UiRequest,
    _names: ExecutableNames<'_>,
) -> Observation<UiStructureReport> {
    unavailable()
}

pub fn observe_console_membership(
    _request: &ProcessRequest,
    _names: ExecutableNames<'_>,
) -> Observation<ConsoleMembershipReport> {
    unavailable()
}
