//! Offline device descriptors and their documented logical-control mappings.

use serde::{Deserialize, Serialize};

use crate::domain::{Action, Slot};

use super::{
    evidence::{
        DeviceKind, Evidence, EvidenceLabel, EvidenceScope, FeatureEvidence, PresentationFeature,
        Transport,
    },
    input::Intent,
};

pub const MICRO_PROFILE: &str = "micro-2-pro";
pub const AKP03_ORIGINAL_PROFILE: &str = "akp03-original";
pub const AKP03_REV2_PROFILE: &str = "akp03e-rev2";
pub const CT_PROFILE: &str = "ct-pid-0003";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Press,
    Turn,
    Stick,
    TouchSlide,
    Wheel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub id: String,
    pub kind: ControlKind,
    pub recommended: Option<Intent>,
    pub near_selection: bool,
    pub isolated_stop: bool,
    pub adjacent: Vec<String>,
    /// A declaration can exist even where the strict adapter drops that input.
    pub normalizes_discrete: bool,
    pub input_evidence: Evidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceProfile {
    pub device: DeviceKind,
    pub id: String,
    pub scope: EvidenceScope,
    pub tile_size: Option<u16>,
    pub controls: Vec<Control>,
    pub features: Vec<FeatureEvidence>,
}

impl DeviceProfile {
    pub fn control(&self, id: &str) -> Option<&Control> {
        self.controls.iter().find(|control| control.id == id)
    }

    pub fn feature(&self, feature: PresentationFeature) -> Option<&Evidence> {
        self.features
            .iter()
            .find(|entry| entry.feature == feature)
            .map(|entry| &entry.evidence)
    }

    pub fn matches_scope(&self, scope: &EvidenceScope) -> bool {
        self.device == scope.device && scope.profile_id.as_deref() == Some(self.id.as_str())
    }

    /// Stable priority order; unassigned controls follow every assigned role.
    pub fn prioritized_controls(&self) -> Vec<&Control> {
        let mut controls: Vec<_> = self.controls.iter().collect();
        controls.sort_by_key(|control| control.recommended.map_or(6, Intent::priority));
        controls
    }
}

fn evidence(scope: &EvidenceScope, label: EvidenceLabel) -> Evidence {
    Evidence {
        scope: scope.clone(),
        label,
        observation_ref: None,
    }
}

fn control(
    scope: &EvidenceScope,
    id: impl Into<String>,
    kind: ControlKind,
    recommended: Option<Intent>,
    near_selection: bool,
    isolated_stop: bool,
    normalizes_discrete: bool,
) -> Control {
    Control {
        id: id.into(),
        kind,
        recommended,
        near_selection,
        isolated_stop,
        adjacent: Vec::new(),
        normalizes_discrete,
        input_evidence: evidence(scope, EvidenceLabel::OfflineDecoded),
    }
}

fn neighbors(controls: &mut [Control], left: &str, right: &str) {
    for (from, to) in [(left, right), (right, left)] {
        if let Some(control) = controls.iter_mut().find(|control| control.id == from) {
            control.adjacent.push(to.to_owned());
        }
    }
}

fn grid_neighbors(controls: &mut [Control], prefix: &str, rows: usize, columns: usize) {
    for row in 0..rows {
        for column in 0..columns {
            let id = format!("{prefix}{:02}", row * columns + column);
            if column + 1 < columns {
                neighbors(
                    controls,
                    &id,
                    &format!("{prefix}{:02}", row * columns + column + 1),
                );
            }
            if row + 1 < rows {
                neighbors(
                    controls,
                    &id,
                    &format!("{prefix}{:02}", (row + 1) * columns + column),
                );
            }
        }
    }
}

