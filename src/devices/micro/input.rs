use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::calibration::{Calibration, Direction};
use super::error::MicroError;
use super::json::{numeric, numeric_equals_u64};

pub const PROFILE_ID: &str = "micro-2-pro";
pub const SLOT_DEBOUNCE: Duration = Duration::from_millis(300);
pub const COMMAND_DEBOUNCE: Duration = Duration::from_millis(500);
pub const MICROPHONE_DEBOUNCE: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Slot(u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidSlot;

impl fmt::Display for InvalidSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("slot must be in 1..=6")
    }
}

impl std::error::Error for InvalidSlot {}

impl Slot {
    pub const ALL: [Slot; 6] = [Slot(1), Slot(2), Slot(3), Slot(4), Slot(5), Slot(6)];

    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Slot {
    type Error = InvalidSlot;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if (1..=6).contains(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidSlot)
        }
    }
}

impl From<Slot> for u8 {
    fn from(value: Slot) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    None,
    Autopilot,
    Accept,
    Stop,
    Fork,
    Send,
    ModelPicker,
    Up,
    Right,
    Down,
    Left,
}

impl Action {
    fn allowed_for_dpad(self) -> bool {
        matches!(
            self,
            Self::None | Self::Up | Self::Right | Self::Down | Self::Left
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommandKey {
    Act06,
    Act07,
    Act08,
    Act09,
    Act12,
    DpadUp,
    DpadRight,
    DpadDown,
    DpadLeft,
}

impl CommandKey {
    pub const ALL: [Self; 9] = [
        Self::Act06,
        Self::Act07,
        Self::Act08,
        Self::Act09,
        Self::Act12,
        Self::DpadUp,
        Self::DpadRight,
        Self::DpadDown,
        Self::DpadLeft,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Act06 => "ACT06",
            Self::Act07 => "ACT07",
            Self::Act08 => "ACT08",
            Self::Act09 => "ACT09",
            Self::Act12 => "ACT12",
            Self::DpadUp => "DPAD_UP",
            Self::DpadRight => "DPAD_RIGHT",
            Self::DpadDown => "DPAD_DOWN",
            Self::DpadLeft => "DPAD_LEFT",
        }
    }

    pub const fn default_action(self) -> Action {
        match self {
            Self::Act06 => Action::Autopilot,
            Self::Act07 => Action::Accept,
            Self::Act08 => Action::Stop,
            Self::Act09 => Action::Fork,
            Self::Act12 => Action::Send,
            Self::DpadUp => Action::Up,
            Self::DpadRight => Action::Right,
            Self::DpadDown => Action::Down,
            Self::DpadLeft => Action::Left,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Act06 => "Run",
            Self::Act07 => "A",
            Self::Act08 => "B",
            Self::Act09 => "Branch",
            Self::Act12 => "Send",
            Self::DpadUp => "D-pad Up",
            Self::DpadRight => "D-pad Right",
            Self::DpadDown => "D-pad Down",
            Self::DpadLeft => "D-pad Left",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.name() == name)
    }

    const fn is_dpad(self) -> bool {
        matches!(
            self,
            Self::DpadUp | Self::DpadRight | Self::DpadDown | Self::DpadLeft
        )
    }
}

impl From<Direction> for CommandKey {
    fn from(direction: Direction) -> Self {
        match direction {
            Direction::Up => Self::DpadUp,
            Direction::Right => Self::DpadRight,
            Direction::Down => Self::DpadDown,
            Direction::Left => Self::DpadLeft,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingError(pub String);

impl fmt::Display for BindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Invalid Micro binding: {}", self.0)
    }
}

impl std::error::Error for BindingError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    entries: BTreeMap<CommandKey, Action>,
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            entries: CommandKey::ALL
                .into_iter()
                .map(|key| (key, key.default_action()))
                .collect(),
        }
    }
}

impl Bindings {
    pub fn empty() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// Validates a complete supplied map; keys not supplied are unbound.
    pub fn from_entries(
        entries: impl IntoIterator<Item = (String, Action)>,
    ) -> Result<Self, BindingError> {
        let mut result = Self::empty();
        for (name, action) in entries {
            let key = CommandKey::from_name(&name).ok_or_else(|| BindingError(name.clone()))?;
            if key.is_dpad() && !action.allowed_for_dpad() {
                return Err(BindingError(name));
            }
            if result.entries.insert(key, action).is_some() {
                return Err(BindingError(name));
            }
        }
        Ok(result)
    }

