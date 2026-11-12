//! Strict, bounded recorded-gesture assignment; candidates are never activated.

use std::{
    io::{self, Read},
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};

use crate::domain::ClockSample;

use super::{
    evidence::DeviceKind,
    input::{
        Intent, LogicalBinding, PhysicalEvent, PhysicalEventKind, RoleError,
        validate_binding_shapes,
    },
    profiles::{
        AKP03_ORIGINAL_PROFILE, AKP03_REV2_PROFILE, CT_PROFILE, ControlKind, MICRO_PROFILE,
        ajazz_original, ajazz_rev2, ct, default_profile, micro,
    },
};

pub const MAX_RECORDED_BYTES: usize = 32_768;
pub const EXPORT_FILENAME: &str = "device-binding.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedAssignment {
    device: DeviceKind,
    intent: Intent,
    adjacency_confirmed: bool,
    events: Vec<PhysicalEvent>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateBlocker {
    CompleteScopeAndNativeInputEvidenceRequired,
    ControlNotNormalizedByProfileAdapter,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Activation {
    CandidateOnly,
}

/// This type can only be produced by strict validation and contains no events.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CandidateAssignment {
    version: u8,
    device: DeviceKind,
    profile: String,
    binding: LogicalBinding,
    activation: Activation,
    blocking_reasons: Vec<CandidateBlocker>,
    command_ready: bool,
}

impl CandidateAssignment {
    pub fn device(&self) -> DeviceKind {
        self.device
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    pub fn binding(&self) -> &LogicalBinding {
        &self.binding
    }

    pub fn blocking_reasons(&self) -> &[CandidateBlocker] {
        &self.blocking_reasons
    }
}

#[derive(Debug)]
pub enum AssignmentError {
    Read(io::Error),
    TooLarge,
    InvalidJson(serde_json::Error),
    ExactOneGestureRequired,
    ChangedGeneration,
    ClockMismatch,
    HoldTooLong,
    WrongControlKind,
    ProfileMismatch,
    Role(RoleError),
}

impl std::fmt::Display for AssignmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(error) => write!(f, "recorded input could not be read: {error}"),
            Self::TooLarge => f.write_str("recorded input exceeds 32768 bytes"),
            Self::InvalidJson(error) => write!(f, "invalid recorded assignment: {error}"),
            Self::ExactOneGestureRequired => {
                f.write_str("exactly one matched press/release or unit turn is required")
            }
            Self::ChangedGeneration => f.write_str("gesture connection generation changed"),
            Self::ClockMismatch => f.write_str("gesture monotonic clock is invalid or changed"),
            Self::HoldTooLong => f.write_str("press was held for more than three seconds"),
            Self::WrongControlKind => f.write_str("gesture does not match the control kind"),
            Self::ProfileMismatch => {
                f.write_str("selected profile is unsupported or does not match the recorded device")
            }
            Self::Role(error) => write!(f, "candidate role is invalid: {error}"),
        }
    }
}

impl std::error::Error for AssignmentError {}

fn valid_sample(sample: ClockSample) -> bool {
    !sample.stamp.epoch.is_nil() && sample.stamp.frequency != 0
}

pub fn recorded_assignment(input: impl Read) -> Result<CandidateAssignment, AssignmentError> {
    recorded_assignment_inner(input, None)
}

/// Select a supported profile out of band without adding fields to the strict input schema.
pub fn recorded_assignment_for_profile(
    input: impl Read,
    profile_id: &str,
) -> Result<CandidateAssignment, AssignmentError> {
    recorded_assignment_inner(input, Some(profile_id))
}

