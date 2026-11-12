use std::{fmt, str::FromStr};

use serde::{Serialize, Serializer};

use crate::domain::{ClockSample, NativeProcessIdentity, RuntimeEpoch};

use super::{
    clock::sample_monotonic,
    contracts::{Observation, PlatformError, PlatformErrorKind},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutableClass {
    RecognizedCli,
    RecognizedNativeTerminalHost,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProcessEvidence {
    pub identity: NativeProcessIdentity,
    pub executable_class: ExecutableClass,
    pub observed_at: ClockSample,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProcessCandidate {
    pub snapshot_pid: u32,
    pub snapshot_class: ExecutableClass,
    pub lifetime: Observation<NativeProcessIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProcessInventory {
    examined: u32,
    candidates: Vec<ProcessCandidate>,
}

impl ProcessInventory {
    pub fn new(examined: u32, candidates: Vec<ProcessCandidate>) -> Result<Self, PlatformError> {
        if candidates.len() > examined as usize
            || candidates
                .iter()
                .any(|candidate| candidate.snapshot_class == ExecutableClass::Other)
        {
            return Err(PlatformError::new(
                PlatformErrorKind::InvalidData,
                "process_inventory",
                None,
            ));
        }
        Ok(Self {
            examined,
            candidates,
        })
    }

    pub fn examined(&self) -> u32 {
        self.examined
    }

    pub fn candidates(&self) -> &[ProcessCandidate] {
        &self.candidates
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ForegroundEvidence {
    pub window: u64,
    pub owner_thread_id: u32,
    pub owner: ProcessEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DesktopContext {
    pub session_id: u32,
    pub active_console_session_id: Option<u32>,
    pub active: bool,
    pub local: bool,
    pub interactive: bool,
    pub default_desktop: bool,
    pub observed_at: ClockSample,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidComPort;

impl fmt::Display for InvalidComPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid COM port")
    }
}

impl std::error::Error for InvalidComPort {}

/// Stores only a validated port number, never a raw interface path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ComPort(u16);

impl FromStr for ComPort {
    type Err = InvalidComPort;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = value.as_bytes();
        if !(4..=8).contains(&bytes.len())
            || !bytes[..3].eq_ignore_ascii_case(b"COM")
            || bytes[3] == b'0'
            || !bytes[3..].iter().all(u8::is_ascii_digit)
        {
            return Err(InvalidComPort);
        }
        let number = value[3..].parse::<u16>().map_err(|_| InvalidComPort)?;
        if number == 0 {
            return Err(InvalidComPort);
        }
        Ok(Self(number))
    }
}

impl fmt::Display for ComPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "COM{}", self.0)
    }
}

impl Serialize for ComPort {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialSelection {
    NotFound,
    Unique,
    Ambiguous,
    Incomplete,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SerialDiscovery {
    selection: SerialSelection,
    present_count: u32,
    matching_count: u32,
    ports: Vec<ComPort>,
    selected: Option<ComPort>,
}

impl SerialDiscovery {
    /// Invalid or absent raw ports are discarded by the observation adapter.
    pub fn from_validated(
        present_count: u32,
        matching_count: u32,
        ports: Vec<ComPort>,
        enumeration_complete: bool,
    ) -> Result<Self, PlatformError> {
        let valid_count = u32::try_from(ports.len()).map_err(|_| {
            PlatformError::new(PlatformErrorKind::LimitExceeded, "serial_discovery", None)
        })?;
        if matching_count > present_count || valid_count > matching_count {
            return Err(PlatformError::new(
                PlatformErrorKind::InvalidData,
                "serial_discovery",
                None,
            ));
        }
        let selection = if !enumeration_complete || valid_count != matching_count {
            SerialSelection::Incomplete
        } else {
            match matching_count {
                0 => SerialSelection::NotFound,
                1 => SerialSelection::Unique,
                _ => SerialSelection::Ambiguous,
            }
        };
        let selected = if selection == SerialSelection::Unique {
            ports.first().copied()
        } else {
            None
        };
        Ok(Self {
            selection,
            present_count,
            matching_count,
            ports,
            selected,
        })
    }

    pub fn selection(&self) -> SerialSelection {
        self.selection
    }

    pub fn selected(&self) -> Option<ComPort> {
        self.selected
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticSections {
    pub clock: Observation<ClockSample>,
    pub current_process: Observation<ProcessEvidence>,
    pub processes: Observation<ProcessInventory>,
    pub foreground: Observation<ForegroundEvidence>,
    pub desktop: Observation<DesktopContext>,
    pub serial: Observation<SerialDiscovery>,
}

const LIMITATIONS: &[&str] = &[
    "Enumeration is not terminal or device qualification.",
    "Provenance, user/logon ownership, and integrity compatibility are not verified.",
    "Pane/tab identity, split-pane exclusion, and pane-to-console ownership are unproven.",
    "No UI Automation, UI text, console attachment, focus change, input, or serial opening is performed.",
    "Desktop, session, and counter samples are snapshots, not transition subscriptions or physical timing proof.",
    "Readiness and native or physical qualification remain false.",
];

#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticReport {
    schema: u8,
    qualification: &'static str,
    read_only: bool,
    command_ready: bool,
    w2_qualified: bool,
    #[serde(flatten)]
    pub sections: DiagnosticSections,
    limitations: &'static [&'static str],
}

impl DiagnosticReport {
    pub fn from_sections(sections: DiagnosticSections) -> Self {
        Self {
            schema: 1,
            qualification: "foundation_only",
            read_only: true,
            command_ready: false,
            w2_qualified: false,
            sections,
            limitations: LIMITATIONS,
        }
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    pub fn command_ready(&self) -> bool {
        self.command_ready
    }

    pub fn w2_qualified(&self) -> bool {
        self.w2_qualified
    }
}

/// Collect only the portable clock; Windows observations are a separate block.
pub fn portable_diagnostic_report(epoch: RuntimeEpoch) -> DiagnosticReport {
    let clock = match sample_monotonic(epoch) {
        Ok(sample) => Observation::Observed(sample),
        Err(error) => Observation::Unavailable(error),
    };
    #[cfg(windows)]
    let unavailable = PlatformError::new(
        PlatformErrorKind::UnsupportedContext,
        "windows_observations_not_integrated",
        None,
    );
    #[cfg(not(windows))]
    let unavailable = PlatformError::new(
        PlatformErrorKind::UnsupportedPlatform,
        "windows_platform",
        None,
    );
    DiagnosticReport::from_sections(DiagnosticSections {
        clock,
        current_process: Observation::Unavailable(unavailable.clone()),
        processes: Observation::Unavailable(unavailable.clone()),
        foreground: Observation::Unavailable(unavailable.clone()),
        desktop: Observation::Unavailable(unavailable.clone()),
        serial: Observation::Unavailable(unavailable),
    })
}