    /// Validates a partial override object over the nine default bindings.
    /// The caller is responsible for duplicate-key validation during JSON parsing.
    pub fn from_overrides(value: &Value) -> Result<Self, BindingError> {
        let object = value
            .as_object()
            .ok_or_else(|| BindingError("bindings must be an object".to_owned()))?;
        let mut parsed = Vec::new();
        for (name, value) in object {
            let action = serde_json::from_value::<Action>(value.clone())
                .map_err(|_| BindingError(name.clone()))?;
            parsed.push((name.clone(), action));
        }
        let overrides = Self::from_entries(parsed)?;
        let mut result = Self::default();
        result.entries.extend(overrides.entries);
        Ok(result)
    }

    pub fn get(&self, key: CommandKey) -> Option<Action> {
        self.entries.get(&key).copied()
    }

    fn decision(&self, key: CommandKey, controls_enabled: bool) -> CommandOutput {
        let disposition = match self.get(key) {
            None | Some(Action::None) => CommandDisposition::Unbound,
            Some(_) if !controls_enabled => CommandDisposition::ControlsDisabled,
            Some(action) => CommandDisposition::Action(action),
        };
        CommandOutput { key, disposition }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidBrightness;

impl fmt::Display for InvalidBrightness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("brightness must be a finite number in [0, 1]")
    }
}

impl std::error::Error for InvalidBrightness {}

/// Micro-relevant values only; the shared settings schema and persistence are external.
#[derive(Debug, Clone, PartialEq)]
pub struct MicroSettings {
    pub brightness: f64,
    pub controls_enabled: bool,
    pub voice_enabled: bool,
    pub auto_bind: bool,
    pub bindings: Bindings,
    pub calibration: Calibration,
}

impl Default for MicroSettings {
    fn default() -> Self {
        Self {
            brightness: 0.25,
            controls_enabled: false,
            voice_enabled: false,
            auto_bind: false,
            bindings: Bindings::default(),
            calibration: Calibration::default(),
        }
    }
}

