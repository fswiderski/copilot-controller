use super::{Profile, input::DecodedInput};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

macro_rules! control_ids {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum ControlId {
            $(#[serde(rename = $name)] $variant),+
        }

        impl ControlId {
            pub const ALL: [Self; 15] = [$(Self::$variant),+];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }
        }

        impl FromStr for ControlId {
            type Err = UnknownControlId;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($name => Ok(Self::$variant),)+
                    _ => Err(UnknownControlId),
                }
            }
        }
    };
}

control_ids! {
    Button0 => "button_0",
    Button1 => "button_1",
    Button2 => "button_2",
    Button3 => "button_3",
    Button4 => "button_4",
    Button5 => "button_5",
    Button6 => "button_6",
    Button7 => "button_7",
    Button8 => "button_8",
    Encoder0Turn => "encoder_0_turn",
    Encoder0Press => "encoder_0_press",
    Encoder1Turn => "encoder_1_turn",
    Encoder1Press => "encoder_1_press",
    Encoder2Turn => "encoder_2_turn",
    Encoder2Press => "encoder_2_press",
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownControlId;

impl fmt::Display for UnknownControlId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown AKP03 control ID")
    }
}

impl std::error::Error for UnknownControlId {}

const BUTTONS: [ControlId; 9] = [
    ControlId::Button0,
    ControlId::Button1,
    ControlId::Button2,
    ControlId::Button3,
    ControlId::Button4,
    ControlId::Button5,
    ControlId::Button6,
    ControlId::Button7,
    ControlId::Button8,
];
const ENCODER_TURNS: [ControlId; 3] = [
    ControlId::Encoder0Turn,
    ControlId::Encoder1Turn,
    ControlId::Encoder2Turn,
];
const ENCODER_PRESSES: [ControlId; 3] = [
    ControlId::Encoder0Press,
    ControlId::Encoder1Press,
    ControlId::Encoder2Press,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Press,
    Turn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendedAction {
    Stop,
}

/// A recommendation is descriptive data, not an installed binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    SelectSlot(u8),
    MoveCursor,
    CommitCursor,
    Command(RecommendedAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ControlDescriptor {
    pub id: ControlId,
    pub kind: ControlKind,
    pub recommended: Option<Recommendation>,
    /// True also covers unspecified proximity, so it cannot bypass confirmation.
    pub near_selection: bool,
    pub isolated_stop: bool,
    pub adjacent: &'static [ControlId],
}

const fn control(
    id: ControlId,
    kind: ControlKind,
    recommended: Option<Recommendation>,
    near_selection: bool,
    isolated_stop: bool,
    adjacent: &'static [ControlId],
) -> ControlDescriptor {
    ControlDescriptor {
        id,
        kind,
        recommended,
        near_selection,
        isolated_stop,
        adjacent,
    }
}

const CONTROLS: [ControlDescriptor; 15] = [
    control(
        ControlId::Button0,
        ControlKind::Press,
        Some(Recommendation::SelectSlot(1)),
        true,
        false,
        &[],
    ),
    control(
        ControlId::Button1,
        ControlKind::Press,
        Some(Recommendation::SelectSlot(2)),
        true,
        false,
        &[],
    ),
    control(
        ControlId::Button2,
        ControlKind::Press,
        Some(Recommendation::SelectSlot(3)),
        true,
        false,
        &[],
    ),
    control(
        ControlId::Button3,
        ControlKind::Press,
        Some(Recommendation::SelectSlot(4)),
        true,
        false,
        &[],
    ),
    control(
        ControlId::Button4,
        ControlKind::Press,
        Some(Recommendation::SelectSlot(5)),
        true,
        false,
        &[],
    ),
    control(
        ControlId::Button5,
        ControlKind::Press,
        Some(Recommendation::SelectSlot(6)),
        true,
        false,
        &[],
    ),
    control(
        ControlId::Encoder1Turn,
        ControlKind::Turn,
        Some(Recommendation::MoveCursor),
        true,
        false,
        &[],
    ),
    control(
        ControlId::Encoder1Press,
        ControlKind::Press,
        Some(Recommendation::CommitCursor),
        true,
        false,
        &[],
    ),
    control(
        ControlId::Button8,
        ControlKind::Press,
        Some(Recommendation::Command(RecommendedAction::Stop)),
        false,
        true,
        &[ControlId::Button7],
    ),
    control(
        ControlId::Button6,
        ControlKind::Press,
        None,
        true,
        false,
        &[],
    ),
    control(
        ControlId::Button7,
        ControlKind::Press,
        None,
        true,
        false,
        &[ControlId::Button8],
    ),
    control(
        ControlId::Encoder0Turn,
        ControlKind::Turn,
        None,
        true,
        false,
        &[],
    ),
    control(
        ControlId::Encoder0Press,
        ControlKind::Press,
        None,
        true,
        false,
        &[],
    ),
    control(
        ControlId::Encoder2Turn,
        ControlKind::Turn,
        None,
        true,
        false,
        &[],
    ),
    control(
        ControlId::Encoder2Press,
        ControlKind::Press,
        None,
        true,
        false,
        &[],
    ),
];

/// Both profiles share controls, ordered by recommendation priority with
/// unassigned controls last. This does not imply shared release behavior.
pub fn profile_controls(_profile: Profile) -> &'static [ControlDescriptor] {
    &CONTROLS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhysicalEventKind {
    Press,
    Release,
    Turn(i8),
}

/// `T` is supplied by the shared clock layer and is passed through unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalEvent<T> {
    pub control: ControlId,
    pub kind: PhysicalEventKind,
    pub connection_generation: u64,
    pub at: T,
}

/// Map only explicit push states and unit turns; this does not admit a gesture.
pub fn normalize<T>(
    input: DecodedInput,
    connection_generation: u64,
    at: T,
) -> Option<PhysicalEvent<T>> {
    let (control, kind) = match input {
        DecodedInput::ButtonState { index, pressed } => (
            *BUTTONS.get(usize::from(index))?,
            if pressed {
                PhysicalEventKind::Press
            } else {
                PhysicalEventKind::Release
            },
        ),
        DecodedInput::EncoderState { index, pressed } => (
            *ENCODER_PRESSES.get(usize::from(index))?,
            if pressed {
                PhysicalEventKind::Press
            } else {
                PhysicalEventKind::Release
            },
        ),
        DecodedInput::EncoderTurn { index, delta } if matches!(delta, -1 | 1) => (
            *ENCODER_TURNS.get(usize::from(index))?,
            PhysicalEventKind::Turn(delta),
        ),
        _ => return None,
    };
    Some(PhysicalEvent {
        control,
        kind,
        connection_generation,
        at,
    })
}
