//! Deterministic reduction of minimized source events and hooks.

use crate::domain::Status;

use super::{
    event::{Event, EventKind},
    hook::{Hook, HookKind, PendingKind},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reducer {
    status: Status,
    meaningful_event_ms: u64,
    final_candidate: bool,
    last_hook_ms: u64,
    pending_kind: Option<PendingKind>,
    pending_at_ms: Option<u64>,
    run_token: Option<u64>,
}

impl Default for Reducer {
    fn default() -> Self {
        Self {
            status: Status::Idle,
            meaningful_event_ms: 0,
            final_candidate: false,
            last_hook_ms: 0,
            pending_kind: None,
            pending_at_ms: None,
            run_token: None,
        }
    }
}

impl Reducer {
    pub fn new() -> Self {
        Self::default()
    }

    pub const fn status(&self) -> Status {
        self.status
    }

    pub const fn meaningful_event_ms(&self) -> u64 {
        self.meaningful_event_ms
    }

    pub const fn final_candidate(&self) -> bool {
        self.final_candidate
    }

    pub const fn last_hook_ms(&self) -> u64 {
        self.last_hook_ms
    }

    pub const fn pending_kind(&self) -> Option<PendingKind> {
        self.pending_kind
    }

    pub const fn pending_at_ms(&self) -> Option<u64> {
        self.pending_at_ms
    }

    pub const fn run_token(&self) -> Option<u64> {
        self.run_token
    }

    fn transition(
        &mut self,
        status: Status,
        timestamp_ms: u64,
        pending: Option<PendingKind>,
        new_prompt: bool,
    ) {
        if status == Status::Working
            && (new_prompt || !matches!(self.status, Status::Working | Status::Waiting))
        {
            self.run_token = Some(timestamp_ms);
        }
        if status == Status::Waiting {
            self.pending_kind = pending;
            self.pending_at_ms = Some(timestamp_ms);
        } else {
            self.pending_kind = None;
        }
        self.status = status;
        self.meaningful_event_ms = timestamp_ms;
    }

    pub fn apply_event(&mut self, event: Event) {
        if event.timestamp_ms < self.meaningful_event_ms {
            return;
        }
        let timestamp = event.timestamp_ms;
        match event.kind {
            EventKind::UserMessage => {
                self.final_candidate = false;
                self.transition(Status::Working, timestamp, None, true);
            }
            EventKind::AssistantTurnStart => {
                self.final_candidate = false;
                self.transition(Status::Working, timestamp, None, false);
            }
            EventKind::AssistantMessage {
                content_present,
                tool_requests_present,
            } => {
                self.final_candidate = content_present && !tool_requests_present;
                self.meaningful_event_ms = timestamp;
            }
            EventKind::AssistantTurnEnd if self.final_candidate => {
                self.transition(Status::Complete, timestamp, None, false);
            }
            EventKind::TaskComplete => {
                self.transition(Status::Complete, timestamp, None, false);
            }
            EventKind::Abort => {
                self.final_candidate = false;
                self.transition(Status::Idle, timestamp, None, false);
            }
            EventKind::SessionError => {
                self.transition(Status::Error, timestamp, None, false);
            }
            EventKind::ToolExecutionStart { asks_user: true } => {
                self.transition(
                    Status::Waiting,
                    timestamp,
                    Some(PendingKind::ElicitationDialog),
                    false,
                );
            }
            EventKind::ToolExecutionStart { asks_user: false } => {
                self.transition(Status::Working, timestamp, None, false);
            }
            EventKind::ToolExecutionComplete if self.status == Status::Waiting => {
                self.transition(Status::Working, timestamp, None, false);
            }
            EventKind::AssistantTurnEnd | EventKind::ToolExecutionComplete | EventKind::Unknown => {
            }
        }
    }

    pub fn apply_hook(&mut self, hook: Hook) {
        let timestamp = hook.timestamp_ms();
        self.last_hook_ms = self.last_hook_ms.max(timestamp);
        if timestamp < self.meaningful_event_ms {
            return;
        }
        match hook.kind() {
            HookKind::SessionStart => self.transition(Status::Idle, timestamp, None, false),
            HookKind::SessionEnd => self.transition(Status::Off, timestamp, None, false),
            HookKind::UserPromptSubmitted => {
                self.transition(Status::Working, timestamp, None, true);
            }
            HookKind::AgentStop => self.transition(Status::Complete, timestamp, None, false),
            HookKind::Notification => {
                if let Some(kind) = hook.notification() {
                    self.transition(Status::Waiting, timestamp, Some(kind), false);
                }
            }
            HookKind::PostToolUse if self.status != Status::Complete => {
                self.transition(Status::Working, timestamp, None, false);
            }
            HookKind::ErrorOccurred => self.transition(Status::Error, timestamp, None, false),
            HookKind::PostToolUse => {}
        }
    }

    /// Call only after an external coordinator verifies selection and focus.
    pub fn acknowledge_completion(&mut self) {
        if self.status == Status::Complete {
            self.status = Status::Idle;
        }
    }

    pub fn invalidate(&mut self) {
        *self = Self {
            status: Status::Unknown,
            ..Self::default()
        };
    }
}
