//! CT tile models, exact layout, and offline scene construction.
//!
//! State, label consent, freshness, selection, feedback lifetime, and action
//! availability are supplied by the caller. A READY tile is never authority.

use super::Slot;
use super::pixels::{REGIONS, Region, Rgb};
use super::scene::{ActionGlyph, Alignment, Icon, Operation, Rect, Scene, Text, Weight};
use super::text::{Direction, prepare_text};

const BLACK: Rgb = Rgb::new(0, 0, 0);
const WHITE: Rgb = Rgb::new(255, 255, 255);
const KEY_BACKGROUND: Rgb = Rgb::new(18, 18, 18);
const NORMAL: Rgb = Rgb::new(240, 240, 240);
const DIM: Rgb = Rgb::new(150, 150, 150);
const PENDING: Rgb = Rgb::new(0, 229, 255);
const NO_SELECTION_RING: Rgb = Rgb::new(80, 80, 80);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Off,
    Idle,
    Working,
    Waiting,
    Complete,
    Error,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingHealth {
    Live,
    Exited,
    PidReused,
    Unresolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileState {
    Unassigned,
    Off,
    Idle,
    Working,
    Waiting,
    Complete,
    Error,
    Unknown,
    Busy,
    Stale,
    Exited,
    PidReused,
    Unresolved,
}

/// Binding health takes priority over freshness, then an in-flight operation.
pub fn resolve_state(
    status: Option<SessionStatus>,
    health: BindingHealth,
    stale: bool,
    busy: bool,
) -> TileState {
    let Some(status) = status else {
        return TileState::Unassigned;
    };
    match health {
        BindingHealth::Exited => return TileState::Exited,
        BindingHealth::PidReused => return TileState::PidReused,
        BindingHealth::Unresolved => return TileState::Unresolved,
        BindingHealth::Live => {}
    }
    if stale {
        return TileState::Stale;
    }
    if busy {
        return TileState::Busy;
    }
    match status {
        SessionStatus::Off => TileState::Off,
        SessionStatus::Idle => TileState::Idle,
        SessionStatus::Working => TileState::Working,
        SessionStatus::Waiting => TileState::Waiting,
        SessionStatus::Complete => TileState::Complete,
        SessionStatus::Error => TileState::Error,
        SessionStatus::Unknown => TileState::Unknown,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BandStyle {
    pub word: &'static str,
    pub fill: Rgb,
    pub text: Rgb,
}

impl TileState {
    pub const fn style(self) -> BandStyle {
        match self {
            Self::Unassigned => BandStyle {
                word: "EMPTY",
                fill: Rgb::new(48, 48, 48),
                text: Rgb::new(176, 176, 176),
            },
            Self::Off => BandStyle {
                word: "OFF",
                fill: Rgb::new(97, 97, 97),
                text: WHITE,
            },
            Self::Idle => BandStyle {
                word: "IDLE",
                fill: WHITE,
                text: BLACK,
            },
            Self::Working => BandStyle {
                word: "WORKING",
                fill: Rgb::new(32, 112, 255),
                text: BLACK,
            },
            Self::Waiting => BandStyle {
                word: "WAITING",
                fill: Rgb::new(255, 170, 0),
                text: BLACK,
            },
            Self::Complete => BandStyle {
                word: "DONE",
                fill: Rgb::new(0, 208, 96),
                text: BLACK,
            },
            Self::Error => BandStyle {
                word: "ERROR",
                fill: Rgb::new(255, 48, 48),
                text: BLACK,
            },
            Self::Unknown => BandStyle {
                word: "UNKNOWN",
                fill: Rgb::new(96, 125, 139),
                text: BLACK,
            },
            Self::Busy => BandStyle {
                word: "BUSY",
                fill: Rgb::new(106, 27, 154),
                text: WHITE,
            },
            Self::Stale => unavailable_style("STALE"),
            Self::Exited => unavailable_style("EXITED"),
            Self::PidReused => unavailable_style("PID REUSED"),
            Self::Unresolved => unavailable_style("NOT FOUND"),
        }
    }

    fn unavailable(self) -> bool {
        matches!(
            self,
            Self::Stale | Self::Exited | Self::PidReused | Self::Unresolved
        )
    }
}

const fn unavailable_style(word: &'static str) -> BandStyle {
    BandStyle {
        word,
        fill: Rgb::new(93, 64, 55),
        text: Rgb::new(255, 213, 79),
    }
}

pub const REFUSAL_STYLE: BandStyle = BandStyle {
    word: "REFUSED",
    fill: Rgb::new(191, 54, 12),
    text: WHITE,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalReason {
    Disabled,
    Unsupported,
    UnknownUi,
    StaleState,
    ClockMismatch,
    WrongTarget,
    ChangedBinding,
    ChangedGeneration,
    ChangedRun,
    ChangedDraft,
    ChangedDialog,
    CommandDraft,
    EmptyDraft,
    Busy,
    BroadApproval,
    Duplicate,
    IndeterminateDelivery,
    PendingInput,
    Unavailable,
    UnsafeContext,
    ExpiredIntent,
}

impl RefusalReason {
    pub const fn word(self) -> &'static str {
        match self {
            Self::Disabled => "DISABLED",
            Self::Unsupported => "UNSUPPORTED",
            Self::UnknownUi => "UNKNOWN UI",
            Self::StaleState => "STALE",
            Self::ClockMismatch => "CLOCK",
            Self::WrongTarget => "WRONG TARGET",
            Self::ChangedBinding => "REBOUND",
            Self::ChangedGeneration => "OUTDATED",
            Self::ChangedRun => "NEW RUN",
            Self::ChangedDraft => "DRAFT EDIT",
            Self::ChangedDialog => "NEW DIALOG",
            Self::CommandDraft => "COMMAND",
            Self::EmptyDraft => "EMPTY DRAFT",
            Self::Busy => "BUSY",
            Self::BroadApproval => "TOO BROAD",
            Self::Duplicate => "DUPLICATE",
            Self::IndeterminateDelivery => "UNCERTAIN",
            Self::PendingInput => "INPUT WAIT",
            Self::Unavailable => "UNAVAILABLE",
            Self::UnsafeContext => "UNSAFE",
            Self::ExpiredIntent => "EXPIRED",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Label {
    Missing,
    Masked,
    Approved { text: String, available: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedLabel {
    pub text: String,
    pub dim: bool,
    pub direction: Direction,
    pub truncated: bool,
    pub substituted: bool,
}

pub fn prepare_label(slot: Slot, state: TileState, label: &Label) -> PreparedLabel {
    let fallback = || PreparedLabel {
        text: format!("Slot {}", slot.number()),
        dim: true,
        direction: Direction::LeftToRight,
        truncated: false,
        substituted: false,
    };
    match label {
        Label::Missing => fallback(),
        Label::Masked => PreparedLabel {
            text: "Title hidden".to_owned(),
            dim: true,
            direction: Direction::LeftToRight,
            truncated: false,
            substituted: false,
        },
        Label::Approved { text, available } => {
            let prepared = prepare_text(text, 40);
            if prepared.text.is_empty() {
                let mut result = fallback();
                result.truncated = prepared.truncated;
                result.substituted = prepared.substituted;
                result
            } else {
                PreparedLabel {
                    text: prepared.text,
                    dim: !available || state.unavailable(),
                    direction: prepared.direction,
                    truncated: prepared.truncated,
                    substituted: prepared.substituted,
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    None,
    Confirmed,
    Pending,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotView {
    pub state: TileState,
    pub label: Label,
    pub refusal: Option<RefusalReason>,
}

impl Default for SlotView {
    fn default() -> Self {
        Self {
            state: TileState::Unassigned,
            label: Label::Missing,
            refusal: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandAvailability {
    pub run: bool,
    pub accept: bool,
    pub stop: bool,
    pub fork: bool,
    pub send: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Health {
    #[default]
    Unknown,
    Ok,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub slots: [SlotView; 6],
    pub confirmed: Option<Slot>,
    pub pending: Option<Slot>,
    pub commands: CommandAvailability,
    pub health: Health,
    pub neutral: bool,
}

impl Default for View {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| SlotView::default()),
            confirmed: None,
            pending: None,
            commands: CommandAvailability::default(),
            health: Health::Unknown,
            neutral: false,
        }
    }
}

impl View {
    /// Marks all assigned slots stale without erasing confirmed selection.
    pub fn make_stale(&mut self) {
        for slot in &mut self.slots {
            if slot.state != TileState::Unassigned {
                slot.state = TileState::Stale;
            }
            slot.refusal = None;
        }
        self.pending = None;
        self.commands = CommandAvailability::default();
        self.health = Health::Unknown;
        self.neutral = false;
    }

    pub fn neutral() -> Self {
        Self {
            neutral: true,
            ..Self::default()
        }
    }

    fn selection(&self, slot: Slot) -> Selection {
        if self.pending == Some(slot) {
            Selection::Pending
        } else if self.confirmed == Some(slot) {
            Selection::Confirmed
        } else {
            Selection::None
        }
    }

    fn slot_tile(&self, slot: Slot) -> SlotTile {
        let view = &self.slots[slot.index()];
        SlotTile {
            slot,
            state: view.state,
            label: prepare_label(slot, view.state, &view.label),
            selection: self.selection(slot),
            refusal: view.refusal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionTileKind {
    Run,
    Accept,
    Stop,
    Fork,
    Send,
    Health,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticAction {
    Autopilot,
    Accept,
    Stop,
    Fork,
    Send,
}

impl ActionTileKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Run => "Run",
            Self::Accept => "Accept",
            Self::Stop => "Stop",
            Self::Fork => "Fork",
            Self::Send => "Send",
            Self::Health => "Health",
        }
    }

    pub const fn semantic_action(self) -> Option<SemanticAction> {
        match self {
            Self::Run => Some(SemanticAction::Autopilot),
            Self::Accept => Some(SemanticAction::Accept),
            Self::Stop => Some(SemanticAction::Stop),
            Self::Fork => Some(SemanticAction::Fork),
            Self::Send => Some(SemanticAction::Send),
            Self::Health => None,
        }
    }

    fn glyph(self) -> ActionGlyph {
        match self {
            Self::Run => ActionGlyph::Run,
            Self::Accept => ActionGlyph::Accept,
            Self::Stop => ActionGlyph::Stop,
            Self::Fork => ActionGlyph::Fork,
            Self::Send => ActionGlyph::Send,
            Self::Health => ActionGlyph::Health,
        }
    }
}

const ACTIONS: [ActionTileKind; 6] = [
    ActionTileKind::Run,
    ActionTileKind::Accept,
    ActionTileKind::Stop,
    ActionTileKind::Fork,
    ActionTileKind::Send,
    ActionTileKind::Health,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotTile {
    pub slot: Slot,
    pub state: TileState,
    pub label: PreparedLabel,
    pub selection: Selection,
    pub refusal: Option<RefusalReason>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionTile {
    pub action: ActionTileKind,
    pub band: BandStyle,
    pub foreground: Rgb,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tile {
    Slot(SlotTile),
    Action(ActionTile),
    Neutral { slot: Option<Slot> },
}

fn action_tile(view: &View, action: ActionTileKind) -> ActionTile {
    let available = match action {
        ActionTileKind::Run => view.commands.run,
        ActionTileKind::Accept => view.commands.accept,
        ActionTileKind::Stop => view.commands.stop,
        ActionTileKind::Fork => view.commands.fork,
        ActionTileKind::Send => view.commands.send,
        ActionTileKind::Health => view.health != Health::Unknown,
    };
    let band = if action == ActionTileKind::Health {
        match view.health {
            Health::Unknown => BandStyle {
                word: "UNKNOWN",
                fill: Rgb::new(69, 90, 100),
                text: WHITE,
            },
            Health::Ok => ready_style("OK"),
            Health::Warning => BandStyle {
                word: "WARNING",
                fill: Rgb::new(249, 168, 37),
                text: BLACK,
            },
            Health::Error => BandStyle {
                word: "ERROR",
                fill: Rgb::new(198, 40, 40),
                text: WHITE,
            },
        }
    } else if available {
        ready_style("READY")
    } else {
        BandStyle {
            word: "NOT READY",
            fill: Rgb::new(48, 48, 48),
            text: Rgb::new(176, 176, 176),
        }
    };
    ActionTile {
        action,
        band,
        foreground: if available { NORMAL } else { DIM },
    }
}

const fn ready_style(word: &'static str) -> BandStyle {
    BandStyle {
        word,
        fill: Rgb::new(27, 94, 32),
        text: WHITE,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutError {
    InvalidKey(u8),
    DuplicateKey(u8),
    InvalidInset(u8),
    InvalidWheelDiameter(u16),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    slot_keys: [u8; 6],
    action_keys: [u8; 6],
    inset: u8,
    wheel_diameter: u16,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            slot_keys: [0, 1, 4, 5, 8, 9],
            action_keys: [2, 3, 6, 7, 10, 11],
            inset: 0,
            wheel_diameter: 240,
        }
    }
}

impl Layout {
    pub fn new(
        slot_keys: [u8; 6],
        action_keys: [u8; 6],
        inset: u8,
        wheel_diameter: u16,
    ) -> Result<Self, LayoutError> {
        if inset > 6 {
            return Err(LayoutError::InvalidInset(inset));
        }
        if !(200..=240).contains(&wheel_diameter) {
            return Err(LayoutError::InvalidWheelDiameter(wheel_diameter));
        }
        let mut used = [false; 12];
        for key in slot_keys.into_iter().chain(action_keys) {
            if key >= 12 {
                return Err(LayoutError::InvalidKey(key));
            }
            if used[usize::from(key)] {
                return Err(LayoutError::DuplicateKey(key));
            }
            used[usize::from(key)] = true;
        }
        Ok(Self {
            slot_keys,
            action_keys,
            inset,
            wheel_diameter,
        })
    }

    pub const fn slot_keys(&self) -> [u8; 6] {
        self.slot_keys
    }

    pub const fn action_keys(&self) -> [u8; 6] {
        self.action_keys
    }

    pub const fn inset(&self) -> u8 {
        self.inset
    }

    pub const fn wheel_diameter(&self) -> u16 {
        self.wheel_diameter
    }

    pub fn key_geometry(&self) -> KeyGeometry {
        let inset = u16::from(self.inset);
        let safe = Rect::new(inset, inset, 90 - 2 * inset, 90 - 2 * inset);
        let content = Rect::new(inset + 4, inset + 4, 82 - 2 * inset, 82 - 2 * inset);
        let number = Rect::new(content.x + 1, content.y, 18, 18);
        let marker = Rect::new(content.x + 19, content.y + 3, content.width - 20, 12);
        let band = Rect::new(
            content.x,
            content.y + content.height - 27,
            content.width,
            27,
        );
        let label_area_y = content.y + 19;
        let label_height = band.y - 2 - label_area_y;
        let label_lines = if label_height >= 32 { 2 } else { 1 };
        let used_height = 16 * u16::from(label_lines);
        let label = Rect::new(
            content.x + 1,
            label_area_y + (label_height - used_height) / 2,
            content.width - 2,
            used_height,
        );
        let action_height = band.y - 2 - content.y;
        let icon_size = 40_u16.min(action_height - 16 - 1);
        let combined_height = icon_size + 1 + 16;
        let icon_y = content.y + (action_height - combined_height) / 2;
        let action_icon = Rect::new(
            content.x + (content.width - icon_size) / 2,
            icon_y,
            icon_size,
            icon_size,
        );
        let action_name = Rect::new(content.x + 1, icon_y + icon_size + 1, content.width - 2, 16);
        KeyGeometry {
            safe,
            content,
            number,
            marker,
            band,
            label,
            label_lines,
            action_icon,
            action_name,
        }
    }

    pub fn wheel_geometry(&self) -> WheelGeometry {
        let radius = self.wheel_diameter / 2;
        let safe = radius - 12;
        let scaled = |value: u16| scale(value, safe);
        let marker_line = scaled(16);
        let number_line = scaled(48);
        let label_line = scaled(19);
        let marker = circle_rect(safe, 120 - i32::from(scaled(88)), marker_line, scaled(140));
        let number = circle_rect(safe, 120 - i32::from(scaled(70)), number_line, scaled(160));
        let label = circle_rect(
            safe,
            120 - i32::from(scaled(18)),
            label_line * 2,
            scaled(180),
        );
        let pill = circle_rect(safe, 120 + i32::from(scaled(26)), scaled(34), scaled(150));
        WheelGeometry {
            radius,
            inner_radius: radius - 8,
            safe_radius: safe,
            marker,
            marker_font: scaled(12) as u8,
            marker_line,
            number,
            number_font: scaled(40) as u8,
            number_line,
            label,
            label_font: scaled(14) as u8,
            label_line,
            pill,
            normal_font: scaled(13) as u8,
            normal_line: scaled(18),
            refusal_font: scaled(11) as u8,
            refusal_line: scaled(15),
        }
    }

    pub fn tiles(&self, view: &View) -> [Tile; 12] {
        std::array::from_fn(|key| {
            if let Some(index) = self
                .slot_keys
                .iter()
                .position(|&value| usize::from(value) == key)
            {
                let slot = Slot::new(index as u8 + 1).expect("six layout slots");
                if view.neutral {
                    Tile::Neutral { slot: Some(slot) }
                } else {
                    Tile::Slot(view.slot_tile(slot))
                }
            } else {
                let index = self
                    .action_keys
                    .iter()
                    .position(|&value| usize::from(value) == key)
                    .expect("validated layout covers every key");
                if view.neutral {
                    Tile::Neutral { slot: None }
                } else {
                    Tile::Action(action_tile(view, ACTIONS[index]))
                }
            }
        })
    }

    /// Returns all 15 independent scenes in the stable region order.
    pub fn scenes(&self, view: &View) -> Vec<Scene> {
        let key_geometry = self.key_geometry();
        let tiles = self.tiles(view);
        let mut scenes = Vec::with_capacity(REGIONS.len());
        for (index, tile) in tiles.iter().enumerate() {
            scenes.push(key_scene(
                Region::key(index as u8).expect("twelve layout keys"),
                &key_geometry,
                tile,
            ));
        }
        scenes.push(Scene {
            region: Region::LeftStrip,
            background: BLACK,
            operations: Vec::new(),
        });
        scenes.push(Scene {
            region: Region::RightStrip,
            background: BLACK,
            operations: Vec::new(),
        });
        scenes.push(wheel_scene(self.wheel_geometry(), view));
        scenes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyGeometry {
    pub safe: Rect,
    pub content: Rect,
    pub number: Rect,
    pub marker: Rect,
    pub band: Rect,
    pub label: Rect,
    pub label_lines: u8,
    pub action_icon: Rect,
    pub action_name: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WheelGeometry {
    pub radius: u16,
    pub inner_radius: u16,
    pub safe_radius: u16,
    pub marker: Rect,
    pub marker_font: u8,
    pub marker_line: u16,
    pub number: Rect,
    pub number_font: u8,
    pub number_line: u16,
    pub label: Rect,
    pub label_font: u8,
    pub label_line: u16,
    pub pill: Rect,
    pub normal_font: u8,
    pub normal_line: u16,
    pub refusal_font: u8,
    pub refusal_line: u16,
}

fn scale(value: u16, safe: u16) -> u16 {
    ((u32::from(value) * u32::from(safe) + 54) / 108) as u16
}

fn integer_sqrt(number: u32) -> u16 {
    let mut result = 0_u16;
    while u32::from(result + 1) * u32::from(result + 1) <= number {
        result += 1;
    }
    result
}

fn circle_rect(safe: u16, top: i32, height: u16, maximum_width: u16) -> Rect {
    let dy = (top - 120).abs().max((top + i32::from(height) - 120).abs()) as u32;
    let circle_half = integer_sqrt(u32::from(safe).pow(2).saturating_sub(dy.pow(2)));
    let half = circle_half.min(maximum_width / 2);
    Rect::new(120 - half, top as u16, half * 2, height)
}

#[derive(Clone, Copy)]
struct TypeStyle {
    size: u8,
    weight: Weight,
    line_height: u16,
    lines: u8,
    alignment: Alignment,
}

const fn type_style(
    size: u8,
    weight: Weight,
    line_height: u16,
    lines: u8,
    alignment: Alignment,
) -> TypeStyle {
    TypeStyle {
        size,
        weight,
        line_height,
        lines,
        alignment,
    }
}

fn add_text(
    operations: &mut Vec<Operation>,
    rect: Rect,
    text: impl Into<String>,
    color: Rgb,
    style: TypeStyle,
    direction: Direction,
) {
    operations.push(Operation::Text(Text {
        rect,
        text: text.into(),
        color,
        font_size: style.size,
        weight: style.weight,
        line_height: style.line_height,
        lines: style.lines,
        alignment: style.alignment,
        direction,
    }));
}

fn selection_details(selection: Selection) -> Option<(&'static str, Rgb, bool)> {
    match selection {
        Selection::None => None,
        Selection::Confirmed => Some(("SELECTED", WHITE, false)),
        Selection::Pending => Some(("PENDING", PENDING, true)),
    }
}

fn band_text(
    operations: &mut Vec<Operation>,
    band: Rect,
    style: BandStyle,
    refusal: Option<RefusalReason>,
) {
    operations.push(Operation::Fill {
        rect: band,
        color: style.fill,
    });
    let (text, font, line_height, lines) = if let Some(reason) = refusal {
        (format!("REFUSED\n{}", reason.word()), 10, 13, 2)
    } else {
        (style.word.to_owned(), 12, 16, 1)
    };
    let height = line_height * u16::from(lines);
    add_text(
        operations,
        Rect::new(
            band.x + 1,
            band.y + (band.height - height) / 2,
            band.width - 2,
            height,
        ),
        text,
        style.text,
        type_style(font, Weight::Bold, line_height, lines, Alignment::Center),
        Direction::LeftToRight,
    );
}

fn key_scene(region: Region, geometry: &KeyGeometry, tile: &Tile) -> Scene {
    let mut operations = Vec::new();
    let background = if matches!(tile, Tile::Neutral { .. }) {
        BLACK
    } else {
        KEY_BACKGROUND
    };
    match tile {
        Tile::Neutral { slot } => {
            if let Some(slot) = slot {
                add_text(
                    &mut operations,
                    geometry.number,
                    slot.number().to_string(),
                    DIM,
                    type_style(14, Weight::Bold, 18, 1, Alignment::Leading),
                    Direction::LeftToRight,
                );
            }
        }
        Tile::Slot(slot) => {
            if let Some((word, color, dashed)) = selection_details(slot.selection) {
                operations.push(Operation::Frame {
                    rect: geometry.safe,
                    thickness: 3,
                    color,
                    dashed,
                });
                add_text(
                    &mut operations,
                    geometry.marker,
                    word,
                    color,
                    type_style(9, Weight::Bold, 12, 1, Alignment::Trailing),
                    Direction::LeftToRight,
                );
            }
            add_text(
                &mut operations,
                geometry.number,
                slot.slot.number().to_string(),
                NORMAL,
                type_style(14, Weight::Bold, 18, 1, Alignment::Leading),
                Direction::LeftToRight,
            );
            add_text(
                &mut operations,
                geometry.label,
                slot.label.text.clone(),
                if slot.label.dim { DIM } else { NORMAL },
                type_style(
                    12,
                    Weight::Normal,
                    16,
                    geometry.label_lines,
                    Alignment::Center,
                ),
                slot.label.direction,
            );
            let style = if slot.refusal.is_some() {
                REFUSAL_STYLE
            } else {
                slot.state.style()
            };
            band_text(&mut operations, geometry.band, style, slot.refusal);
        }
        Tile::Action(action) => {
            operations.push(Operation::Icon(Icon {
                rect: geometry.action_icon,
                glyph: action.action.glyph(),
                color: action.foreground,
            }));
            add_text(
                &mut operations,
                geometry.action_name,
                action.action.name(),
                action.foreground,
                type_style(12, Weight::Bold, 16, 1, Alignment::Center),
                Direction::LeftToRight,
            );
            band_text(&mut operations, geometry.band, action.band, None);
        }
    }
    Scene {
        region,
        background,
        operations,
    }
}

fn wheel_scene(geometry: WheelGeometry, view: &View) -> Scene {
    if view.neutral {
        return Scene {
            region: Region::Wheel,
            background: BLACK,
            operations: Vec::new(),
        };
    }
    let focused = view.pending.or(view.confirmed);
    let selection = focused.map_or(Selection::None, |slot| view.selection(slot));
    let (marker, ring_color, dashed) =
        selection_details(selection).unwrap_or(("NO SELECTION", NO_SELECTION_RING, false));
    let mut operations = vec![Operation::Ring {
        inner: geometry.inner_radius,
        outer: geometry.radius,
        color: ring_color,
        dashed,
    }];
    add_text(
        &mut operations,
        geometry.marker,
        marker,
        if selection == Selection::None {
            DIM
        } else {
            ring_color
        },
        type_style(
            geometry.marker_font,
            Weight::Bold,
            geometry.marker_line,
            1,
            Alignment::Center,
        ),
        Direction::LeftToRight,
    );
    add_text(
        &mut operations,
        geometry.number,
        focused.map_or_else(|| "\u{2013}".to_owned(), |slot| slot.number().to_string()),
        if focused.is_some() { NORMAL } else { DIM },
        type_style(
            geometry.number_font,
            Weight::Bold,
            geometry.number_line,
            1,
            Alignment::Center,
        ),
        Direction::LeftToRight,
    );
    if let Some(slot) = focused {
        let tile = view.slot_tile(slot);
        add_text(
            &mut operations,
            geometry.label,
            tile.label.text,
            if tile.label.dim { DIM } else { NORMAL },
            type_style(
                geometry.label_font,
                Weight::Normal,
                geometry.label_line,
                2,
                Alignment::Center,
            ),
            tile.label.direction,
        );
        let style = if tile.refusal.is_some() {
            REFUSAL_STYLE
        } else {
            tile.state.style()
        };
        operations.push(Operation::Fill {
            rect: geometry.pill,
            color: style.fill,
        });
        let (text, font, line, lines) = if let Some(reason) = tile.refusal {
            (
                format!("REFUSED\n{}", reason.word()),
                geometry.refusal_font,
                geometry.refusal_line,
                2,
            )
        } else {
            (
                style.word.to_owned(),
                geometry.normal_font,
                geometry.normal_line,
                1,
            )
        };
        let height = line * u16::from(lines);
        add_text(
            &mut operations,
            Rect::new(
                geometry.pill.x + 2,
                geometry.pill.y + (geometry.pill.height - height) / 2,
                geometry.pill.width - 4,
                height,
            ),
            text,
            style.text,
            type_style(font, Weight::Bold, line, lines, Alignment::Center),
            Direction::LeftToRight,
        );
    }
    operations.push(Operation::MaskCircle {
        radius: geometry.radius,
    });
    Scene {
        region: Region::Wheel,
        background: KEY_BACKGROUND,
        operations,
    }
}
