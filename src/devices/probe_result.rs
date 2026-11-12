//! Explicit disclosure of report data omitted by the diagnostic response budget.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OmittedField {
    DeviceStatus,
    Keymap,
    Firmware,
    Candidate,
    Phase,
    Error,
    CleanupError,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseLimit {
    pub omitted_fields: Vec<OmittedField>,
    pub captures_dropped: usize,
    pub inputs_dropped: usize,
    pub serialization_failed: bool,
}