fn profile_base(device: DeviceKind, id: &str, tile_size: Option<u16>) -> DeviceProfile {
    let scope = EvidenceScope {
        device,
        profile_id: Some(id.to_owned()),
        firmware: None,
        transport: match device {
            DeviceKind::Micro | DeviceKind::Ajazz => Transport::Usb,
            DeviceKind::Ct => Transport::Serial,
        },
        mode: None,
        platform: None,
        architecture: None,
    };
    let features = [
        PresentationFeature::Color,
        PresentationFeature::Words,
        PresentationFeature::SelectionFrames,
        PresentationFeature::StatusIcons,
        PresentationFeature::Brightness,
        PresentationFeature::Steady,
        PresentationFeature::SlowPulse,
    ]
    .into_iter()
    .map(|feature| {
        let implemented = match feature {
            PresentationFeature::Color | PresentationFeature::Steady => true,
            PresentationFeature::Words | PresentationFeature::SelectionFrames => {
                device != DeviceKind::Micro
            }
            PresentationFeature::StatusIcons => device == DeviceKind::Ajazz,
            PresentationFeature::Brightness => device == DeviceKind::Micro,
            PresentationFeature::SlowPulse => false,
        };
        FeatureEvidence {
            feature,
            evidence: evidence(
                &scope,
                if implemented {
                    EvidenceLabel::MockTested
                } else {
                    EvidenceLabel::NotImplemented
                },
            ),
        }
    })
    .collect();
    DeviceProfile {
        device,
        id: id.to_owned(),
        scope,
        tile_size,
        controls: Vec::new(),
        features,
    }
}

pub fn micro() -> DeviceProfile {
    let mut profile = profile_base(DeviceKind::Micro, MICRO_PROFILE, None);
    for index in 0..6 {
        profile.controls.push(control(
            &profile.scope,
            format!("AG{index:02}"),
            ControlKind::Press,
            Some(Intent::SelectSlot(Slot::ALL[index])),
            true,
            false,
            true,
        ));
    }
    profile.controls.push(control(
        &profile.scope,
        "encoder_turn",
        ControlKind::Turn,
        Some(Intent::MoveCursor),
        false,
        false,
        true,
    ));
    profile.controls.push(control(
        &profile.scope,
        "encoder_press",
        ControlKind::Press,
        Some(Intent::CommitCursor),
        true,
        false,
        false,
    ));
    for (id, action) in [
        ("ACT06", Action::Autopilot),
        ("ACT07", Action::Accept),
        ("ACT08", Action::Stop),
        ("ACT09", Action::Fork),
        ("ACT12", Action::Send),
    ] {
        // The appendix does not establish physical isolation for these keys.
        profile.controls.push(control(
            &profile.scope,
            id,
            ControlKind::Press,
            Some(Intent::Command(action)),
            true,
            false,
            true,
        ));
    }
    for (id, action) in [
        ("DPAD_UP", Action::Up),
        ("DPAD_RIGHT", Action::Right),
        ("DPAD_DOWN", Action::Down),
        ("DPAD_LEFT", Action::Left),
    ] {
        profile.controls.push(control(
            &profile.scope,
            id,
            ControlKind::Stick,
            Some(Intent::Command(action)),
            false,
            false,
            false,
        ));
    }
    profile
}

pub fn ajazz_original() -> DeviceProfile {
    ajazz(AKP03_ORIGINAL_PROFILE, 60, false)
}

pub fn ajazz_rev2() -> DeviceProfile {
    ajazz(AKP03_REV2_PROFILE, 64, true)
}

fn ajazz(id: &str, tile_size: u16, explicit_release: bool) -> DeviceProfile {
    let mut profile = profile_base(DeviceKind::Ajazz, id, Some(tile_size));
    for index in 0..9 {
        let recommended = if index < 6 {
            Some(Intent::SelectSlot(Slot::ALL[index]))
        } else if index == 8 {
            Some(Intent::Command(Action::Stop))
        } else {
            None
        };
        profile.controls.push(control(
            &profile.scope,
            format!("button_{index}"),
            ControlKind::Press,
            recommended,
            index != 8,
            index == 8,
            explicit_release,
        ));
    }
    for index in 0..3 {
        profile.controls.push(control(
            &profile.scope,
            format!("encoder_{index}_turn"),
            ControlKind::Turn,
            (index == 1).then_some(Intent::MoveCursor),
            false,
            false,
            true,
        ));
        profile.controls.push(control(
            &profile.scope,
            format!("encoder_{index}_press"),
            ControlKind::Press,
            (index == 1).then_some(Intent::CommitCursor),
            true,
            false,
            explicit_release,
        ));
    }
    for (left, right) in [
        ("button_0", "button_1"),
        ("button_1", "button_2"),
        ("button_3", "button_4"),
        ("button_4", "button_5"),
        ("button_0", "button_3"),
        ("button_1", "button_4"),
        ("button_2", "button_5"),
        ("button_6", "button_7"),
        ("button_7", "button_8"),
    ] {
        neighbors(&mut profile.controls, left, right);
    }
    profile
}

