use super::super::{
    Profile,
    controls::ControlId,
    layout::{
        EncoderLocation, LayoutError, ObservedRev2Position, ObservedTurnDirection, Size,
        diagnostic_layout, legacy_preview_layout, observed_rev2_position,
        observed_rev2_turn_direction, profile_preview_layout,
    },
};

#[test]
fn diagnostic_grid_maps_numbers_to_upright_logical_positions() {
    for profile in [Profile::Original, Profile::Rev2] {
        let positions = diagnostic_layout(profile);
        for (index, position) in positions.into_iter().enumerate() {
            assert_eq!(position.key, index as u8);
            assert_eq!(position.number, index as u8 + 1);
            assert_eq!(position.row, index as u8 / 3);
            assert_eq!(position.column, index as u8 % 3);
            assert_eq!(
                position.size,
                Size {
                    width: profile.key_size(),
                    height: profile.key_size()
                }
            );
        }
    }
}

#[test]
fn observed_rev2_positions_and_unit_turn_directions_are_scoped() {
    for (id, row, column) in [
        (ControlId::Button0, 0, 0),
        (ControlId::Button1, 0, 1),
        (ControlId::Button2, 0, 2),
        (ControlId::Button3, 1, 0),
        (ControlId::Button4, 1, 1),
        (ControlId::Button5, 1, 2),
    ] {
        assert_eq!(
            observed_rev2_position(id),
            ObservedRev2Position::DisplayKey { row, column }
        );
    }
    for (id, column) in [
        (ControlId::Button6, 0),
        (ControlId::Button7, 1),
        (ControlId::Button8, 2),
    ] {
        assert_eq!(
            observed_rev2_position(id),
            ObservedRev2Position::ExtraButton { column }
        );
    }
    for (turn, press, location) in [
        (
            ControlId::Encoder0Turn,
            ControlId::Encoder0Press,
            EncoderLocation::BottomLeft,
        ),
        (
            ControlId::Encoder1Turn,
            ControlId::Encoder1Press,
            EncoderLocation::LargeTop,
        ),
        (
            ControlId::Encoder2Turn,
            ControlId::Encoder2Press,
            EncoderLocation::BottomRight,
        ),
    ] {
        let expected = ObservedRev2Position::Encoder { location };
        assert_eq!(observed_rev2_position(turn), expected);
        assert_eq!(observed_rev2_position(press), expected);
    }
    assert_eq!(
        observed_rev2_turn_direction(-1),
        Some(ObservedTurnDirection::Counterclockwise)
    );
    assert_eq!(
        observed_rev2_turn_direction(1),
        Some(ObservedTurnDirection::Clockwise)
    );
    for delta in [i8::MIN, -2, 0, 2, i8::MAX] {
        assert_eq!(observed_rev2_turn_direction(delta), None);
    }
}

#[test]
fn legacy_and_profile_preview_geometry_remains_upright() {
    let legacy = legacy_preview_layout();
    assert_eq!(
        legacy.canvas,
        Size {
            width: 260,
            height: 310
        }
    );
    for (key, (x, y)) in legacy.keys.into_iter().zip([
        (24, 38),
        (100, 38),
        (176, 38),
        (24, 114),
        (100, 114),
        (176, 114),
    ]) {
        assert_eq!((key.x, key.y), (x, y));
        assert_eq!(
            key.size,
            Size {
                width: 60,
                height: 60
            }
        );
    }
    for side in [60, 64] {
        let size = Size {
            width: side,
            height: side,
        };
        let layout = profile_preview_layout([Some(size); 6]).unwrap();
        assert_eq!(
            layout.canvas,
            Size {
                width: 240,
                height: 190
            }
        );
        for (key, (x, y)) in layout.keys.into_iter().zip([
            (10, 10),
            (86, 10),
            (162, 10),
            (10, 95),
            (86, 95),
            (162, 95),
        ]) {
            assert_eq!((key.x, key.y), (x, y));
            assert_eq!(key.size, size);
        }
    }
}

#[test]
fn profile_layout_refuses_missing_unequal_rectangular_and_unsupported_geometry() {
    let sixty = Size {
        width: 60,
        height: 60,
    };
    for index in 0..6 {
        let mut screens = [Some(sixty); 6];
        screens[index] = None;
        assert_eq!(
            profile_preview_layout(screens),
            Err(LayoutError::MissingGeometry { key: index as u8 })
        );
    }
    for invalid in [
        Size {
            width: 60,
            height: 64,
        },
        Size {
            width: 59,
            height: 59,
        },
        Size {
            width: 65,
            height: 65,
        },
        Size {
            width: 0,
            height: 0,
        },
    ] {
        let mut screens = [Some(sixty); 6];
        screens[4] = Some(invalid);
        assert_eq!(
            profile_preview_layout(screens),
            Err(LayoutError::UnsupportedGeometry {
                key: 4,
                size: invalid
            })
        );
    }
    for index in 1..6 {
        let mut screens = [Some(sixty); 6];
        let actual = Size {
            width: 64,
            height: 64,
        };
        screens[index] = Some(actual);
        assert_eq!(
            profile_preview_layout(screens),
            Err(LayoutError::UnequalGeometry {
                key: index as u8,
                expected: sixty,
                actual
            })
        );
    }
}
