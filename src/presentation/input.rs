//! Logical-role validation and clean, fully evidenced discrete gestures.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::domain::{Action, ClockSample, RefusalReason, RuntimeEpoch, Slot};

use super::{
    evidence::{DeviceKind, EvidenceError, EvidenceScope, EvidenceUse},
    profiles::{
        AKP03_ORIGINAL_PROFILE, AKP03_REV2_PROFILE, CT_BUTTON_NAMES, ControlKind, DeviceProfile,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    SelectSlot(Slot),
    MoveCursor,
    CommitCursor,
    Command(Action),
    ShowHealth,
    PageNext,
    PagePrevious,
}

impl Intent {
    pub const fn priority(self) -> u8 {
        match self {
            Self::SelectSlot(_) => 0,
            Self::MoveCursor | Self::CommitCursor => 1,
            Self::Command(Action::Stop) => 2,
            Self::Command(Action::Accept | Action::Send) => 3,
            Self::Command(Action::Up | Action::Right | Action::Down | Action::Left) => 4,
            _ => 5,
        }
    }

    fn sensitive(self) -> bool {
        matches!(
            self,
            Self::Command(Action::Stop | Action::Accept | Action::Send | Action::Fork)
        )
    }

    fn is_command(self) -> bool {
        matches!(self, Self::Command(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalBinding {
    pub control: String,
    pub intent: Intent,
    pub adjacency_confirmed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoleError {
    TooManyBindings,
    DuplicateControl(String),
    UnknownControl(String),
    UnsupportedAction,
    WrongControlKind(String),
    StopNotIsolated(String),
    StopAdjacentToAcceptOrSend(String),
    AdjacencyConfirmationRequired(String),
    ProfileScopeMismatch,
    Scope(EvidenceError),
    InputEvidence {
        control: String,
        reason: EvidenceError,
    },
}

impl std::fmt::Display for RoleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyBindings => f.write_str("at most 64 logical bindings are allowed"),
            Self::DuplicateControl(id) => write!(f, "duplicate control assignment: {id}"),
            Self::UnknownControl(id) => write!(f, "unknown control: {id}"),
            Self::UnsupportedAction => f.write_str("the none action is unsupported"),
            Self::WrongControlKind(id) => write!(f, "control kind cannot perform this role: {id}"),
            Self::StopNotIsolated(id) => write!(f, "Stop is not physically isolated: {id}"),
            Self::StopAdjacentToAcceptOrSend(id) => {
                write!(f, "Stop is adjacent to Accept or Send: {id}")
            }
            Self::AdjacencyConfirmationRequired(id) => {
                write!(f, "sensitive control requires adjacency confirmation: {id}")
            }
            Self::ProfileScopeMismatch => f.write_str("scope does not match the device profile"),
            Self::Scope(error) => write!(f, "input scope: {error}"),
            Self::InputEvidence { control, reason } => {
                write!(f, "input evidence for {control}: {reason}")
            }
        }
    }
}

impl std::error::Error for RoleError {}

/// Shape validation is deliberately separate from input admission.
pub fn validate_binding_shapes(
    profile: &DeviceProfile,
    bindings: &[LogicalBinding],
) -> Result<(), RoleError> {
    if bindings.len() > 64 {
        return Err(RoleError::TooManyBindings);
    }
    let mut seen = BTreeSet::new();
    for binding in bindings {
        if !seen.insert(binding.control.as_str()) {
            return Err(RoleError::DuplicateControl(binding.control.clone()));
        }
        let control = profile
            .control(&binding.control)
            .ok_or_else(|| RoleError::UnknownControl(binding.control.clone()))?;
        if binding.intent == Intent::Command(Action::None) {
            return Err(RoleError::UnsupportedAction);
        }
        let kind_ok = match binding.intent {
            Intent::MoveCursor => matches!(control.kind, ControlKind::Turn | ControlKind::Wheel),
            Intent::SelectSlot(_) | Intent::CommitCursor => control.kind == ControlKind::Press,
            intent if intent.sensitive() => control.kind == ControlKind::Press,
            _ => true,
        };
        if !kind_ok {
            return Err(RoleError::WrongControlKind(binding.control.clone()));
        }
        if binding.intent == Intent::Command(Action::Stop) && !control.isolated_stop {
            return Err(RoleError::StopNotIsolated(binding.control.clone()));
        }
        if binding.intent.sensitive() && control.near_selection && !binding.adjacency_confirmed {
            return Err(RoleError::AdjacencyConfirmationRequired(
                binding.control.clone(),
            ));
        }
    }
    for stop in bindings
        .iter()
        .filter(|binding| binding.intent == Intent::Command(Action::Stop))
    {
        let stop_control = profile
            .control(&stop.control)
            .expect("all control IDs were checked above");
        for other in bindings.iter().filter(|binding| {
            matches!(
                binding.intent,
                Intent::Command(Action::Accept | Action::Send)
            )
        }) {
            let other_control = profile
                .control(&other.control)
                .expect("all control IDs were checked above");
            if stop_control.adjacent.contains(&other.control)
                || other_control.adjacent.contains(&stop.control)
            {
                return Err(RoleError::StopAdjacentToAcceptOrSend(stop.control.clone()));
            }
        }
    }
    Ok(())
}

/// This checks supplied evidence but never activates or dispatches a binding.
pub fn validate_live_bindings(
    profile: &DeviceProfile,
    scope: &EvidenceScope,
    bindings: &[LogicalBinding],
) -> Result<(), RoleError> {
    validate_binding_shapes(profile, bindings)?;
    if !profile.matches_scope(scope) {
        return Err(RoleError::ProfileScopeMismatch);
    }
    scope.validate(true).map_err(RoleError::Scope)?;
    for binding in bindings {
        let control = profile
            .control(&binding.control)
            .expect("all control IDs were checked above");
        control
            .input_evidence
            .check(scope, EvidenceUse::InputAdmission)
            .map_err(|reason| RoleError::InputEvidence {
                control: binding.control.clone(),
                reason,
            })?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhysicalEventKind {
    Press,
    Release,
    Turn(i8),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalEvent {
    pub control: String,
    pub kind: PhysicalEventKind,
    pub connection_generation: u64,
    pub at: ClockSample,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LogicalRequest {
    pub control: String,
    pub intent: Intent,
    pub delta: i8,
}

#[derive(Clone, Debug)]
struct GestureContext {
    profile: DeviceProfile,
    scope: EvidenceScope,
    bindings: Vec<LogicalBinding>,
}

#[derive(Clone, Debug)]
struct CandidatePress {
    control: String,
    started: ClockSample,
}

/// Tracks one connection; a lifecycle/clock cut must explicitly reset it.
#[derive(Clone, Debug)]
pub struct GestureTracker {
    connection_generation: u64,
    epoch: RuntimeEpoch,
    held: BTreeSet<String>,
    context: Option<GestureContext>,
    candidate: Option<CandidatePress>,
    tainted: bool,
    overflowed: bool,
    last_sample: Option<ClockSample>,
    last_accepted: BTreeMap<String, ClockSample>,
}

impl GestureTracker {
    pub fn new(connection_generation: u64, epoch: RuntimeEpoch) -> Result<Self, RefusalReason> {
        if epoch.is_nil() {
            return Err(RefusalReason::ClockMismatch);
        }
        Ok(Self {
            connection_generation,
            epoch,
            held: BTreeSet::new(),
            context: None,
            candidate: None,
            tainted: false,
            overflowed: false,
            last_sample: None,
            last_accepted: BTreeMap::new(),
        })
    }

    pub fn reset(
        &mut self,
        connection_generation: u64,
        epoch: RuntimeEpoch,
    ) -> Result<(), RefusalReason> {
        *self = Self::new(connection_generation, epoch)?;
        Ok(())
    }

    fn clear_gesture_if_released(&mut self) {
        if self.held.is_empty() {
            self.context = None;
            self.candidate = None;
            self.tainted = self.overflowed;
        }
    }

    fn record_tainted_event(&mut self, event: &PhysicalEvent) {
        self.tainted = true;
        self.candidate = None;
        match event.kind {
            PhysicalEventKind::Press if !self.held.contains(&event.control) => {
                if self.held.len() < 64 {
                    self.held.insert(event.control.clone());
                } else {
                    self.overflowed = true;
                }
            }
            PhysicalEventKind::Release => {
                self.held.remove(&event.control);
                self.clear_gesture_if_released();
            }
            _ => {}
        }
    }

    fn context_change(
        &self,
        profile: &DeviceProfile,
        scope: &EvidenceScope,
        bindings: &[LogicalBinding],
    ) -> Option<RefusalReason> {
        let context = self.context.as_ref()?;
        if context.scope != *scope || !profile.matches_scope(scope) {
            Some(RefusalReason::WrongTarget)
        } else if context.bindings != bindings || context.profile != *profile {
            Some(RefusalReason::ChangedBinding)
        } else {
            None
        }
    }

    pub fn observe(
        &mut self,
        profile: &DeviceProfile,
        scope: &EvidenceScope,
        bindings: &[LogicalBinding],
        event: &PhysicalEvent,
    ) -> Result<Option<LogicalRequest>, RefusalReason> {
        if event.connection_generation != self.connection_generation {
            // Keep current-generation holds tainted until release or explicit reset.
            if !self.held.is_empty() {
                self.tainted = true;
                self.candidate = None;
            }
            return Err(RefusalReason::ChangedGeneration);
        }
        if event.at.stamp.epoch != self.epoch
            || event.at.stamp.frequency == 0
            || self
                .last_sample
                .is_some_and(|earlier| event.at.elapsed_since(earlier).is_err())
        {
            if event.kind == PhysicalEventKind::Release {
                self.tainted = true;
                self.candidate = None;
            } else {
                self.record_tainted_event(event);
            }
            return Err(RefusalReason::ClockMismatch);
        }
        self.last_sample = Some(event.at);
        if self.overflowed {
            self.record_tainted_event(event);
            return Err(RefusalReason::Busy);
        }
        if let Some(reason) = self.context_change(profile, scope, bindings) {
            self.record_tainted_event(event);
            return Err(reason);
        }
        if bindings.len() > 64 {
            self.record_tainted_event(event);
            return Err(RefusalReason::Unsupported);
        }
        if profile.control(&event.control).is_none() {
            self.record_tainted_event(event);
            return Err(if self.overflowed {
                RefusalReason::Busy
            } else {
                RefusalReason::Unsupported
            });
        }
        match event.kind {
            PhysicalEventKind::Press => {
                if self.held.contains(&event.control) {
                    return Ok(None);
                }
                if self.held.len() >= 64 {
                    self.tainted = true;
                    self.overflowed = true;
                    self.candidate = None;
                    return Err(RefusalReason::Busy);
                }
                if self.held.is_empty() {
                    self.context = Some(GestureContext {
                        profile: profile.clone(),
                        scope: scope.clone(),
                        bindings: bindings.to_vec(),
                    });
                    self.candidate = Some(CandidatePress {
                        control: event.control.clone(),
                        started: event.at,
                    });
                } else {
                    self.tainted = true;
                    self.candidate = None;
                }
                self.held.insert(event.control.clone());
                Ok(None)
            }
            PhysicalEventKind::Release => {
                if !self.held.remove(&event.control) {
                    return Ok(None);
                }
                let candidate = self.candidate.take();
                let clean = !self.tainted && self.held.is_empty();
                self.clear_gesture_if_released();
                let Some(candidate) =
                    candidate.filter(|candidate| clean && candidate.control == event.control)
                else {
                    return Ok(None);
                };
                match event
                    .at
                    .elapsed_within(candidate.started, std::time::Duration::from_secs(3))
                {
                    Ok(true) => {}
                    Ok(false) => return Err(RefusalReason::ExpiredIntent),
                    Err(_) => return Err(RefusalReason::ClockMismatch),
                }
                self.admit(profile, scope, bindings, event, 0)
            }
            PhysicalEventKind::Turn(delta) => {
                if !self.held.is_empty() {
                    self.tainted = true;
                    self.candidate = None;
                    return Ok(None);
                }
                if self.tainted {
                    return Ok(None);
                }
                if !matches!(delta, -1 | 1) {
                    return Ok(None);
                }
                self.admit(profile, scope, bindings, event, delta)
            }
        }
    }

    fn admit(
        &mut self,
        profile: &DeviceProfile,
        scope: &EvidenceScope,
        bindings: &[LogicalBinding],
        event: &PhysicalEvent,
        delta: i8,
    ) -> Result<Option<LogicalRequest>, RefusalReason> {
        validate_live_bindings(profile, scope, bindings).map_err(|error| match error {
            RoleError::ProfileScopeMismatch => RefusalReason::WrongTarget,
            RoleError::StopNotIsolated(_)
            | RoleError::StopAdjacentToAcceptOrSend(_)
            | RoleError::AdjacencyConfirmationRequired(_) => RefusalReason::UnsafeContext,
            RoleError::Scope(_) | RoleError::InputEvidence { .. } => RefusalReason::Unavailable,
            _ => RefusalReason::Unsupported,
        })?;
        let control = profile
            .control(&event.control)
            .ok_or(RefusalReason::Unsupported)?;
        let binding = bindings
            .iter()
            .find(|binding| binding.control == event.control)
            .ok_or(RefusalReason::Unsupported)?;
        let discrete_kind_matches = if delta == 0 {
            control.kind == ControlKind::Press
        } else {
            matches!(control.kind, ControlKind::Turn | ControlKind::Wheel)
        };
        if !discrete_kind_matches {
            return Err(RefusalReason::Unsupported);
        }
        let minimum_ms = if binding.intent.is_command() {
            500
        } else {
            300
        };
        if let Some(previous) = self.last_accepted.get(&event.control) {
            event
                .at
                .elapsed_since(*previous)
                .map_err(|_| RefusalReason::ClockMismatch)?;
            let ticks = event.at.stamp.ticks - previous.stamp.ticks;
            if u128::from(ticks) * 1000
                < u128::from(event.at.stamp.frequency) * u128::from(minimum_ms as u32)
            {
                return Err(RefusalReason::Duplicate);
            }
        }
        self.last_accepted.insert(event.control.clone(), event.at);
        if binding.intent.is_command() {
            return Err(RefusalReason::Disabled);
        }
        Ok(Some(LogicalRequest {
            control: event.control.clone(),
            intent: binding.intent,
            delta,
        }))
    }
}

/// Separate legacy wide-Mic coalescer; it never accesses a microphone.
///
/// The strict semantic adapter below intentionally does not emit these keys.
#[derive(Clone, Debug)]
pub struct MicroMicCoalescer {
    connection_generation: u64,
    epoch: RuntimeEpoch,
    down: [bool; 2],
    last_sample: Option<ClockSample>,
    last_accepted: Option<ClockSample>,
}

impl MicroMicCoalescer {
    pub fn new(connection_generation: u64, epoch: RuntimeEpoch) -> Result<Self, RefusalReason> {
        if epoch.is_nil() {
            return Err(RefusalReason::ClockMismatch);
        }
        Ok(Self {
            connection_generation,
            epoch,
            down: [false; 2],
            last_sample: None,
            last_accepted: None,
        })
    }

    pub fn reset(
        &mut self,
        connection_generation: u64,
        epoch: RuntimeEpoch,
    ) -> Result<(), RefusalReason> {
        *self = Self::new(connection_generation, epoch)?;
        Ok(())
    }

    /// Returns only a logical Mic activation, never voice access or dispatch.
    pub fn observe(&mut self, event: &PhysicalEvent) -> Result<bool, RefusalReason> {
        let index = match event.control.as_str() {
            "ACT10" => 0,
            "ACT11" => 1,
            _ => return Ok(false),
        };
        let pressed = match event.kind {
            PhysicalEventKind::Press => true,
            PhysicalEventKind::Release => false,
            PhysicalEventKind::Turn(_) => return Ok(false),
        };
        if event.connection_generation != self.connection_generation {
            return Err(RefusalReason::ChangedGeneration);
        }
        if event.at.stamp.epoch != self.epoch
            || event.at.stamp.frequency == 0
            || self
                .last_sample
                .is_some_and(|earlier| event.at.elapsed_since(earlier).is_err())
        {
            self.down[index] = pressed;
            return Err(RefusalReason::ClockMismatch);
        }
        self.last_sample = Some(event.at);
        let was_held = self.down.iter().any(|down| *down);
        self.down[index] = pressed;
        if !pressed || was_held {
            return Ok(false);
        }
        if let Some(previous) = self.last_accepted {
            event
                .at
                .elapsed_since(previous)
                .map_err(|_| RefusalReason::ClockMismatch)?;
            let ticks = event.at.stamp.ticks - previous.stamp.ticks;
            if u128::from(ticks) * 1000 < u128::from(event.at.stamp.frequency) * 400 {
                return Ok(false);
            }
        }
        self.last_accepted = Some(event.at);
        Ok(true)
    }
}

fn physical(
    control: impl Into<String>,
    kind: PhysicalEventKind,
    connection_generation: u64,
    at: ClockSample,
) -> PhysicalEvent {
    PhysicalEvent {
        control: control.into(),
        kind,
        connection_generation,
        at,
    }
}

/// Strict Micro adapter: no coercions, encoder press, Mic, or D-pad events.
pub fn normalize_micro(
    value: &serde_json::Value,
    generation: u64,
    at: ClockSample,
) -> Option<PhysicalEvent> {
    let object = value.as_object()?;
    if object.get("m")?.as_str()? != "v.oai.hid" {
        return None;
    }
    let payload = object.get("p")?.as_object()?;
    let key = payload.get("k")?.as_str()?;
    let action = payload.get("act")?.as_u64()?;
    match (key, action) {
        ("ENC_CW", 2) => Some(physical(
            "encoder_turn",
            PhysicalEventKind::Turn(1),
            generation,
            at,
        )),
        ("ENC_CC", 2) => Some(physical(
            "encoder_turn",
            PhysicalEventKind::Turn(-1),
            generation,
            at,
        )),
        (
            "AG00" | "AG01" | "AG02" | "AG03" | "AG04" | "AG05" | "ACT06" | "ACT07" | "ACT08"
            | "ACT09" | "ACT12",
            0 | 1,
        ) => Some(physical(
            key,
            if action == 1 {
                PhysicalEventKind::Press
            } else {
                PhysicalEventKind::Release
            },
            generation,
            at,
        )),
        _ => None,
    }
}

/// The codec supplies only its decoded facts, never inferred releases.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AjazzDecodedInput {
    Acknowledgment,
    NoControl,
    ButtonPressObserved(u8),
    EncoderPressObserved(u8),
    ButtonState { index: u8, pressed: bool },
    EncoderState { index: u8, pressed: bool },
    EncoderTurn { index: u8, delta: i8 },
}

pub fn normalize_ajazz(
    profile: &DeviceProfile,
    input: AjazzDecodedInput,
    generation: u64,
    at: ClockSample,
) -> Option<PhysicalEvent> {
    if profile.device != DeviceKind::Ajazz
        || !matches!(
            profile.id.as_str(),
            AKP03_ORIGINAL_PROFILE | AKP03_REV2_PROFILE
        )
    {
        return None;
    }
    let kind = |pressed| {
        if pressed {
            PhysicalEventKind::Press
        } else {
            PhysicalEventKind::Release
        }
    };
    match input {
        AjazzDecodedInput::ButtonState { index, pressed }
            if profile.id == AKP03_REV2_PROFILE && index < 9 =>
        {
            Some(physical(
                format!("button_{index}"),
                kind(pressed),
                generation,
                at,
            ))
        }
        AjazzDecodedInput::EncoderState { index, pressed }
            if profile.id == AKP03_REV2_PROFILE && index < 3 =>
        {
            Some(physical(
                format!("encoder_{index}_press"),
                kind(pressed),
                generation,
                at,
            ))
        }
        AjazzDecodedInput::EncoderTurn { index, delta } if index < 3 && matches!(delta, -1 | 1) => {
            Some(physical(
                format!("encoder_{index}_turn"),
                PhysicalEventKind::Turn(delta),
                generation,
                at,
            ))
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CtDecodedInput {
    Button { id: u8, pressed: bool },
    Rotation { id: u8, delta: i8 },
    Touch,
    Tick,
}

/// All CT touch contacts are dropped; no tap or slide is inferred.
pub fn normalize_ct(
    input: CtDecodedInput,
    generation: u64,
    at: ClockSample,
) -> Option<PhysicalEvent> {
    match input {
        CtDecodedInput::Button { id, pressed } => {
            CT_BUTTON_NAMES.get(usize::from(id)).map(|name| {
                physical(
                    *name,
                    if pressed {
                        PhysicalEventKind::Press
                    } else {
                        PhysicalEventKind::Release
                    },
                    generation,
                    at,
                )
            })
        }
        CtDecodedInput::Rotation { id, delta } if id < 7 && matches!(delta, -1 | 1) => {
            Some(physical(
                format!("{}_turn", CT_BUTTON_NAMES[usize::from(id)]),
                PhysicalEventKind::Turn(delta),
                generation,
                at,
            ))
        }
        _ => None,
    }
}
