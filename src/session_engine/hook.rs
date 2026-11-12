//! Strict, minimized hooks and their bounded, session-scoped inbox.

use std::{fmt, fmt::Formatter};

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, SeqAccess, Visitor},
};

use crate::domain::SessionId;

use super::{
    event::{MAX_EVENT_BYTES, StringSelection, TimestampMs, select_string},
    reducer::Reducer,
    strict::{self, Object},
};

pub const HOOK_CAPACITY: usize = 256;
pub const HEARTBEAT_MAX_AGE_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingKind {
    PermissionPrompt,
    ElicitationDialog,
}

impl<'de> Deserialize<'de> for PendingKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PendingVisitor;
        impl Visitor<'_> for PendingVisitor {
            type Value = PendingKind;

            fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str("a pending kind")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                match value {
                    "permission_prompt" => Ok(PendingKind::PermissionPrompt),
                    "elicitation_dialog" => Ok(PendingKind::ElicitationDialog),
                    _ => Err(E::custom("invalid pending kind")),
                }
            }
        }
        deserializer
            .deserialize_str(PendingVisitor)
            .map_err(|_| de::Error::custom("invalid pending kind"))
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HookKind {
    SessionStart,
    SessionEnd,
    UserPromptSubmitted,
    AgentStop,
    Notification,
    PostToolUse,
    ErrorOccurred,
}

impl<'de> Deserialize<'de> for HookKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KindVisitor;
        impl Visitor<'_> for KindVisitor {
            type Value = HookKind;

            fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str("a hook kind")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                HookKind::from_name(value).map_err(|_| E::custom("invalid hook kind"))
            }
        }
        deserializer
            .deserialize_str(KindVisitor)
            .map_err(|_| de::Error::custom("invalid hook kind"))
    }
}

impl HookKind {
    pub fn from_name(name: &str) -> Result<Self, HookParseError> {
        match name {
            "sessionStart" => Ok(Self::SessionStart),
            "sessionEnd" => Ok(Self::SessionEnd),
            "userPromptSubmitted" => Ok(Self::UserPromptSubmitted),
            "agentStop" => Ok(Self::AgentStop),
            "notification" => Ok(Self::Notification),
            "postToolUse" => Ok(Self::PostToolUse),
            "errorOccurred" => Ok(Self::ErrorOccurred),
            _ => Err(HookParseError::UnknownKind),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct Hook {
    session_id: SessionId,
    timestamp_ms: u64,
    kind: HookKind,
    notification: Option<PendingKind>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHook {
    session_id: SessionId,
    timestamp_ms: u64,
    kind: HookKind,
    #[serde(deserialize_with = "required_nullable")]
    notification: Option<PendingKind>,
}

fn required_nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(deserializer)
}

impl<'de> Deserialize<'de> for Hook {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw: RawHook =
            strict::object(deserializer).map_err(|_| de::Error::custom("invalid hook record"))?;
        Self::new(raw.session_id, raw.timestamp_ms, raw.kind, raw.notification)
            .map_err(de::Error::custom)
    }
}

impl Hook {
    pub fn new(
        session_id: SessionId,
        timestamp_ms: u64,
        kind: HookKind,
        notification: Option<PendingKind>,
    ) -> Result<Self, HookParseError> {
        if kind != HookKind::Notification && notification.is_some() {
            return Err(HookParseError::InvalidRecord);
        }
        Ok(Self {
            session_id,
            timestamp_ms,
            kind,
            notification,
        })
    }

    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub const fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }

    pub const fn kind(&self) -> HookKind {
        self.kind
    }

    pub const fn notification(&self) -> Option<PendingKind> {
        self.notification
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookParseError {
    TooLarge,
    UnknownKind,
    InvalidRecord,
}

impl fmt::Display for HookParseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLarge => "hook input exceeds its byte limit",
            Self::UnknownKind => "unsupported hook kind",
            Self::InvalidRecord => "invalid hook record",
        })
    }
}

impl std::error::Error for HookParseError {}