fn recorded_assignment_inner(
    mut input: impl Read,
    profile_id: Option<&str>,
) -> Result<CandidateAssignment, AssignmentError> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take((MAX_RECORDED_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(AssignmentError::Read)?;
    if bytes.len() > MAX_RECORDED_BYTES {
        return Err(AssignmentError::TooLarge);
    }
    let recorded: RecordedAssignment =
        serde_json::from_slice(&bytes).map_err(AssignmentError::InvalidJson)?;
    let profile = match (recorded.device, profile_id) {
        (device, None) => default_profile(device),
        (DeviceKind::Micro, Some(MICRO_PROFILE)) => micro(),
        (DeviceKind::Ajazz, Some(AKP03_ORIGINAL_PROFILE)) => ajazz_original(),
        (DeviceKind::Ajazz, Some(AKP03_REV2_PROFILE)) => ajazz_rev2(),
        (DeviceKind::Ct, Some(CT_PROFILE)) => ct(),
        _ => return Err(AssignmentError::ProfileMismatch),
    };
    let (control, turn) = match recorded.events.as_slice() {
        [event] => {
            if !valid_sample(event.at) {
                return Err(AssignmentError::ClockMismatch);
            }
            if !matches!(event.kind, PhysicalEventKind::Turn(-1 | 1)) {
                return Err(AssignmentError::ExactOneGestureRequired);
            }
            (&event.control, true)
        }
        [press, release] => {
            if press.kind != PhysicalEventKind::Press
                || release.kind != PhysicalEventKind::Release
                || press.control != release.control
            {
                return Err(AssignmentError::ExactOneGestureRequired);
            }
            if press.connection_generation != release.connection_generation {
                return Err(AssignmentError::ChangedGeneration);
            }
            if !valid_sample(press.at) || !valid_sample(release.at) {
                return Err(AssignmentError::ClockMismatch);
            }
            match release
                .at
                .elapsed_within(press.at, std::time::Duration::from_secs(3))
            {
                Ok(true) => {}
                Ok(false) => return Err(AssignmentError::HoldTooLong),
                Err(_) => return Err(AssignmentError::ClockMismatch),
            }
            (&press.control, false)
        }
        _ => return Err(AssignmentError::ExactOneGestureRequired),
    };
    let binding = LogicalBinding {
        control: control.clone(),
        intent: recorded.intent,
        adjacency_confirmed: recorded.adjacency_confirmed,
    };
    validate_binding_shapes(&profile, std::slice::from_ref(&binding))
        .map_err(AssignmentError::Role)?;
    let definition = profile
        .control(control)
        .expect("shape validation checked the control");
    let discrete_kind_matches = if turn {
        matches!(definition.kind, ControlKind::Turn | ControlKind::Wheel)
    } else {
        definition.kind == ControlKind::Press
    };
    if !discrete_kind_matches {
        return Err(AssignmentError::WrongControlKind);
    }
    let mut blocking_reasons = vec![CandidateBlocker::CompleteScopeAndNativeInputEvidenceRequired];
    if !definition.normalizes_discrete {
        blocking_reasons.push(CandidateBlocker::ControlNotNormalizedByProfileAdapter);
    }
    Ok(CandidateAssignment {
        version: 1,
        device: recorded.device,
        profile: profile.id,
        binding,
        activation: Activation::CandidateOnly,
        blocking_reasons,
        command_ready: false,
    })
}

/// Implemented only by the separately integrated native private-root service.
///
/// The service must pin and revalidate the requested root and file identity,
/// enforce the platform's private-root/filesystem rules, create the basename
/// exclusively without following links, preserve partial writes on failure,
/// and synchronize both the file and directory. This trait supplies no default
/// filesystem implementation.
pub trait PrivateExportRoot {
    fn create_exclusive_and_sync(
        &mut self,
        requested_root: &Path,
        basename: &str,
        bytes: &[u8],
    ) -> io::Result<()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportOutcome {
    NotRequested,
    Delegated,
}

#[derive(Debug)]
pub enum ExportError {
    RootAndConfirmationRequired,
    InvalidConfirmation,
    RootMustBeAbsolute,
    NativePrivateRootUnavailable,
    Serialization(serde_json::Error),
    PrivateRoot(io::Error),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RootAndConfirmationRequired => {
                f.write_str("export requires both an absolute root and --confirm ASSIGN")
            }
            Self::InvalidConfirmation => f.write_str("export confirmation must be exactly ASSIGN"),
            Self::RootMustBeAbsolute => {
                f.write_str("export root must be absolute and contain no parent traversal")
            }
            Self::NativePrivateRootUnavailable => f.write_str(
                "native private-root exclusive export, identity verification, and durable sync are not integrated",
            ),
            Self::Serialization(error) => write!(f, "candidate serialization failed: {error}"),
            Self::PrivateRoot(error) => write!(f, "private-root export failed: {error}"),
        }
    }
}

impl std::error::Error for ExportError {}

pub fn export_candidate(
    candidate: &CandidateAssignment,
    root: Option<&Path>,
    confirmation: Option<&str>,
    private_root: Option<&mut dyn PrivateExportRoot>,
) -> Result<ExportOutcome, ExportError> {
    let (root, confirmation) = match (root, confirmation) {
        (None, None) => return Ok(ExportOutcome::NotRequested),
        (Some(root), Some(confirmation)) => (root, confirmation),
        _ => return Err(ExportError::RootAndConfirmationRequired),
    };
    if confirmation != "ASSIGN" {
        return Err(ExportError::InvalidConfirmation);
    }
    if !root.is_absolute() || root.components().any(|part| part == Component::ParentDir) {
        return Err(ExportError::RootMustBeAbsolute);
    }
    let private_root = private_root.ok_or(ExportError::NativePrivateRootUnavailable)?;
    let mut bytes = serde_json::to_vec_pretty(candidate).map_err(ExportError::Serialization)?;
    bytes.push(b'\n');
    private_root
        .create_exclusive_and_sync(root, EXPORT_FILENAME, &bytes)
        .map_err(ExportError::PrivateRoot)?;
    Ok(ExportOutcome::Delegated)
}
