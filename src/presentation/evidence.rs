//! Exact-scope evidence checks. Evidence is supplied by the integrator.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Micro,
    Ajazz,
    Ct,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Usb,
    Ble,
    Serial,
}

/// Missing fields are representable for offline descriptors, never for admission.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceScope {
    pub device: DeviceKind,
    pub profile_id: Option<String>,
    pub firmware: Option<String>,
    pub transport: Transport,
    pub mode: Option<String>,
    pub platform: Option<String>,
    pub architecture: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceError {
    InvalidScopeField(&'static str),
    IncompleteScope(&'static str),
    ScopeMismatch,
    InsufficientEvidence,
    MissingObservationReference,
    InvalidObservationReference,
}

impl std::fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidScopeField(field) => write!(f, "invalid evidence scope field: {field}"),
            Self::IncompleteScope(field) => write!(f, "missing evidence scope field: {field}"),
            Self::ScopeMismatch => f.write_str("evidence scope does not exactly match"),
            Self::InsufficientEvidence => f.write_str("evidence category is insufficient"),
            Self::MissingObservationReference => {
                f.write_str("native or physical evidence requires an observation reference")
            }
            Self::InvalidObservationReference => f.write_str("invalid observation reference"),
        }
    }
}

impl std::error::Error for EvidenceError {}

fn bounded_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

impl EvidenceScope {
    pub fn validate(&self, complete: bool) -> Result<(), EvidenceError> {
        for (name, value) in [
            ("profile_id", &self.profile_id),
            ("firmware", &self.firmware),
            ("mode", &self.mode),
            ("platform", &self.platform),
            ("architecture", &self.architecture),
        ] {
            match value {
                Some(value) if !bounded_text(value) => {
                    return Err(EvidenceError::InvalidScopeField(name));
                }
                None if complete => return Err(EvidenceError::IncompleteScope(name)),
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLabel {
    NotImplemented,
    Inferred,
    SourceReported,
    VendorClaim,
    OfflineDecoded,
    MockTested,
    NativeObserved,
    PhysicallyAccepted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceUse {
    PresentationPlanning,
    InputAdmission,
    VisibleOutput,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub scope: EvidenceScope,
    pub label: EvidenceLabel,
    pub observation_ref: Option<String>,
}

impl Evidence {
    /// Validates only the supplied assertion; it does not authenticate observations.
    pub fn check(&self, expected: &EvidenceScope, usage: EvidenceUse) -> Result<(), EvidenceError> {
        self.scope.validate(false)?;
        expected.validate(false)?;
        if self.scope != *expected {
            return Err(EvidenceError::ScopeMismatch);
        }
        if usage != EvidenceUse::PresentationPlanning {
            expected.validate(true)?;
        }
        if let Some(reference) = &self.observation_ref
            && !bounded_text(reference)
        {
            return Err(EvidenceError::InvalidObservationReference);
        }
        if matches!(
            self.label,
            EvidenceLabel::NativeObserved | EvidenceLabel::PhysicallyAccepted
        ) && self.observation_ref.is_none()
        {
            return Err(EvidenceError::MissingObservationReference);
        }
        let accepted = match usage {
            EvidenceUse::PresentationPlanning => matches!(
                self.label,
                EvidenceLabel::MockTested
                    | EvidenceLabel::NativeObserved
                    | EvidenceLabel::PhysicallyAccepted
            ),
            EvidenceUse::InputAdmission => matches!(
                self.label,
                EvidenceLabel::NativeObserved | EvidenceLabel::PhysicallyAccepted
            ),
            EvidenceUse::VisibleOutput => self.label == EvidenceLabel::PhysicallyAccepted,
        };
        if accepted {
            Ok(())
        } else {
            Err(EvidenceError::InsufficientEvidence)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationFeature {
    Color,
    Words,
    SelectionFrames,
    StatusIcons,
    Brightness,
    Steady,
    SlowPulse,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureEvidence {
    pub feature: PresentationFeature,
    pub evidence: Evidence,
}
