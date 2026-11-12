use super::{assertion, complete_scope};
use crate::presentation::{
    evidence::{
        DeviceKind, EvidenceError, EvidenceLabel, EvidenceUse, PresentationFeature, Transport,
    },
    profiles,
};

#[test]
fn v02_categories_are_distinct_for_planning_input_and_visible_output() {
    let profile = profiles::micro();
    let scope = complete_scope(&profile);
    for (label, planning, input, visible) in [
        (EvidenceLabel::NotImplemented, false, false, false),
        (EvidenceLabel::Inferred, false, false, false),
        (EvidenceLabel::SourceReported, false, false, false),
        (EvidenceLabel::VendorClaim, false, false, false),
        (EvidenceLabel::OfflineDecoded, false, false, false),
        (EvidenceLabel::MockTested, true, false, false),
        (EvidenceLabel::NativeObserved, true, true, false),
        (EvidenceLabel::PhysicallyAccepted, true, true, true),
    ] {
        let evidence = assertion(&scope, label);
        assert_eq!(
            evidence
                .check(&scope, EvidenceUse::PresentationPlanning)
                .is_ok(),
            planning
        );
        assert_eq!(
            evidence.check(&scope, EvidenceUse::InputAdmission).is_ok(),
            input
        );
        assert_eq!(
            evidence.check(&scope, EvidenceUse::VisibleOutput).is_ok(),
            visible
        );
    }
}

#[test]
fn v02_every_scope_dimension_must_match_exactly() {
    let scope = complete_scope(&profiles::micro());
    let evidence = assertion(&scope, EvidenceLabel::PhysicallyAccepted);
    let mut variants = Vec::new();
    let mut value = scope.clone();
    value.device = DeviceKind::Ajazz;
    variants.push(value);
    let mut value = scope.clone();
    value.profile_id = Some("different-profile".into());
    variants.push(value);
    let mut value = scope.clone();
    value.firmware = Some("different-firmware".into());
    variants.push(value);
    let mut value = scope.clone();
    value.transport = Transport::Ble;
    variants.push(value);
    let mut value = scope.clone();
    value.mode = Some("different-mode".into());
    variants.push(value);
    let mut value = scope.clone();
    value.platform = Some("different-platform".into());
    variants.push(value);
    let mut value = scope.clone();
    value.architecture = Some("different-arch".into());
    variants.push(value);
    for other in variants {
        assert_eq!(
            evidence.check(&other, EvidenceUse::VisibleOutput),
            Err(EvidenceError::ScopeMismatch)
        );
    }
}

#[test]
fn v02_incomplete_invalid_and_bounded_scopes_never_admit_input_or_output() {
    let full = complete_scope(&profiles::micro());
    for field in 0..5 {
        let mut partial = full.clone();
        match field {
            0 => partial.profile_id = None,
            1 => partial.firmware = None,
            2 => partial.mode = None,
            3 => partial.platform = None,
            _ => partial.architecture = None,
        }
        let evidence = assertion(&partial, EvidenceLabel::PhysicallyAccepted);
        assert!(
            evidence
                .check(&partial, EvidenceUse::PresentationPlanning)
                .is_ok()
        );
        assert!(matches!(
            evidence.check(&partial, EvidenceUse::InputAdmission),
            Err(EvidenceError::IncompleteScope(_))
        ));
        assert!(matches!(
            evidence.check(&partial, EvidenceUse::VisibleOutput),
            Err(EvidenceError::IncompleteScope(_))
        ));
    }
    for bad in [
        "".to_owned(),
        "   ".to_owned(),
        "bad\nvalue".to_owned(),
        "x".repeat(129),
        "é".repeat(65),
    ] {
        let mut scope = full.clone();
        scope.firmware = Some(bad);
        assert!(matches!(
            scope.validate(true),
            Err(EvidenceError::InvalidScopeField("firmware"))
        ));
    }
    let mut boundary = full;
    boundary.firmware = Some("é".repeat(64));
    assert!(boundary.validate(true).is_ok());
}

#[test]
fn v02_native_and_physical_assertions_require_bounded_observation_references() {
    let scope = complete_scope(&profiles::micro());
    for label in [
        EvidenceLabel::NativeObserved,
        EvidenceLabel::PhysicallyAccepted,
    ] {
        let mut evidence = assertion(&scope, label);
        evidence.observation_ref = None;
        assert_eq!(
            evidence.check(&scope, EvidenceUse::PresentationPlanning),
            Err(EvidenceError::MissingObservationReference)
        );
        for invalid in ["", "  ", "bad\tref", &"x".repeat(129)] {
            evidence.observation_ref = Some(invalid.into());
            assert_eq!(
                evidence.check(&scope, EvidenceUse::PresentationPlanning),
                Err(EvidenceError::InvalidObservationReference)
            );
        }
        evidence.observation_ref = Some("x".repeat(128));
        assert!(
            evidence
                .check(&scope, EvidenceUse::PresentationPlanning)
                .is_ok()
        );
    }
}

#[test]
fn v02_default_descriptors_never_upgrade_recordings_or_claim_pulse() {
    for profile in [
        profiles::micro(),
        profiles::ajazz_original(),
        profiles::ajazz_rev2(),
        profiles::ct(),
    ] {
        assert!(profile.scope.firmware.is_none());
        assert!(profile.scope.mode.is_none());
        assert!(
            profile
                .controls
                .iter()
                .all(|control| control.input_evidence.label == EvidenceLabel::OfflineDecoded)
        );
        assert_eq!(
            profile
                .feature(PresentationFeature::SlowPulse)
                .unwrap()
                .label,
            EvidenceLabel::NotImplemented
        );
        assert!(profile.controls.iter().all(|control| {
            control
                .input_evidence
                .check(&profile.scope, EvidenceUse::InputAdmission)
                .is_err()
        }));
    }
}
