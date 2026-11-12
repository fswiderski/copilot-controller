//! Stable control descriptions and the deliberately touch-free profile adapter.

use super::Slot;
use super::protocol::{ButtonState, InputEvent, Touch, TouchFamily};

pub const PROFILE_ID: &str = "ct-pid-0003";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RawControlId(u8);

impl RawControlId {
    pub const fn new(value: u8) -> Option<Self> {
        if value <= 0x1a {
            Some(Self(value))
        } else {
            None
        }
    }

    pub const fn value(self) -> u8 {
        self.0
    }

    pub fn info(self) -> &'static RawControlInfo {
        &RAW_CONTROLS[usize::from(self.0)]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalPosition {
    Wheel,
    Knob {
        side: Side,
        row: u8,
    },
    Round {
        number: u8,
        legend: &'static str,
    },
    Square {
        side: Side,
        row: u8,
        column: u8,
        printing: &'static str,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MappingEvidence {
    SourceReported,
    Inferred,
    SingleCapturedPressRelease,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlCapabilities {
    /// Physical button-state reports, not a touch contact.
    pub press: bool,
    pub rotate: bool,
    pub rgb_led: bool,
    pub touch: bool,
    pub display: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawControlInfo {
    pub id: RawControlId,
    pub name: &'static str,
    pub position: PhysicalPosition,
    pub capabilities: ControlCapabilities,
    /// Historical input mapping only; this does not qualify live input or LEDs.
    pub mapping_evidence: MappingEvidence,
}

const fn raw(
    id: u8,
    name: &'static str,
    position: PhysicalPosition,
    evidence: MappingEvidence,
) -> RawControlInfo {
    RawControlInfo {
        id: RawControlId(id),
        name,
        position,
        capabilities: ControlCapabilities {
            press: true,
            rotate: id <= 6,
            rgb_led: id >= 7 && id <= 14,
            touch: false,
            display: false,
        },
        mapping_evidence: evidence,
    }
}

use MappingEvidence::{Inferred, SingleCapturedPressRelease, SourceReported};
use PhysicalPosition::{Knob, Round, Square, Wheel};
use Side::{Left, Right};

pub const RAW_CONTROLS: [RawControlInfo; 27] = [
    raw(0x00, "wheel", Wheel, SourceReported),
    raw(
        0x01,
        "knob_left_top",
        Knob { side: Left, row: 0 },
        SourceReported,
    ),
    raw(
        0x02,
        "knob_left_centre",
        Knob { side: Left, row: 1 },
        SourceReported,
    ),
    raw(
        0x03,
        "knob_left_bottom",
        Knob { side: Left, row: 2 },
        SourceReported,
    ),
    raw(
        0x04,
        "knob_right_top",
        Knob {
            side: Right,
            row: 0,
        },
        SourceReported,
    ),
    raw(
        0x05,
        "knob_right_centre",
        Knob {
            side: Right,
            row: 1,
        },
        SourceReported,
    ),
    raw(
        0x06,
        "knob_right_bottom",
        Knob {
            side: Right,
            row: 2,
        },
        SourceReported,
    ),
    raw(
        0x07,
        "round_1",
        Round {
            number: 1,
            legend: "Tab",
        },
        SingleCapturedPressRelease,
    ),
    raw(
        0x08,
        "round_2",
        Round {
            number: 2,
            legend: "Shift",
        },
        SourceReported,
    ),
    raw(
        0x09,
        "round_3",
        Round {
            number: 3,
            legend: "Ctrl",
        },
        SourceReported,
    ),
    raw(
        0x0a,
        "round_4",
        Round {
            number: 4,
            legend: "Alt",
        },
        SourceReported,
    ),
    raw(
        0x0b,
        "round_5",
        Round {
            number: 5,
            legend: "Command",
        },
        SourceReported,
    ),
    raw(
        0x0c,
        "round_6",
        Round {
            number: 6,
            legend: "Space",
        },
        SourceReported,
    ),
    raw(
        0x0d,
        "round_7",
        Round {
            number: 7,
            legend: "Delete/trash",
        },
        SourceReported,
    ),
    raw(
        0x0e,
        "round_8",
        Round {
            number: 8,
            legend: "Backspace",
        },
        SourceReported,
    ),
    raw(
        0x0f,
        "button_home",
        Square {
            side: Left,
            row: 0,
            column: 0,
            printing: "circle",
        },
        Inferred,
    ),
    raw(
        0x10,
        "button_undo",
        Square {
            side: Left,
            row: 1,
            column: 0,
            printing: "Undo",
        },
        SourceReported,
    ),
    raw(
        0x11,
        "button_keyboard",
        Square {
            side: Left,
            row: 2,
            column: 0,
            printing: "keyboard",
        },
        SourceReported,
    ),
    raw(
        0x12,
        "button_enter",
        Square {
            side: Left,
            row: 0,
            column: 1,
            printing: "Enter",
        },
        SourceReported,
    ),
    raw(
        0x13,
        "button_save",
        Square {
            side: Left,
            row: 1,
            column: 1,
            printing: "Save",
        },
        SourceReported,
    ),
    raw(
        0x14,
        "button_fn_left",
        Square {
            side: Left,
            row: 2,
            column: 1,
            printing: "Fn/lock",
        },
        SourceReported,
    ),
    raw(
        0x15,
        "button_a",
        Square {
            side: Right,
            row: 0,
            column: 0,
            printing: "up/A",
        },
        SourceReported,
    ),
    raw(
        0x16,
        "button_c",
        Square {
            side: Right,
            row: 1,
            column: 0,
            printing: "left/C",
        },
        SourceReported,
    ),
    raw(
        0x17,
        "button_fn_right",
        Square {
            side: Right,
            row: 2,
            column: 0,
            printing: "Fn/lock",
        },
        SourceReported,
    ),
    raw(
        0x18,
        "button_b",
        Square {
            side: Right,
            row: 0,
            column: 1,
            printing: "down/B",
        },
        SourceReported,
    ),
    raw(
        0x19,
        "button_d",
        Square {
            side: Right,
            row: 1,
            column: 1,
            printing: "right/D",
        },
        SourceReported,
    ),
    raw(
        0x1a,
        "button_e",
        Square {
            side: Right,
            row: 2,
            column: 1,
            printing: "E",
        },
        SourceReported,
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl TouchRect {
    pub fn contains(self, x: u16, y: u16) -> bool {
        u32::from(x) >= u32::from(self.x)
            && u32::from(y) >= u32::from(self.y)
            && u32::from(x) < u32::from(self.x) + u32::from(self.width)
            && u32::from(y) < u32::from(self.y) + u32::from(self.height)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchArea {
    pub name: &'static str,
    pub family: TouchFamily,
    pub rect: TouchRect,
    pub slide: bool,
    pub capabilities: ControlCapabilities,
}

const TOUCH_CAPABILITIES: ControlCapabilities = ControlCapabilities {
    press: false,
    rotate: false,
    rgb_led: false,
    touch: true,
    display: true,
};

const fn key(name: &'static str, index: u16) -> TouchArea {
    TouchArea {
        name,
        family: TouchFamily::Standard,
        rect: TouchRect {
            x: 60 + 90 * (index % 4),
            y: 90 * (index / 4),
            width: 90,
            height: 90,
        },
        slide: false,
        capabilities: TOUCH_CAPABILITIES,
    }
}

pub const TOUCH_AREAS: [TouchArea; 15] = [
    key("key_00", 0),
    key("key_01", 1),
    key("key_02", 2),
    key("key_03", 3),
    key("key_04", 4),
    key("key_05", 5),
    key("key_06", 6),
    key("key_07", 7),
    key("key_08", 8),
    key("key_09", 9),
    key("key_10", 10),
    key("key_11", 11),
    TouchArea {
        name: "strip_left",
        family: TouchFamily::Standard,
        rect: TouchRect {
            x: 0,
            y: 0,
            width: 60,
            height: 270,
        },
        slide: true,
        capabilities: TOUCH_CAPABILITIES,
    },
    TouchArea {
        name: "strip_right",
        family: TouchFamily::Standard,
        rect: TouchRect {
            x: 420,
            y: 0,
            width: 60,
            height: 270,
        },
        slide: true,
        capabilities: TOUCH_CAPABILITIES,
    },
    TouchArea {
        name: "wheel_screen",
        family: TouchFamily::Ct,
        rect: TouchRect {
            x: 0,
            y: 0,
            width: 240,
            height: 240,
        },
        slide: false,
        capabilities: TOUCH_CAPABILITIES,
    },
];

/// Diagnostic naming only; it does not recognize a touch gesture.
pub fn touch_area(touch: &Touch) -> Option<&'static TouchArea> {
    match (touch.family, touch.contact_id) {
        (TouchFamily::Standard, 1..=255) | (TouchFamily::Ct, 0) => {}
        _ => return None,
    }
    TOUCH_AREAS.iter().find(|area| {
        area.capabilities.touch
            && area.family == touch.family
            && area.rect.contains(touch.x, touch.y)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedControl {
    Raw(&'static RawControlInfo),
    Touch(&'static TouchArea),
}

impl NamedControl {
    pub fn name(self) -> &'static str {
        match self {
            Self::Raw(control) => control.name,
            Self::Touch(area) => area.name,
        }
    }
}

/// Names raw controls without converting touch contacts into gestures.
pub fn name_input(event: &InputEvent) -> Option<NamedControl> {
    match *event {
        InputEvent::Button { control, .. } if control.info().capabilities.press => {
            Some(NamedControl::Raw(control.info()))
        }
        InputEvent::Rotation { control, .. } if control.info().capabilities.rotate => {
            Some(NamedControl::Raw(control.info()))
        }
        InputEvent::Touch(touch) => touch_area(&touch).map(NamedControl::Touch),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileControlKind {
    Press,
    TouchSlide,
    Wheel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecommendedIntent {
    SelectSlot(Slot),
    CommitCursor,
    MoveCursor,
    Stop,
}

/// `None` means unknown, not false or an empty adjacency set, and cannot qualify a role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoleGeometry {
    pub near_selection: Option<bool>,
    pub isolated_stop: Option<bool>,
    pub adjacent_controls: Option<&'static [&'static str]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileControl {
    pub id: &'static str,
    pub kind: ProfileControlKind,
    pub recommendation: Option<RecommendedIntent>,
    /// Historical input mapping, not current scoped evidence.
    pub mapping_evidence: MappingEvidence,
    pub role_geometry: RoleGeometry,
}

const TURN_NAMES: [&str; 7] = [
    "wheel_turn",
    "knob_left_top_turn",
    "knob_left_centre_turn",
    "knob_left_bottom_turn",
    "knob_right_top_turn",
    "knob_right_centre_turn",
    "knob_right_bottom_turn",
];

/// Recommendations are descriptive, not evidence or action authorization.
pub fn profile_controls() -> Vec<ProfileControl> {
    let mut controls = Vec::with_capacity(49);
    for raw in RAW_CONTROLS {
        controls.push(ProfileControl {
            id: raw.name,
            kind: ProfileControlKind::Press,
            recommendation: (raw.id.value() == 7).then_some(RecommendedIntent::Stop),
            mapping_evidence: raw.mapping_evidence,
            role_geometry: RoleGeometry::default(),
        });
    }
    for (index, area) in TOUCH_AREAS.into_iter().enumerate() {
        let selected_slot = match index {
            0 => Some(1),
            1 => Some(2),
            4 => Some(3),
            5 => Some(4),
            8 => Some(5),
            9 => Some(6),
            _ => None,
        };
        let recommendation = if index == 14 {
            Some(RecommendedIntent::CommitCursor)
        } else {
            selected_slot.map(|slot| {
                RecommendedIntent::SelectSlot(Slot::new(slot).expect("profile slot is valid"))
            })
        };
        controls.push(ProfileControl {
            id: area.name,
            kind: if area.slide {
                ProfileControlKind::TouchSlide
            } else {
                ProfileControlKind::Press
            },
            recommendation,
            mapping_evidence: SourceReported,
            role_geometry: RoleGeometry::default(),
        });
    }
    for (index, name) in TURN_NAMES.into_iter().enumerate() {
        controls.push(ProfileControl {
            id: name,
            kind: ProfileControlKind::Wheel,
            recommendation: (index == 0).then_some(RecommendedIntent::MoveCursor),
            mapping_evidence: RAW_CONTROLS[index].mapping_evidence,
            role_geometry: RoleGeometry::default(),
        });
    }
    controls
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalEventKind {
    Press,
    Release,
    Turn(i8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedEvent<T> {
    pub control: &'static str,
    pub kind: PhysicalEventKind,
    pub connection_generation: u64,
    pub at: T,
}

/// Preserve the caller's clock sample and generation without interpreting either.
pub fn normalize<T>(
    event: &InputEvent,
    connection_generation: u64,
    at: T,
) -> Option<NormalizedEvent<T>> {
    let (control, kind) = match *event {
        InputEvent::Button { control, state, .. } => (
            control.info().name,
            match state {
                ButtonState::Down => PhysicalEventKind::Press,
                ButtonState::Up => PhysicalEventKind::Release,
            },
        ),
        InputEvent::Rotation {
            control,
            delta: delta @ (-1 | 1),
            ..
        } => (
            *TURN_NAMES.get(usize::from(control.value()))?,
            PhysicalEventKind::Turn(delta),
        ),
        InputEvent::Tick { .. } | InputEvent::Rotation { .. } | InputEvent::Touch(_) => {
            return None;
        }
    };
    Some(NormalizedEvent {
        control,
        kind,
        connection_generation,
        at,
    })
}
