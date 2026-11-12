use std::collections::HashSet;

use super::super::Slot;
use super::super::layout::{
    ActionTileKind, BindingHealth, CommandAvailability, Health, Label, Layout, LayoutError,
    REFUSAL_STYLE, RefusalReason, Selection, SemanticAction, SessionStatus, SlotView, Tile,
    TileState, View, prepare_label, resolve_state,
};
use super::super::pixels::{Region, Rgb};
use super::super::scene::{Operation, Rect, Scene, changed_scenes};
use super::super::text::Direction;

fn slot(number: u8) -> Slot {
    Slot::new(number).unwrap()
}

fn populated() -> View {
    View {
        slots: std::array::from_fn(|index| SlotView {
            state: TileState::Working,
            label: Label::Approved {
                text: format!("agent-{}", index + 1),
                available: true,
            },
            refusal: None,
        }),
        confirmed: Some(slot(1)),
        pending: None,
        commands: CommandAvailability {
            run: true,
            accept: false,
            stop: true,
            fork: true,
            send: true,
        },
        health: Health::Warning,
        neutral: false,
    }
}

fn texts(scene: &Scene) -> Vec<&str> {
    scene
        .operations
        .iter()
        .filter_map(|operation| match operation {
            Operation::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

fn corners_inside(rect: Rect, radius: u16) -> bool {
    let radius2 = i32::from(radius).pow(2);
    [rect.x, rect.x + rect.width].into_iter().all(|x| {
        [rect.y, rect.y + rect.height]
            .into_iter()
            .all(|y| (i32::from(x) - 120).pow(2) + (i32::from(y) - 120).pow(2) <= radius2)
    })
}

#[test]
fn layout_limits_geometry_and_wheel_safe_circle_are_exhaustive() {
    let defaults = Layout::default();
    assert_eq!(defaults.slot_keys(), [0, 1, 4, 5, 8, 9]);
    assert_eq!(defaults.action_keys(), [2, 3, 6, 7, 10, 11]);
    let g = defaults.key_geometry();
    assert_eq!(g.safe, Rect::new(0, 0, 90, 90));
    assert_eq!(g.content, Rect::new(4, 4, 82, 82));
    assert_eq!(g.number, Rect::new(5, 4, 18, 18));
    assert_eq!(g.marker, Rect::new(23, 7, 62, 12));
    assert_eq!(g.band, Rect::new(4, 59, 82, 27));
    assert_eq!(g.label, Rect::new(5, 24, 80, 32));
    assert_eq!(g.action_icon, Rect::new(27, 4, 36, 36));
    assert_eq!(g.action_name, Rect::new(5, 41, 80, 16));
    let default_wheel = defaults.wheel_geometry();
    assert_eq!(
        (
            default_wheel.radius,
            default_wheel.inner_radius,
            default_wheel.safe_radius
        ),
        (120, 112, 108)
    );
    assert_eq!(default_wheel.marker, Rect::new(58, 32, 124, 16));
    assert_eq!(default_wheel.number, Rect::new(40, 50, 160, 48));
    assert_eq!(default_wheel.label, Rect::new(30, 102, 180, 38));
    assert_eq!(default_wheel.pill, Rect::new(45, 146, 150, 34));

    for inset in 0..=6 {
        for diameter in 200..=240 {
            let layout = Layout::new(
                defaults.slot_keys(),
                defaults.action_keys(),
                inset,
                diameter,
            )
            .unwrap();
            let g = layout.key_geometry();
            assert_eq!(g.content.width, 82 - 2 * u16::from(inset));
            assert!(g.action_icon.width >= 24 && g.action_icon.width <= 36);
            assert_eq!(g.action_icon.width, g.action_icon.height);
            if inset == 6 {
                assert_eq!(g.action_icon.width, 24);
                assert_eq!(g.label_lines, 1);
            }
            let wheel = layout.wheel_geometry();
            assert_eq!(wheel.radius, diameter / 2);
            for rect in [wheel.marker, wheel.number, wheel.label, wheel.pill] {
                assert!(
                    corners_inside(rect, wheel.safe_radius),
                    "inset {inset} diameter {diameter} rect {rect:?}"
                );
            }
            let mut view = populated();
            view.slots[0].refusal = Some(RefusalReason::ChangedDialog);
            for scene in layout.scenes(&view) {
                assert_eq!(
                    scene.validate(),
                    Ok(()),
                    "inset {inset} diameter {diameter}, region {:?}",
                    scene.region
                );
            }
        }
    }
    assert_eq!(
        Layout::new([0, 1, 4, 5, 8, 9], [2, 3, 6, 7, 10, 10], 0, 240),
        Err(LayoutError::DuplicateKey(10))
    );
    assert_eq!(
        Layout::new([0, 1, 4, 5, 8, 12], [2, 3, 6, 7, 10, 11], 0, 240),
        Err(LayoutError::InvalidKey(12))
    );
    assert_eq!(
        Layout::new(defaults.slot_keys(), defaults.action_keys(), 7, 240),
        Err(LayoutError::InvalidInset(7))
    );
    assert_eq!(
        Layout::new(defaults.slot_keys(), defaults.action_keys(), 0, 199),
        Err(LayoutError::InvalidWheelDiameter(199))
    );
    assert_eq!(
        Layout::new(defaults.slot_keys(), defaults.action_keys(), 0, 241),
        Err(LayoutError::InvalidWheelDiameter(241))
    );
    assert!(Slot::new(0).is_none());
    assert!(Slot::new(7).is_none());
}

fn luminance(color: Rgb) -> f64 {
    fn linear(channel: u8) -> f64 {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

fn contrast(a: Rgb, b: Rgb) -> f64 {
    let a = luminance(a);
    let b = luminance(b);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[test]
fn palette_and_refusal_words_match_the_ct_contract() {
    let vectors = [
        (
            TileState::Unassigned,
            "EMPTY",
            Rgb::new(48, 48, 48),
            Rgb::new(176, 176, 176),
        ),
        (
            TileState::Off,
            "OFF",
            Rgb::new(97, 97, 97),
            Rgb::new(255, 255, 255),
        ),
        (
            TileState::Idle,
            "IDLE",
            Rgb::new(255, 255, 255),
            Rgb::new(0, 0, 0),
        ),
        (
            TileState::Working,
            "WORKING",
            Rgb::new(32, 112, 255),
            Rgb::new(0, 0, 0),
        ),
        (
            TileState::Waiting,
            "WAITING",
            Rgb::new(255, 170, 0),
            Rgb::new(0, 0, 0),
        ),
        (
            TileState::Complete,
            "DONE",
            Rgb::new(0, 208, 96),
            Rgb::new(0, 0, 0),
        ),
        (
            TileState::Error,
            "ERROR",
            Rgb::new(255, 48, 48),
            Rgb::new(0, 0, 0),
        ),
        (
            TileState::Unknown,
            "UNKNOWN",
            Rgb::new(96, 125, 139),
            Rgb::new(0, 0, 0),
        ),
        (
            TileState::Busy,
            "BUSY",
            Rgb::new(106, 27, 154),
            Rgb::new(255, 255, 255),
        ),
        (
            TileState::Stale,
            "STALE",
            Rgb::new(93, 64, 55),
            Rgb::new(255, 213, 79),
        ),
        (
            TileState::Exited,
            "EXITED",
            Rgb::new(93, 64, 55),
            Rgb::new(255, 213, 79),
        ),
        (
            TileState::PidReused,
            "PID REUSED",
            Rgb::new(93, 64, 55),
            Rgb::new(255, 213, 79),
        ),
        (
            TileState::Unresolved,
            "NOT FOUND",
            Rgb::new(93, 64, 55),
            Rgb::new(255, 213, 79),
        ),
    ];
    for (state, word, fill, text) in vectors {
        let style = state.style();
        assert_eq!((style.word, style.fill, style.text), (word, fill, text));
    }
    let reasons = [
        (RefusalReason::Disabled, "DISABLED"),
        (RefusalReason::Unsupported, "UNSUPPORTED"),
        (RefusalReason::UnknownUi, "UNKNOWN UI"),
        (RefusalReason::StaleState, "STALE"),
        (RefusalReason::ClockMismatch, "CLOCK"),
        (RefusalReason::WrongTarget, "WRONG TARGET"),
        (RefusalReason::ChangedBinding, "REBOUND"),
        (RefusalReason::ChangedGeneration, "OUTDATED"),
        (RefusalReason::ChangedRun, "NEW RUN"),
        (RefusalReason::ChangedDraft, "DRAFT EDIT"),
        (RefusalReason::ChangedDialog, "NEW DIALOG"),
        (RefusalReason::CommandDraft, "COMMAND"),
        (RefusalReason::EmptyDraft, "EMPTY DRAFT"),
        (RefusalReason::Busy, "BUSY"),
        (RefusalReason::BroadApproval, "TOO BROAD"),
        (RefusalReason::Duplicate, "DUPLICATE"),
        (RefusalReason::IndeterminateDelivery, "UNCERTAIN"),
        (RefusalReason::PendingInput, "INPUT WAIT"),
        (RefusalReason::Unavailable, "UNAVAILABLE"),
        (RefusalReason::UnsafeContext, "UNSAFE"),
        (RefusalReason::ExpiredIntent, "EXPIRED"),
    ];
    for (reason, word) in reasons {
        assert_eq!(reason.word(), word);
    }
    assert_eq!(
        (REFUSAL_STYLE.word, REFUSAL_STYLE.fill, REFUSAL_STYLE.text),
        ("REFUSED", Rgb::new(191, 54, 12), Rgb::new(255, 255, 255))
    );
}

#[test]
fn palette_refusals_action_semantics_and_health_priority_are_distinct() {
    let states = [
        TileState::Unassigned,
        TileState::Off,
        TileState::Idle,
        TileState::Working,
        TileState::Waiting,
        TileState::Complete,
        TileState::Error,
        TileState::Unknown,
        TileState::Busy,
        TileState::Stale,
        TileState::Exited,
        TileState::PidReused,
        TileState::Unresolved,
    ];
    assert_eq!(
        states
            .iter()
            .map(|state| state.style().word)
            .collect::<HashSet<_>>()
            .len(),
        states.len()
    );
    for style in states
        .into_iter()
        .map(TileState::style)
        .chain([REFUSAL_STYLE])
    {
        assert!(
            contrast(style.fill, style.text) >= 4.5,
            "{}: {}",
            style.word,
            contrast(style.fill, style.text)
        );
    }
    let reasons = [
        RefusalReason::Disabled,
        RefusalReason::Unsupported,
        RefusalReason::UnknownUi,
        RefusalReason::StaleState,
        RefusalReason::ClockMismatch,
        RefusalReason::WrongTarget,
        RefusalReason::ChangedBinding,
        RefusalReason::ChangedGeneration,
        RefusalReason::ChangedRun,
        RefusalReason::ChangedDraft,
        RefusalReason::ChangedDialog,
        RefusalReason::CommandDraft,
        RefusalReason::EmptyDraft,
        RefusalReason::Busy,
        RefusalReason::BroadApproval,
        RefusalReason::Duplicate,
        RefusalReason::IndeterminateDelivery,
        RefusalReason::PendingInput,
        RefusalReason::Unavailable,
        RefusalReason::UnsafeContext,
        RefusalReason::ExpiredIntent,
    ];
    assert_eq!(
        reasons
            .iter()
            .map(|reason| reason.word())
            .collect::<HashSet<_>>()
            .len(),
        reasons.len()
    );
    assert_eq!(
        resolve_state(None, BindingHealth::Exited, true, true),
        TileState::Unassigned
    );
    assert_eq!(
        resolve_state(
            Some(SessionStatus::Working),
            BindingHealth::Exited,
            true,
            true
        ),
        TileState::Exited
    );
    assert_eq!(
        resolve_state(
            Some(SessionStatus::Working),
            BindingHealth::PidReused,
            true,
            true
        ),
        TileState::PidReused
    );
    assert_eq!(
        resolve_state(
            Some(SessionStatus::Working),
            BindingHealth::Unresolved,
            true,
            true
        ),
        TileState::Unresolved
    );
    assert_eq!(
        resolve_state(
            Some(SessionStatus::Working),
            BindingHealth::Live,
            true,
            true
        ),
        TileState::Stale
    );
    assert_eq!(
        resolve_state(
            Some(SessionStatus::Working),
            BindingHealth::Live,
            false,
            true
        ),
        TileState::Busy
    );
    assert_eq!(
        resolve_state(
            Some(SessionStatus::Unknown),
            BindingHealth::Live,
            false,
            false
        ),
        TileState::Unknown
    );
    assert_eq!(
        ActionTileKind::Run.semantic_action(),
        Some(SemanticAction::Autopilot)
    );
    assert_eq!(
        ActionTileKind::Send.semantic_action(),
        Some(SemanticAction::Send)
    );
    assert_eq!(ActionTileKind::Health.semantic_action(), None);

    let layout = Layout::default();
    for (health, word, fill, text) in [
        (
            Health::Unknown,
            "UNKNOWN",
            Rgb::new(69, 90, 100),
            Rgb::new(255, 255, 255),
        ),
        (
            Health::Ok,
            "OK",
            Rgb::new(27, 94, 32),
            Rgb::new(255, 255, 255),
        ),
        (
            Health::Warning,
            "WARNING",
            Rgb::new(249, 168, 37),
            Rgb::new(0, 0, 0),
        ),
        (
            Health::Error,
            "ERROR",
            Rgb::new(198, 40, 40),
            Rgb::new(255, 255, 255),
        ),
    ] {
        let mut view = populated();
        view.health = health;
        let tiles = layout.tiles(&view);
        assert!(matches!(&tiles[2], Tile::Action(action)
            if (action.band.word, action.band.fill, action.band.text)
                == ("READY", Rgb::new(27, 94, 32), Rgb::new(255, 255, 255))));
        assert!(matches!(&tiles[3], Tile::Action(action)
            if (action.band.word, action.band.fill, action.band.text)
                == ("NOT READY", Rgb::new(48, 48, 48), Rgb::new(176, 176, 176))));
        assert!(matches!(&tiles[11], Tile::Action(action)
            if (action.band.word, action.band.fill, action.band.text) == (word, fill, text)));
        for tile in tiles {
            if let Tile::Action(action) = tile {
                assert!(
                    contrast(action.band.fill, action.band.text) >= 4.5,
                    "{:?}",
                    action.band
                );
            }
        }
    }
}

#[test]
fn labels_and_feedback_keep_privacy_and_selection_separate() {
    let missing = prepare_label(slot(1), TileState::Idle, &Label::Missing);
    assert_eq!((missing.text.as_str(), missing.dim), ("Slot 1", true));
    let hidden = prepare_label(slot(2), TileState::Idle, &Label::Masked);
    assert_eq!((hidden.text.as_str(), hidden.dim), ("Title hidden", true));
    let blank = prepare_label(
        slot(3),
        TileState::Idle,
        &Label::Approved {
            text: "\t ".into(),
            available: true,
        },
    );
    assert_eq!((blank.text.as_str(), blank.dim), ("Slot 3", true));
    let rtl = prepare_label(
        slot(4),
        TileState::Exited,
        &Label::Approved {
            text: "\u{202e}\u{05d0}\u{05d1}".into(),
            available: true,
        },
    );
    assert_eq!(
        (rtl.text.as_str(), rtl.dim, rtl.direction),
        ("\u{05d0}\u{05d1}", true, Direction::RightToLeft)
    );

    let layout = Layout::default();
    let mut view = populated();
    view.pending = Some(slot(5));
    view.slots[0].refusal = Some(RefusalReason::StaleState);
    let tiles = layout.tiles(&view);
    assert!(
        matches!(&tiles[0], Tile::Slot(tile) if tile.selection == Selection::Confirmed && tile.refusal == Some(RefusalReason::StaleState))
    );
    assert!(matches!(&tiles[8], Tile::Slot(tile) if tile.selection == Selection::Pending));
    let scenes = layout.scenes(&view);
    assert!(scenes[0].operations.iter().any(|op| matches!(op, Operation::Frame { thickness: 3, color, dashed: false, .. } if *color == Rgb::new(255,255,255))));
    assert!(scenes[8].operations.iter().any(|op| matches!(op, Operation::Frame { thickness: 3, color, dashed: true, .. } if *color == Rgb::new(0,229,255))));
    assert!(texts(&scenes[0]).contains(&"SELECTED"));
    assert!(texts(&scenes[0]).contains(&"REFUSED\nSTALE"));
    assert!(texts(&scenes[8]).contains(&"PENDING"));
    assert!(texts(&scenes[14]).contains(&"PENDING"));
    assert!(texts(&scenes[14]).contains(&"5"));
    assert!(matches!(
        scenes[14].operations.last(),
        Some(Operation::MaskCircle { radius: 120 })
    ));

    view.pending = Some(slot(1));
    let tiles = layout.tiles(&view);
    assert!(matches!(&tiles[0], Tile::Slot(tile) if tile.selection == Selection::Pending));
}

#[test]
fn stale_and_neutral_views_and_changed_regions_follow_content() {
    let layout = Layout::default();
    let mut view = populated();
    let original = layout.scenes(&view);
    view.slots[1].state = TileState::Error;
    let next = layout.scenes(&view);
    assert_eq!(
        changed_scenes(&original, &next)
            .unwrap()
            .iter()
            .map(|scene| scene.region)
            .collect::<Vec<_>>(),
        vec![Region::key(1).unwrap()]
    );

    let original = layout.scenes(&view);
    view.slots[0].label = Label::Approved {
        text: "focused changed".into(),
        available: true,
    };
    let next = layout.scenes(&view);
    assert_eq!(
        changed_scenes(&original, &next)
            .unwrap()
            .iter()
            .map(|scene| scene.region)
            .collect::<Vec<_>>(),
        vec![Region::key(0).unwrap(), Region::Wheel]
    );

    let original = layout.scenes(&view);
    view.pending = Some(slot(5));
    let next = layout.scenes(&view);
    assert_eq!(
        changed_scenes(&original, &next)
            .unwrap()
            .iter()
            .map(|scene| scene.region)
            .collect::<Vec<_>>(),
        vec![Region::key(8).unwrap(), Region::Wheel]
    );

    view.slots[2].state = TileState::Unassigned;
    view.slots[1].refusal = Some(RefusalReason::ChangedBinding);
    view.make_stale();
    assert_eq!(view.confirmed, Some(slot(1)));
    assert_eq!(view.pending, None);
    assert_eq!(view.health, Health::Unknown);
    assert_eq!(view.commands, CommandAvailability::default());
    assert_eq!(view.slots[2].state, TileState::Unassigned);
    assert!(view.slots.iter().all(|slot| slot.refusal.is_none()));
    assert!(
        view.slots
            .iter()
            .enumerate()
            .all(|(index, slot)| index == 2 || slot.state == TileState::Stale)
    );
    assert!(
        matches!(&layout.tiles(&view)[0], Tile::Slot(tile) if tile.selection == Selection::Confirmed)
    );

    view.slots[0].label = Label::Approved {
        text: "sensitive synthetic title".into(),
        available: true,
    };
    view.neutral = true;
    let neutral = layout.scenes(&view);
    assert_eq!(neutral.len(), 15);
    assert!(
        neutral
            .iter()
            .all(|scene| scene.background == Rgb::default())
    );
    assert_eq!(
        neutral
            .iter()
            .map(|scene| scene.operations.len())
            .sum::<usize>(),
        6
    );
    for scene in &neutral {
        for operation in &scene.operations {
            match operation {
                Operation::Text(text) => {
                    assert!(matches!(
                        text.text.as_str(),
                        "1" | "2" | "3" | "4" | "5" | "6"
                    ));
                    assert_eq!(text.color, Rgb::new(150, 150, 150));
                }
                _ => panic!("neutral scene contains non-number content"),
            }
        }
    }
    assert!(neutral[14].operations.is_empty());
}

#[test]
fn no_selection_and_action_tiles_have_no_implicit_authority() {
    let layout = Layout::default();
    let view = View::default();
    let scenes = layout.scenes(&view);
    assert!(texts(&scenes[14]).contains(&"NO SELECTION"));
    assert!(texts(&scenes[14]).contains(&"\u{2013}"));
    assert!(scenes[14].operations.iter().any(|op| matches!(op, Operation::Ring { color, dashed: false, .. } if *color == Rgb::new(80, 80, 80))));
    for index in [2, 3, 6, 7, 10] {
        assert!(texts(&scenes[index]).contains(&"NOT READY"));
    }
    assert!(texts(&scenes[11]).contains(&"UNKNOWN"));
}
