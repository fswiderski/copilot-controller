use crate::domain::{ClockSample, ClockSource, MonotonicStamp, RuntimeEpoch, Status};

use super::{
    evidence::{Evidence, EvidenceLabel, EvidenceScope},
    input::{PhysicalEvent, PhysicalEventKind},
    profiles::DeviceProfile,
    semantic::{Authority, Availability, Occupancy, Selection, SlotSnapshot, Snapshot},
};

mod artwork;
mod assignment;
mod evidence;
mod input;
mod realization;
mod semantic;

fn epoch() -> RuntimeEpoch {
    RuntimeEpoch::parse_str("11111111-1111-4111-8111-111111111111").unwrap()
}

fn sample(ticks: u64) -> ClockSample {
    ClockSample {
        source: ClockSource::LinuxMonotonic,
        stamp: MonotonicStamp {
            epoch: epoch(),
            ticks,
            frequency: 1000,
        },
    }
}

fn snapshot() -> Snapshot {
    let statuses = [
        Status::Idle,
        Status::Working,
        Status::Complete,
        Status::Waiting,
        Status::Error,
        Status::Unknown,
    ];
    Snapshot {
        epoch: epoch(),
        generation: 1,
        connected: true,
        slots: std::array::from_fn(|index| SlotSnapshot {
            occupancy: Occupancy::Bound,
            status: statuses[index],
            availability: Availability::Live,
            selection: Selection::None,
            cursor: false,
            authority: Authority::ObserveOnly,
            overlay: None,
        }),
    }
}

fn complete_scope(profile: &DeviceProfile) -> EvidenceScope {
    let mut scope = profile.scope.clone();
    scope.firmware = Some("fixture-firmware".into());
    scope.mode = Some("fixture-layer".into());
    scope.platform = Some("fixture-platform".into());
    scope.architecture = Some("fixture-arch".into());
    scope
}

fn assertion(scope: &EvidenceScope, label: EvidenceLabel) -> Evidence {
    Evidence {
        scope: scope.clone(),
        label,
        observation_ref: matches!(
            label,
            EvidenceLabel::NativeObserved | EvidenceLabel::PhysicallyAccepted
        )
        .then(|| "fixture-observation".into()),
    }
}

fn input_profile(mut profile: DeviceProfile) -> (DeviceProfile, EvidenceScope) {
    let scope = complete_scope(&profile);
    for control in &mut profile.controls {
        control.input_evidence = assertion(&scope, EvidenceLabel::NativeObserved);
    }
    (profile, scope)
}

fn event(control: &str, kind: PhysicalEventKind, ticks: u64) -> PhysicalEvent {
    PhysicalEvent {
        control: control.into(),
        kind,
        connection_generation: 7,
        at: sample(ticks),
    }
}