struct NotificationSelection;
impl StringSelection for NotificationSelection {
    type Output = Option<PendingKind>;
    fn select(value: &str) -> Self::Output {
        match value {
            "permission_prompt" => Some(PendingKind::PermissionPrompt),
            "elicitation_dialog" => Some(PendingKind::ElicitationDialog),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct HookPayload {
    #[serde(rename = "sessionId")]
    session_id: SessionId,
    timestamp: TimestampMs,
    #[serde(
        default,
        deserialize_with = "select_string::<_, NotificationSelection>"
    )]
    notification_type: Option<PendingKind>,
}

/// Unknown payload fields and unrecognized notifications are discarded.
pub fn parse_hook(name: &str, bytes: &[u8]) -> Result<Hook, HookParseError> {
    let kind = HookKind::from_name(name)?;
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(HookParseError::TooLarge);
    }
    std::str::from_utf8(bytes).map_err(|_| HookParseError::InvalidRecord)?;
    let payload: Object<HookPayload> =
        serde_json::from_slice(bytes).map_err(|_| HookParseError::InvalidRecord)?;
    let payload = payload.0;
    let notification = (kind == HookKind::Notification)
        .then_some(payload.notification_type)
        .flatten();
    Hook::new(payload.session_id, payload.timestamp.0, kind, notification)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionContext {
    pub bound_session: Option<SessionId>,
    pub stopped: bool,
    pub heartbeat_ms: Option<u64>,
    pub now_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Admission {
    Added,
    Duplicate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxError {
    Unbound,
    Stopped,
    MissingHeartbeat,
    FutureHeartbeat,
    StaleHeartbeat,
    Full,
    InvalidRecord,
    TooLarge,
}

impl fmt::Display for InboxError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unbound => "hook session is not bound",
            Self::Stopped => "coordinator is stopped",
            Self::MissingHeartbeat => "coordinator heartbeat is missing",
            Self::FutureHeartbeat => "coordinator heartbeat is in the future",
            Self::StaleHeartbeat => "coordinator heartbeat is stale",
            Self::Full => "hook inbox is full",
            Self::InvalidRecord => "invalid hook inbox",
            Self::TooLarge => "hook inbox exceeds its byte limit",
        })
    }
}

impl std::error::Error for InboxError {}

/// Serializable minimized records only. The caller supplies transactional
/// persistence; admission is not authentication.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HookInbox {
    hooks: Vec<Hook>,
}

impl Serialize for HookInbox {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.hooks.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for HookInbox {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct InboxVisitor;
        impl<'de> Visitor<'de> for InboxVisitor {
            type Value = HookInbox;

            fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str("at most 256 distinct minimized hooks")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut hooks = Vec::new();
                while let Some(hook) = sequence.next_element::<Hook>()? {
                    if hooks.len() == HOOK_CAPACITY || hooks.contains(&hook) {
                        return Err(de::Error::custom("invalid hook inbox"));
                    }
                    hooks.push(hook);
                }
                Ok(HookInbox { hooks })
            }
        }
        deserializer
            .deserialize_seq(InboxVisitor)
            .map_err(|_| de::Error::custom("invalid hook inbox"))
    }
}

impl HookInbox {
    pub fn from_json(bytes: &[u8]) -> Result<Self, InboxError> {
        if bytes.len() > MAX_EVENT_BYTES {
            return Err(InboxError::TooLarge);
        }
        serde_json::from_slice(bytes).map_err(|_| InboxError::InvalidRecord)
    }

    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Hook> {
        self.hooks.iter()
    }

    pub fn admit(
        &mut self,
        hook: Hook,
        context: AdmissionContext,
    ) -> Result<Admission, InboxError> {
        if context.bound_session != Some(hook.session_id) {
            return Err(InboxError::Unbound);
        }
        if context.stopped {
            return Err(InboxError::Stopped);
        }
        let heartbeat = context.heartbeat_ms.ok_or(InboxError::MissingHeartbeat)?;
        let age = context
            .now_ms
            .checked_sub(heartbeat)
            .ok_or(InboxError::FutureHeartbeat)?;
        if age > HEARTBEAT_MAX_AGE_MS {
            return Err(InboxError::StaleHeartbeat);
        }
        if self.hooks.contains(&hook) {
            return Ok(Admission::Duplicate);
        }
        if self.hooks.len() == HOOK_CAPACITY {
            return Err(InboxError::Full);
        }
        self.hooks.push(hook);
        Ok(Admission::Added)
    }

    /// Apply only this session's hooks in stable timestamp/arrival order.
    pub fn drain_session(&mut self, session: SessionId, reducer: &mut Reducer) -> usize {
        let (mut matching, remaining): (Vec<_>, Vec<_>) = std::mem::take(&mut self.hooks)
            .into_iter()
            .partition(|hook| hook.session_id == session);
        self.hooks = remaining;
        matching.sort_by_key(|hook| hook.timestamp_ms);
        let count = matching.len();
        for hook in matching {
            reducer.apply_hook(hook);
        }
        count
    }
}
