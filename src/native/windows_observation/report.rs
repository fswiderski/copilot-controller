use super::{
    ClockSample, DesktopContext, ExecutableNames, ForegroundEvidence, Observation, ProcessEvidence,
    ProcessInventory, SerialSelection, observe_clock, observe_current_process, observe_desktop,
    observe_foreground, observe_process_inventory, observe_serial,
};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
pub struct OrdinaryDiagnosticReport {
    pub schema: u32,
    pub read_only: bool,
    pub command_ready: bool,
    pub w2_qualified: bool,
    pub clock: Observation<ClockSample>,
    pub current_process: Observation<ProcessEvidence>,
    pub processes: Observation<ProcessInventory>,
    pub foreground: Observation<ForegroundEvidence>,
    pub desktop: Observation<DesktopContext>,
    pub serial: Observation<SerialSelection>,
    pub limitations: &'static [&'static str],
}

pub fn ordinary_diagnostic(epoch: Uuid, names: ExecutableNames<'_>) -> OrdinaryDiagnosticReport {
    OrdinaryDiagnosticReport {
        schema: 1,
        read_only: true,
        command_ready: false,
        w2_qualified: false,
        clock: observe_clock(epoch).into(),
        current_process: observe_current_process(epoch, names),
        processes: observe_process_inventory(epoch, names),
        foreground: observe_foreground(epoch, names),
        desktop: observe_desktop(epoch),
        serial: observe_serial(),
        limitations: &[
            "Enumeration does not qualify a terminal or device.",
            "Provenance, user/logon ownership and integrity compatibility are not verified here.",
            "Pane/tab identity, split-pane exclusion and pane-to-console ownership are unproven.",
            "No UI automation, console attachment, text, focus action, input or serial opening is performed.",
            "Desktop, session and clock samples are snapshots, not transition subscriptions or physical timing proof.",
        ],
    }
}