/// CT raw button names, indexed by the documented 0x00..=0x1a IDs.
pub const CT_BUTTON_NAMES: [&str; 27] = [
    "wheel",
    "knob_left_top",
    "knob_left_centre",
    "knob_left_bottom",
    "knob_right_top",
    "knob_right_centre",
    "knob_right_bottom",
    "round_1",
    "round_2",
    "round_3",
    "round_4",
    "round_5",
    "round_6",
    "round_7",
    "round_8",
    "button_home",
    "button_undo",
    "button_keyboard",
    "button_enter",
    "button_save",
    "button_fn_left",
    "button_a",
    "button_c",
    "button_fn_right",
    "button_b",
    "button_d",
    "button_e",
];

pub fn ct() -> DeviceProfile {
    let mut profile = profile_base(DeviceKind::Ct, CT_PROFILE, None);
    for (index, id) in CT_BUTTON_NAMES.iter().enumerate() {
        profile.controls.push(control(
            &profile.scope,
            *id,
            ControlKind::Press,
            (index == 7).then_some(Intent::Command(Action::Stop)),
            true,
            false,
            true,
        ));
        if index < 7 {
            profile.controls.push(control(
                &profile.scope,
                format!("{id}_turn"),
                ControlKind::Wheel,
                (index == 0).then_some(Intent::MoveCursor),
                false,
                false,
                true,
            ));
        }
    }
    for index in 0..12 {
        let slot_index = [0, 1, 4, 5, 8, 9].iter().position(|item| *item == index);
        profile.controls.push(control(
            &profile.scope,
            format!("key_{index:02}"),
            ControlKind::Press,
            slot_index.map(|slot| Intent::SelectSlot(Slot::ALL[slot])),
            true,
            false,
            false,
        ));
    }
    for id in ["strip_left", "strip_right"] {
        profile.controls.push(control(
            &profile.scope,
            id,
            ControlKind::TouchSlide,
            None,
            true,
            false,
            false,
        ));
    }
    profile.controls.push(control(
        &profile.scope,
        "wheel_screen",
        ControlKind::Press,
        Some(Intent::CommitCursor),
        true,
        false,
        false,
    ));
    grid_neighbors(&mut profile.controls, "key_", 3, 4);
    for index in 1..8 {
        neighbors(
            &mut profile.controls,
            &format!("round_{index}"),
            &format!("round_{}", index + 1),
        );
    }
    for (left, right) in [
        ("button_home", "button_enter"),
        ("button_undo", "button_save"),
        ("button_keyboard", "button_fn_left"),
        ("button_home", "button_undo"),
        ("button_undo", "button_keyboard"),
        ("button_enter", "button_save"),
        ("button_save", "button_fn_left"),
        ("button_a", "button_b"),
        ("button_c", "button_d"),
        ("button_fn_right", "button_e"),
        ("button_a", "button_c"),
        ("button_c", "button_fn_right"),
        ("button_b", "button_d"),
        ("button_d", "button_e"),
    ] {
        neighbors(&mut profile.controls, left, right);
    }
    profile
}

pub fn default_profile(device: DeviceKind) -> DeviceProfile {
    match device {
        DeviceKind::Micro => micro(),
        DeviceKind::Ajazz => ajazz_original(),
        DeviceKind::Ct => ct(),
    }
}
