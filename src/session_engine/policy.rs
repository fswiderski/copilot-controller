//! Pure structural checks and process-local duplicate suppression.
//!
//! Success is not authority, delivery, authentication, or an input journal.

use std::{collections::BTreeMap, time::Duration};

use crate::domain::{
    Action, ClockSample, Generations, NativeProcessIdentity, RefusalReason, SessionId, Slot, Status,
};

use super::hook::PendingKind;

const VALIDITY: Duration = Duration::from_secs(3);
const RESERVATION_CAPACITY: usize = 6;
const ACCEPT_RESERVATION_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DraftKind {
    Empty,
    Conversation,
    Command,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialogSelection {
    Unknown,
    Ordinary,
    SingleRequest,
    BroadApproval,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiEvidence {
    Unknown,
    Composer {
        draft: DraftKind,
        busy: bool,
        draft_fingerprint: [u8; 32],
        context_fingerprint: [u8; 32],
    },
    Dialog {
        selection: DialogSelection,
        permission: bool,
        busy: bool,
        dialog_fingerprint: [u8; 32],
        dialog_token: [u8; 32],
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Intent {
    pub action: Action,
    pub slot: Slot,
    pub session: SessionId,
    pub process: NativeProcessIdentity,
    pub generations: Generations,
    pub occurred_at: ClockSample,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Evidence {
    pub slot: Slot,
    pub session: SessionId,
    pub process: NativeProcessIdentity,
    pub generations: Generations,
    pub observed_at: ClockSample,
    pub controls_enabled: bool,
    pub stopped: bool,
    pub state_ready: bool,
    pub verified: bool,
    pub selected: bool,
    pub foreground: bool,
    pub presentation_current: bool,
    pub pending_input: bool,
    pub indeterminate_delivery: bool,
    pub status: Status,
    pub pending_kind: Option<PendingKind>,
    pub pending_at_ms: Option<u64>,
    pub run_token: Option<u64>,
    pub ui: UiEvidence,
}

/// Evaluate the action table only, without action or target authority.
pub fn structural_guard(
    action: Action,
    status: Status,
    pending_kind: Option<PendingKind>,
    ui: UiEvidence,
) -> Result<(), RefusalReason> {
    if action == Action::None {
        return Err(RefusalReason::Unsupported);
    }
    if ui == UiEvidence::Unknown {
        return Err(RefusalReason::UnknownUi);
    }
    if matches!(status, Status::Off | Status::Unknown) {
        return Err(RefusalReason::Unavailable);
    }
    if action.is_direction() {
        return Ok(());
    }
    match action {
        Action::Stop => {
            let busy = match ui {
                UiEvidence::Composer { busy, .. } | UiEvidence::Dialog { busy, .. } => busy,
                UiEvidence::Unknown => false,
            };
            if matches!(status, Status::Working | Status::Waiting) && busy
                || status == Status::Waiting && matches!(ui, UiEvidence::Dialog { .. })
            {
                Ok(())
            } else {
                Err(RefusalReason::UnsafeContext)
            }
        }
        Action::Accept => match ui {
            UiEvidence::Dialog {
                selection,
                permission,
                ..
            } => match selection {
                DialogSelection::Unknown => Err(RefusalReason::UnknownUi),
                DialogSelection::BroadApproval => Err(RefusalReason::BroadApproval),
                DialogSelection::Ordinary
                    if permission || pending_kind == Some(PendingKind::PermissionPrompt) =>
                {
                    Err(RefusalReason::BroadApproval)
                }
                DialogSelection::Ordinary | DialogSelection::SingleRequest => Ok(()),
            },
            _ => Err(RefusalReason::UnsafeContext),
        },
        Action::Send | Action::Autopilot | Action::Fork | Action::ModelPicker => {
            if !matches!(status, Status::Idle | Status::Complete) {
                return Err(RefusalReason::Busy);
            }
            match ui {
                UiEvidence::Composer {
                    draft, busy: true, ..
                } => {
                    let _ = draft;
                    Err(RefusalReason::Busy)
                }
                UiEvidence::Composer {
                    draft, busy: false, ..
                } => match (action, draft) {
                    (Action::Send, DraftKind::Conversation) => Ok(()),
                    (Action::Send, DraftKind::Empty) => Err(RefusalReason::EmptyDraft),
                    (_, DraftKind::Command) => Err(RefusalReason::CommandDraft),
                    (_, DraftKind::Empty) => Ok(()),
                    (_, DraftKind::Conversation) => Err(RefusalReason::UnsafeContext),
                },
                _ => Err(RefusalReason::UnsafeContext),
            }
        }
        Action::None | Action::Up | Action::Right | Action::Down | Action::Left => {
            Err(RefusalReason::Unsupported)
        }
    }
}

/// Apply ordered initial validation to supplied evidence at a supplied clock.
pub fn validate_initial(
    intent: &Intent,
    evidence: &Evidence,
    now: ClockSample,
) -> Result<(), RefusalReason> {
    if !evidence.controls_enabled {
        return Err(RefusalReason::Disabled);
    }
    if evidence.indeterminate_delivery {
        return Err(RefusalReason::IndeterminateDelivery);
    }
    if evidence.pending_input {
        return Err(RefusalReason::PendingInput);
    }
    if now.stamp.epoch.is_nil() || intent.occurred_at.stamp.epoch.is_nil() {
        return Err(RefusalReason::ClockMismatch);
    }
    if !now
        .elapsed_within(intent.occurred_at, VALIDITY)
        .map_err(|_| RefusalReason::ClockMismatch)?
    {
        return Err(RefusalReason::ExpiredIntent);
    }
    if evidence.stopped {
        return Err(RefusalReason::StaleState);
    }
    if evidence.observed_at.stamp.epoch.is_nil() {
        return Err(RefusalReason::ClockMismatch);
    }
    if !now
        .elapsed_within(evidence.observed_at, VALIDITY)
        .map_err(|_| RefusalReason::ClockMismatch)?
    {
        return Err(RefusalReason::StaleState);
    }
    if intent.slot != evidence.slot
        || intent.session != evidence.session
        || intent.generations.binding != evidence.generations.binding
    {
        return Err(RefusalReason::ChangedBinding);
    }
    if intent.generations.connection != evidence.generations.connection
        || intent.generations.profile != evidence.generations.profile
        || intent.generations.selection != evidence.generations.selection
        || intent.generations.presentation != evidence.generations.presentation
        || !evidence.presentation_current
    {
        return Err(RefusalReason::ChangedGeneration);
    }
    if intent.process != evidence.process
        || evidence.process.pid == 0
        || evidence.process.creation_time_100ns == 0
        || !evidence.verified
        || !evidence.selected
        || !evidence.foreground
    {
        return Err(RefusalReason::WrongTarget);
    }
    if !intent.action.is_direction() && !evidence.state_ready {
        return Err(RefusalReason::StaleState);
    }
    structural_guard(
        intent.action,
        evidence.status,
        evidence.pending_kind,
        evidence.ui,
    )?;
    if intent.action == Action::Stop && evidence.run_token.is_none() {
        return Err(RefusalReason::ChangedRun);
    }
    Ok(())
}

/// Recheck both snapshots and compare the action-specific structural identity.
pub fn validate_final(
    intent: &Intent,
    original: &Evidence,
    current: &Evidence,
    now: ClockSample,
) -> Result<(), RefusalReason> {
    validate_initial(intent, original, now)?;
    validate_initial(intent, current, now)?;
    current
        .observed_at
        .elapsed_since(original.observed_at)
        .map_err(|_| RefusalReason::ClockMismatch)?;
    match intent.action {
        Action::Stop if original.run_token != current.run_token => Err(RefusalReason::ChangedRun),
        Action::Send => match (original.ui, current.ui) {
            (
                UiEvidence::Composer {
                    draft_fingerprint: a,
                    ..
                },
                UiEvidence::Composer {
                    draft_fingerprint: b,
                    ..
                },
            ) if a == b => Ok(()),
            _ => Err(RefusalReason::ChangedDraft),
        },
        Action::Accept => match (original.ui, current.ui) {
            (
                UiEvidence::Dialog {
                    dialog_fingerprint: fingerprint_a,
                    dialog_token: token_a,
                    ..
                },
                UiEvidence::Dialog {
                    dialog_fingerprint: fingerprint_b,
                    dialog_token: token_b,
                    ..
                },
            ) if fingerprint_a == fingerprint_b
                && token_a == token_b
                && original.pending_at_ms == current.pending_at_ms
                && original.pending_kind == current.pending_kind =>
            {
                Ok(())
            }
            _ => Err(RefusalReason::ChangedDialog),
        },
        Action::Autopilot | Action::Fork | Action::ModelPicker => match (original.ui, current.ui) {
            (
                UiEvidence::Composer {
                    context_fingerprint: a,
                    ..
                },
                UiEvidence::Composer {
                    context_fingerprint: b,
                    ..
                },
            ) if a == b => Ok(()),
            _ => Err(RefusalReason::ChangedDraft),
        },
        _ => Ok(()),
    }
}

#[derive(Clone, Copy, Debug)]
struct StopReservation {
    run_token: u64,
    at: ClockSample,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AcceptRequest {
    pending_at_ms: Option<u64>,
    dialog_token: [u8; 32],
}

#[derive(Debug, Default)]
struct SessionReservations {
    stop: Option<StopReservation>,
    accepts: BTreeMap<AcceptRequest, ClockSample>,
}

/// Retain one table for the coordinator's process lifetime. It holds at most six
/// session identities, with no serialization or session eviction. Expired
/// Accept keys are discarded only after a valid clock comparison. Each session
/// retains at most `ACCEPT_RESERVATION_CAPACITY` live Accept keys.
#[derive(Debug, Default)]
pub struct Reservations {
    sessions: BTreeMap<SessionId, SessionReservations>,
}

impl Reservations {
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Validate first; successful reservation never sends or queues input.
    pub fn validate_and_reserve(
        &mut self,
        intent: &Intent,
        original: &Evidence,
        current: &Evidence,
        now: ClockSample,
    ) -> Result<(), RefusalReason> {
        validate_final(intent, original, current, now)?;
        if !matches!(intent.action, Action::Stop | Action::Accept) {
            return Ok(());
        }
        if !self.sessions.contains_key(&intent.session)
            && self.sessions.len() == RESERVATION_CAPACITY
        {
            return Err(RefusalReason::Unavailable);
        }
        match intent.action {
            Action::Stop => {
                let run_token = current.run_token.ok_or(RefusalReason::ChangedRun)?;
                if let Some(old) = self
                    .sessions
                    .get(&intent.session)
                    .and_then(|entry| entry.stop)
                {
                    let within = strictly_less_than_three_seconds(now, old.at)?;
                    if old.run_token == run_token || within {
                        return Err(RefusalReason::Duplicate);
                    }
                }
                self.sessions.entry(intent.session).or_default().stop =
                    Some(StopReservation { run_token, at: now });
            }
            Action::Accept => {
                let UiEvidence::Dialog { dialog_token, .. } = current.ui else {
                    return Err(RefusalReason::UnsafeContext);
                };
                let request = AcceptRequest {
                    pending_at_ms: current.pending_at_ms,
                    dialog_token,
                };
                let mut expired = Vec::new();
                if let Some(previous) = self.sessions.get(&intent.session) {
                    for (&old_request, &at) in &previous.accepts {
                        let within = strictly_less_than_three_seconds(now, at)?;
                        if old_request == request && within {
                            return Err(RefusalReason::Duplicate);
                        }
                        if !within {
                            expired.push(old_request);
                        }
                    }
                }
                let entry = self.sessions.entry(intent.session).or_default();
                for old_request in expired {
                    entry.accepts.remove(&old_request);
                }
                if entry.accepts.len() >= ACCEPT_RESERVATION_CAPACITY {
                    return Err(RefusalReason::Unavailable);
                }
                entry.accepts.insert(request, now);
            }
            _ => unreachable!("only stop and accept use reservations"),
        }
        Ok(())
    }
}

fn strictly_less_than_three_seconds(
    later: ClockSample,
    earlier: ClockSample,
) -> Result<bool, RefusalReason> {
    later
        .elapsed_since(earlier)
        .map_err(|_| RefusalReason::ClockMismatch)?;
    let delta = later.stamp.ticks - earlier.stamp.ticks;
    Ok(u128::from(delta) < u128::from(later.stamp.frequency) * 3)
}