impl MicroSettings {
    pub fn recommended() -> Self {
        Self {
            brightness: 0.75,
            controls_enabled: true,
            auto_bind: true,
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<(), InvalidBrightness> {
        if self.brightness.is_finite() && (0.0..=1.0).contains(&self.brightness) {
            Ok(())
        } else {
            Err(InvalidBrightness)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Press,
    Turn,
    Stick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecommendedIntent {
    SelectSlot(Slot),
    MoveCursor,
    CommitCursor,
    Command(Action),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlDeclaration {
    pub id: &'static str,
    pub kind: ControlKind,
    pub recommended: RecommendedIntent,
}

pub fn declared_controls() -> Vec<ControlDeclaration> {
    let mut controls = Vec::with_capacity(17);
    for (id, slot) in SLOT_KEYS.into_iter().zip(Slot::ALL) {
        controls.push(ControlDeclaration {
            id,
            kind: ControlKind::Press,
            recommended: RecommendedIntent::SelectSlot(slot),
        });
    }
    controls.push(ControlDeclaration {
        id: "encoder_turn",
        kind: ControlKind::Turn,
        recommended: RecommendedIntent::MoveCursor,
    });
    controls.push(ControlDeclaration {
        id: "encoder_press",
        kind: ControlKind::Press,
        recommended: RecommendedIntent::CommitCursor,
    });
    for key in CommandKey::ALL {
        controls.push(ControlDeclaration {
            id: key.name(),
            kind: if key.is_dpad() {
                ControlKind::Stick
            } else {
                ControlKind::Press
            },
            recommended: RecommendedIntent::Command(key.default_action()),
        });
    }
    controls
}

/// A decoded binding result, never permission to dispatch an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandDisposition {
    Unbound,
    ControlsDisabled,
    Action(Action),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandOutput {
    pub key: CommandKey,
    pub disposition: CommandDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Telemetry {
    pub angle: f64,
    pub distance: f64,
    pub timestamp: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LegacyOutput {
    pub command: Option<CommandOutput>,
    pub microphone: bool,
    pub slot: Option<Slot>,
    pub telemetry: Option<Telemetry>,
}

pub struct LegacyContext<'a> {
    pub monotonic: Duration,
    pub wall_seconds: f64,
    pub bindings: &'a Bindings,
    pub controls_enabled: bool,
    /// None means all six; Some(&[]) means none.
    pub active_slots: Option<&'a [Slot]>,
    pub selected_slot: Option<Slot>,
}

const SLOT_KEYS: [&str; 6] = ["AG00", "AG01", "AG02", "AG03", "AG04", "AG05"];

#[derive(Debug)]
pub struct LegacyDecoder {
    slot_down: [bool; 6],
    command_down: BTreeSet<CommandKey>,
    last_accepted: BTreeMap<&'static str, Duration>,
    mic_down: [bool; 2],
    mic_last: Option<Duration>,
    stick_armed: bool,
    telemetry: Option<Telemetry>,
    calibration: Calibration,
}

impl LegacyDecoder {
    pub fn from_settings(settings: &MicroSettings) -> Self {
        Self::new(settings.calibration.clone())
    }

    pub fn new(calibration: Calibration) -> Self {
        Self {
            slot_down: [false; 6],
            command_down: BTreeSet::new(),
            last_accepted: BTreeMap::new(),
            mic_down: [false; 2],
            mic_last: None,
            stick_armed: true,
            telemetry: None,
            calibration,
        }
    }

    pub fn set_calibration(&mut self, calibration: Calibration) {
        self.calibration = calibration;
    }

    pub fn telemetry(&self) -> Option<Telemetry> {
        self.telemetry
    }

    pub fn stick_armed(&self) -> bool {
        self.stick_armed
    }

    fn accept_debounce(&mut self, key: &'static str, now: Duration, window: Duration) -> bool {
        let allowed = self.last_accepted.get(key).is_none_or(|last| {
            now.checked_sub(*last)
                .is_some_and(|elapsed| elapsed >= window)
        });
        if allowed {
            self.last_accepted.insert(key, now);
        }
        allowed
    }

    fn command_button(
        &mut self,
        params: &Map<String, Value>,
        context: &LegacyContext<'_>,
    ) -> Result<Option<CommandOutput>, MicroError> {
        let key = match params.get("k") {
            Some(Value::Array(_) | Value::Object(_)) => return Err(MicroError::MalformedEvent),
            Some(Value::String(key)) => CommandKey::from_name(key),
            _ => None,
        };
        let Some(key) = key else {
            return Ok(None);
        };
        let Some(action) = params.get("act") else {
            return Ok(None);
        };
        if numeric_equals_u64(action, 0) {
            self.command_down.remove(&key);
        } else if numeric_equals_u64(action, 1)
            && self.command_down.insert(key)
            && self.accept_debounce(key.name(), context.monotonic, COMMAND_DEBOUNCE)
        {
            return Ok(Some(
                context.bindings.decision(key, context.controls_enabled),
            ));
        }
        Ok(None)
    }

    fn radial(
        &mut self,
        params: &Map<String, Value>,
        context: &LegacyContext<'_>,
    ) -> Result<(Option<CommandOutput>, Telemetry), MicroError> {
        let angle = params
            .get("a")
            .and_then(numeric)
            .filter(|value| (0.0..=1.0).contains(value))
            .ok_or(MicroError::InvalidTelemetry)?;
        let distance = params
            .get("d")
            .and_then(numeric)
            .filter(|value| (0.0..=1.0).contains(value))
            .ok_or(MicroError::InvalidTelemetry)?;
        let angle = if angle == 0.0 || angle == 1.0 {
            0.0
        } else {
            angle
        };
        let telemetry = Telemetry {
            angle,
            distance,
            timestamp: context.wall_seconds,
        };
        self.telemetry = Some(telemetry);
        let command = if distance < 0.25 {
            self.stick_armed = true;
            None
        } else if distance >= 0.65 && self.stick_armed {
            if let Some(direction) = self.calibration.nearest(angle) {
                self.stick_armed = false;
                let key = CommandKey::from(direction);
                Some(context.bindings.decision(key, context.controls_enabled))
            } else {
                None
            }
        } else {
            None
        };
        Ok((command, telemetry))
    }

    fn microphone(&mut self, params: &Map<String, Value>, now: Duration) -> bool {
        let index = match params.get("k").and_then(Value::as_str) {
            Some("ACT10") => 0,
            Some("ACT11") => 1,
            _ => return false,
        };
        let Some(action) = params.get("act") else {
            return false;
        };
        if numeric_equals_u64(action, 0) {
            self.mic_down[index] = false;
            return false;
        }
        if !numeric_equals_u64(action, 1) {
            return false;
        }
        let was_down = self.mic_down.iter().any(|down| *down);
        self.mic_down[index] = true;
        let elapsed = self.mic_last.is_none_or(|last| {
            now.checked_sub(last)
                .is_some_and(|age| age >= MICROPHONE_DEBOUNCE)
        });
        if !was_down && elapsed {
            self.mic_last = Some(now);
            true
        } else {
            false
        }
    }

    fn slot(&mut self, params: &Map<String, Value>, context: &LegacyContext<'_>) -> Option<Slot> {
        let key = params.get("k").and_then(Value::as_str)?;
        let action = params.get("act")?;
        if let Some(index) = SLOT_KEYS.iter().position(|name| *name == key) {
            if numeric_equals_u64(action, 0) {
                self.slot_down[index] = false;
            } else if numeric_equals_u64(action, 1) && !self.slot_down[index] {
                self.slot_down[index] = true;
                if self.accept_debounce(SLOT_KEYS[index], context.monotonic, SLOT_DEBOUNCE) {
                    return Some(Slot::ALL[index]);
                }
            }
            return None;
        }
        let (key, forward) = match key {
            "ENC_CW" => ("ENC_CW", true),
            "ENC_CC" => ("ENC_CC", false),
            _ => return None,
        };
        if !numeric_equals_u64(action, 2) {
            return None;
        }
        let active = context.active_slots.unwrap_or(&Slot::ALL);
        if active.is_empty() {
            return None;
        }
        let current = context
            .selected_slot
            .and_then(|selected| active.iter().position(|slot| *slot == selected));
        let index = match (current, forward) {
            (Some(index), true) => (index + 1) % active.len(),
            (Some(0), false) | (None, false) => active.len() - 1,
            (Some(index), false) => index - 1,
            (None, true) => 0,
        };
        self.accept_debounce(key, context.monotonic, SLOT_DEBOUNCE)
            .then_some(active[index])
    }

    /// The caller routes notifications; legacy button/Mic/slot helpers do not
    /// independently check the method name.
    pub fn decode(
        &mut self,
        message: &Value,
        context: &LegacyContext<'_>,
    ) -> Result<LegacyOutput, MicroError> {
        let message = message.as_object().ok_or(MicroError::MalformedEvent)?;
        let empty = Map::new();
        let params = match message.get("p") {
            None => &empty,
            Some(Value::Object(params)) => params,
            Some(_) => return Err(MicroError::MalformedEvent),
        };
        let mut output = LegacyOutput::default();
        if matches!(
            message.get("m").and_then(Value::as_str),
            Some("v.oai.rad" | "kb.radial")
        ) {
            let (command, telemetry) = self.radial(params, context)?;
            output.command = command;
            output.telemetry = Some(telemetry);
        } else {
            output.command = self.command_button(params, context)?;
        }
        output.microphone = self.microphone(params, context.monotonic);
        output.slot = self.slot(params, context);
        Ok(output)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictEventKind {
    Press,
    Release,
    Turn(i8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictEvent<T> {
    pub control: &'static str,
    pub kind: StrictEventKind,
    pub connection_generation: u64,
    pub at: T,
}

/// Adapts only the intentionally narrow strict profile; `at` is passed through.
pub fn adapt_strict<T>(
    message: &Value,
    connection_generation: u64,
    at: T,
) -> Option<StrictEvent<T>> {
    let message = message.as_object()?;
    if message.get("m")?.as_str()? != "v.oai.hid" {
        return None;
    }
    let params = message.get("p")?.as_object()?;
    let key = params.get("k")?.as_str()?;
    let Value::Number(action) = params.get("act")? else {
        return None;
    };
    let raw = action.to_string();
    if raw.contains(['.', 'e', 'E']) || raw.starts_with('-') {
        return None;
    }
    let action = raw.parse::<u64>().ok()?;
    let (control, kind) = match (key, action) {
        ("ENC_CW", 2) => ("encoder_turn", StrictEventKind::Turn(1)),
        ("ENC_CC", 2) => ("encoder_turn", StrictEventKind::Turn(-1)),
        (key, 0 | 1) => {
            let control = SLOT_KEYS
                .iter()
                .copied()
                .find(|name| *name == key)
                .or_else(|| {
                    CommandKey::from_name(key)
                        .filter(|key| !key.is_dpad())
                        .map(CommandKey::name)
                })?;
            (
                control,
                if action == 1 {
                    StrictEventKind::Press
                } else {
                    StrictEventKind::Release
                },
            )
        }
        _ => return None,
    };
    Some(StrictEvent {
        control,
        kind,
        connection_generation,
        at,
    })
}
