use super::{Profile, controls::ControlId, output::KEY_COUNT};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rectangle {
    pub x: u16,
    pub y: u16,
    pub size: Size,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticPlacement {
    pub key: u8,
    pub number: u8,
    pub row: u8,
    pub column: u8,
    pub size: Size,
}

/// Row-major logical key positions, independent of JPEG rotation.
pub fn diagnostic_layout(profile: Profile) -> [DiagnosticPlacement; 6] {
    std::array::from_fn(|index| {
        let key = index as u8;
        DiagnosticPlacement {
            key,
            number: key + 1,
            row: key / 3,
            column: key % 3,
            size: Size {
                width: profile.key_size(),
                height: profile.key_size(),
            },
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderLocation {
    BottomLeft,
    LargeTop,
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObservedRev2Position {
    DisplayKey { row: u8, column: u8 },
    ExtraButton { column: u8 },
    Encoder { location: EncoderLocation },
}

/// The historical revision-2 layout is not evidence for a current device or
/// for the original profile.
pub fn observed_rev2_position(control: ControlId) -> ObservedRev2Position {
    use ControlId::{
        Button0, Button1, Button2, Button3, Button4, Button5, Button6, Button7, Button8,
        Encoder0Press, Encoder0Turn, Encoder1Press, Encoder1Turn, Encoder2Press, Encoder2Turn,
    };
    match control {
        Button0 => ObservedRev2Position::DisplayKey { row: 0, column: 0 },
        Button1 => ObservedRev2Position::DisplayKey { row: 0, column: 1 },
        Button2 => ObservedRev2Position::DisplayKey { row: 0, column: 2 },
        Button3 => ObservedRev2Position::DisplayKey { row: 1, column: 0 },
        Button4 => ObservedRev2Position::DisplayKey { row: 1, column: 1 },
        Button5 => ObservedRev2Position::DisplayKey { row: 1, column: 2 },
        Button6 => ObservedRev2Position::ExtraButton { column: 0 },
        Button7 => ObservedRev2Position::ExtraButton { column: 1 },
        Button8 => ObservedRev2Position::ExtraButton { column: 2 },
        Encoder0Turn | Encoder0Press => ObservedRev2Position::Encoder {
            location: EncoderLocation::BottomLeft,
        },
        Encoder1Turn | Encoder1Press => ObservedRev2Position::Encoder {
            location: EncoderLocation::LargeTop,
        },
        Encoder2Turn | Encoder2Press => ObservedRev2Position::Encoder {
            location: EncoderLocation::BottomRight,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedTurnDirection {
    Counterclockwise,
    Clockwise,
}

/// Direction is from the observed revision-2 unit, not a calibration grant.
pub fn observed_rev2_turn_direction(delta: i8) -> Option<ObservedTurnDirection> {
    match delta {
        -1 => Some(ObservedTurnDirection::Counterclockwise),
        1 => Some(ObservedTurnDirection::Clockwise),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewLayout {
    pub canvas: Size,
    pub keys: [Rectangle; 6],
}

/// Geometry only; labels, style, artwork, and SVG rendering belong elsewhere.
pub fn legacy_preview_layout() -> PreviewLayout {
    PreviewLayout {
        canvas: Size {
            width: 260,
            height: 310,
        },
        keys: std::array::from_fn(|index| Rectangle {
            x: 24 + 76 * (index as u16 % 3),
            y: 38 + 76 * (index as u16 / 3),
            size: Size {
                width: 60,
                height: 60,
            },
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    MissingGeometry {
        key: u8,
    },
    UnsupportedGeometry {
        key: u8,
        size: Size,
    },
    UnequalGeometry {
        key: u8,
        expected: Size,
        actual: Size,
    },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::MissingGeometry { key } => write!(f, "missing screen geometry for key {key}"),
            Self::UnsupportedGeometry { key, size } => write!(
                f,
                "unsupported screen geometry {}x{} for key {key}",
                size.width, size.height
            ),
            Self::UnequalGeometry {
                key,
                expected,
                actual,
            } => write!(
                f,
                "key {key} geometry {}x{} differs from {}x{}",
                actual.width, actual.height, expected.width, expected.height
            ),
        }
    }
}

impl std::error::Error for LayoutError {}

/// Validate six realized screen sizes before laying out an upright profile view.
pub fn profile_preview_layout(
    screens: [Option<Size>; KEY_COUNT as usize],
) -> Result<PreviewLayout, LayoutError> {
    let mut expected = None;
    for (index, screen) in screens.into_iter().enumerate() {
        let key = index as u8;
        let size = screen.ok_or(LayoutError::MissingGeometry { key })?;
        if size.width != size.height || !matches!(size.width, 60 | 64) {
            return Err(LayoutError::UnsupportedGeometry { key, size });
        }
        if let Some(first) = expected {
            if first != size {
                return Err(LayoutError::UnequalGeometry {
                    key,
                    expected: first,
                    actual: size,
                });
            }
        } else {
            expected = Some(size);
        }
    }

    // Exactly six screens were validated, so the first entry exists.
    let size = screens[0].ok_or(LayoutError::MissingGeometry { key: 0 })?;
    Ok(PreviewLayout {
        canvas: Size {
            width: 240,
            height: 190,
        },
        keys: std::array::from_fn(|index| Rectangle {
            x: 10 + 76 * (index as u16 % 3),
            y: 10 + 85 * (index as u16 / 3),
            size,
        }),
    })
}
