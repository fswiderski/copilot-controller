//! Fork observations and conservative, revision-checked follow decisions.
//!
//! No fork is initiated here. A retained terminal anchor remains only stored
//! metadata; rebinding it does not verify its current owner.

use std::{collections::HashSet, fmt, fmt::Formatter};

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, SeqAccess, Visitor},
};

use crate::domain::{NativeProcessIdentity, SessionId, Slot, SlotMap};

use super::{event::MAX_EVENT_BYTES, strict};

pub const FORK_CAPACITY: usize = 6;
pub const MAX_SEEN_SESSIONS: usize = 256;
pub const UNKNOWN_CHILD_RETENTION_MS: u64 = 30_000;
pub const KNOWN_CHILD_RETENTION_MS: u64 = 86_400_000;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ForkId(SessionId);

impl ForkId {
    pub fn new(id: SessionId) -> Result<Self, ForkError> {
        if id.is_nil() {
            Err(ForkError::InvalidRecord)
        } else {
            Ok(Self(id))
        }
    }

    pub const fn as_session_id(self) -> SessionId {
        self.0
    }
}

impl<'de> Deserialize<'de> for ForkId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(SessionId::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForkObservation {
    pub child: SessionId,
    pub timestamp_ms: u64,
    pub next_offset: u64,
}

#[derive(Clone, Debug)]
pub struct ForkBegin {
    pub id: ForkId,
    pub slot: Slot,
    pub source: SessionId,
    pub binding_revision: u64,
    pub process: NativeProcessIdentity,
    pub issued_ms: u64,
    pub event_offset: u64,
    pub seen: Vec<SessionId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PendingFork {
    id: ForkId,
    slot: Slot,
    source: SessionId,
    binding_revision: u64,
    process: NativeProcessIdentity,
    issued_ms: u64,
    event_offset: u64,
    seen: SeenSessions,
    child: Option<SessionId>,
    conflicted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
struct SeenSessions(Vec<SessionId>);

impl SeenSessions {
    fn new(seen: Vec<SessionId>) -> Result<Self, ForkError> {
        if seen.len() > MAX_SEEN_SESSIONS {
            return Err(ForkError::InvalidRecord);
        }
        let unique: HashSet<_> = seen.iter().copied().collect();
        if unique.len() != seen.len() {
            Err(ForkError::InvalidRecord)
        } else {
            Ok(Self(seen))
        }
    }
}

impl<'de> Deserialize<'de> for SeenSessions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SeenVisitor;
        impl<'de> Visitor<'de> for SeenVisitor {
            type Value = SeenSessions;

            fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str("at most 256 distinct session UUIDs")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut ids = Vec::new();
                while let Some(id) = sequence.next_element()? {
                    if ids.len() == MAX_SEEN_SESSIONS || ids.contains(&id) {
                        return Err(de::Error::custom("invalid seen sessions"));
                    }
                    ids.push(id);
                }
                Ok(SeenSessions(ids))
            }
        }
        deserializer.deserialize_seq(SeenVisitor)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPendingFork {
    id: ForkId,
    slot: Slot,
    source: SessionId,
    binding_revision: u64,
    process: NativeProcessIdentity,
    issued_ms: u64,
    event_offset: u64,
    seen: SeenSessions,
    #[serde(deserialize_with = "required_nullable")]
    child: Option<SessionId>,
    conflicted: bool,
}

fn required_nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(deserializer)
}

impl<'de> Deserialize<'de> for PendingFork {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw: RawPendingFork =
            strict::object(deserializer).map_err(|_| de::Error::custom("invalid pending fork"))?;
        let pending = Self {
            id: raw.id,
            slot: raw.slot,
            source: raw.source,
            binding_revision: raw.binding_revision,
            process: raw.process,
            issued_ms: raw.issued_ms,
            event_offset: raw.event_offset,
            seen: raw.seen,
            child: raw.child,
            conflicted: raw.conflicted,
        };
        pending.validate().map_err(de::Error::custom)?;
        Ok(pending)
    }
}

impl PendingFork {
    pub fn new(begin: ForkBegin) -> Result<Self, ForkError> {
        let pending = Self {
            id: begin.id,
            slot: begin.slot,
            source: begin.source,
            binding_revision: begin.binding_revision,
            process: begin.process,
            issued_ms: begin.issued_ms,
            event_offset: begin.event_offset,
            seen: SeenSessions::new(begin.seen)?,
            child: None,
            conflicted: false,
        };
        pending.validate()?;
        Ok(pending)
    }

    fn validate(&self) -> Result<(), ForkError> {
        if self.source.is_nil()
            || self.process.pid == 0
            || self.process.creation_time_100ns == 0
            || self.child.is_some_and(|child| {
                child.is_nil() || child == self.source || self.seen.0.contains(&child)
            })
        {
            return Err(ForkError::InvalidRecord);
        }
        Ok(())
    }

    pub const fn id(&self) -> ForkId {
        self.id
    }

    pub const fn slot(&self) -> Slot {
        self.slot
    }

    pub const fn source(&self) -> SessionId {
        self.source
    }

    pub const fn binding_revision(&self) -> u64 {
        self.binding_revision
    }

    pub const fn process(&self) -> NativeProcessIdentity {
        self.process
    }

    pub const fn event_offset(&self) -> u64 {
        self.event_offset
    }

    pub const fn child(&self) -> Option<SessionId> {
        self.child
    }

    pub const fn conflicted(&self) -> bool {
        self.conflicted
    }

    pub fn seen(&self) -> &[SessionId] {
        &self.seen.0
    }

    /// The offset advances before age and identity filtering.
    pub fn observe(&mut self, observation: ForkObservation) -> ObservationResult {
        if observation.next_offset <= self.event_offset {
            return ObservationResult::IgnoredOffset;
        }
        self.event_offset = observation.next_offset;
        if observation.timestamp_ms < self.issued_ms.saturating_sub(1_000)
            || observation.child.is_nil()
            || observation.child == self.source
            || self.seen.0.contains(&observation.child)
        {
            return ObservationResult::Advanced;
        }
        match self.child {
            None => {
                self.child = Some(observation.child);
                ObservationResult::ChildRecorded
            }
            Some(child) if child == observation.child => ObservationResult::Advanced,
            Some(_) => {
                self.conflicted = true;
                ObservationResult::Conflict
            }
        }
    }

    pub fn evaluate(
        &self,
        bindings: &SlotMap,
        now_ms: u64,
        owner: OwnerResolution,
        discovery: Discovery<'_>,
    ) -> ForkDecision {
        if bindings.revision() != self.binding_revision
            || bindings.get(self.slot).map(|binding| binding.session_id()) != Some(self.source)
        {
            return ForkDecision::BindingChanged;
        }
        let Some(age) = now_ms.checked_sub(self.issued_ms) else {
            return ForkDecision::ClockRegressed;
        };
        let retention = if self.child.is_some() {
            KNOWN_CHILD_RETENTION_MS
        } else {
            UNKNOWN_CHILD_RETENTION_MS
        };
        if age > retention {
            return ForkDecision::Expired;
        }
        match owner {
            OwnerResolution::Unresolved => return ForkDecision::Deferred,
            OwnerResolution::Exited => return ForkDecision::LifetimeChanged,
            OwnerResolution::Live(process) if process != self.process => {
                return ForkDecision::LifetimeChanged;
            }
            OwnerResolution::Live(_) => {}
        }
        if self.conflicted {
            return ForkDecision::Ambiguous;
        }
        if !discovery.complete
            || discovery.candidates.len() > MAX_SEEN_SESSIONS
            || discovery
                .candidates
                .iter()
                .any(|candidate| matches!(candidate.access, CandidateAccess::Inaccessible))
        {
            return ForkDecision::Deferred;
        }
        let mut unique = HashSet::new();
        if discovery
            .candidates
            .iter()
            .any(|candidate| !unique.insert(candidate.session))
        {
            return ForkDecision::Ambiguous;
        }
        let mut eligible = discovery.candidates.iter().filter(|candidate| {
            !candidate.session.is_nil()
                && candidate.session != self.source
                && !self.seen.0.contains(&candidate.session)
                && self.child.is_none_or(|child| child == candidate.session)
                && matches!(
                    candidate.access,
                    CandidateAccess::Accessible {
                        verified_owner,
                        interactive: true
                    } if verified_owner == self.process
                )
        });
        match (eligible.next(), eligible.next()) {
            (None, _) => ForkDecision::Deferred,
            (Some(candidate), None) => ForkDecision::Rebind {
                child: candidate.session,
            },
            (Some(_), Some(_)) => ForkDecision::Ambiguous,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationResult {
    IgnoredOffset,
    Advanced,
    ChildRecorded,
    Conflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerResolution {
    Unresolved,
    Exited,
    Live(NativeProcessIdentity),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateAccess {
    Inaccessible,
    Accessible {
        verified_owner: NativeProcessIdentity,
        interactive: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Candidate {
    pub session: SessionId,
    pub access: CandidateAccess,
}

#[derive(Clone, Copy, Debug)]
pub struct Discovery<'a> {
    pub complete: bool,
    pub candidates: &'a [Candidate],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForkDecision {
    BindingChanged,
    ClockRegressed,
    Expired,
    Deferred,
    LifetimeChanged,
    Ambiguous,
    Rebind { child: SessionId },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForkError {
    InvalidRecord,
    TooLarge,
    Full,
    DuplicateId,
    DuplicateSlot,
    BindingChanged,
    NotFound,
    RevisionExhausted,
    DuplicateSession,
    BindingAdapterFailed,
}

impl fmt::Display for ForkError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRecord => "invalid pending fork",
            Self::TooLarge => "pending forks exceed their byte limit",
            Self::Full => "pending fork capacity reached",
            Self::DuplicateId => "pending fork ID already exists",
            Self::DuplicateSlot => "slot already has a pending fork",
            Self::BindingChanged => "fork binding changed",
            Self::NotFound => "pending fork not found",
            Self::RevisionExhausted => "binding revision exhausted",
            Self::DuplicateSession => "fork child is already bound",
            Self::BindingAdapterFailed => "could not apply fork binding",
        })
    }
}

impl std::error::Error for ForkError {}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PendingForks {
    forks: Vec<PendingFork>,
}

impl Serialize for PendingForks {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.forks.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PendingForks {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PendingVisitor;
        impl<'de> Visitor<'de> for PendingVisitor {
            type Value = PendingForks;

            fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str("at most six distinct pending forks")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut forks: Vec<PendingFork> = Vec::new();
                while let Some(fork) = sequence.next_element::<PendingFork>()? {
                    if forks.len() == FORK_CAPACITY
                        || forks
                            .iter()
                            .any(|other| other.id == fork.id || other.slot == fork.slot)
                    {
                        return Err(de::Error::custom("invalid pending forks"));
                    }
                    forks.push(fork);
                }
                Ok(PendingForks { forks })
            }
        }
        deserializer
            .deserialize_seq(PendingVisitor)
            .map_err(|_| de::Error::custom("invalid pending forks"))
    }
}

impl PendingForks {
    pub fn from_json(bytes: &[u8]) -> Result<Self, ForkError> {
        if bytes.len() > MAX_EVENT_BYTES {
            return Err(ForkError::TooLarge);
        }
        serde_json::from_slice(bytes).map_err(|_| ForkError::InvalidRecord)
    }

    pub fn len(&self) -> usize {
        self.forks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.forks.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &PendingFork> {
        self.forks.iter()
    }

    pub fn get(&self, id: ForkId) -> Option<&PendingFork> {
        self.forks.iter().find(|fork| fork.id == id)
    }

    pub fn begin(&mut self, fork: PendingFork, bindings: &SlotMap) -> Result<(), ForkError> {
        fork.validate()?;
        if bindings.revision() != fork.binding_revision
            || bindings.get(fork.slot).map(|binding| binding.session_id()) != Some(fork.source)
        {
            return Err(ForkError::BindingChanged);
        }
        if self.forks.iter().any(|other| other.id == fork.id) {
            return Err(ForkError::DuplicateId);
        }
        if self.forks.iter().any(|other| other.slot == fork.slot) {
            return Err(ForkError::DuplicateSlot);
        }
        if self.forks.len() == FORK_CAPACITY {
            return Err(ForkError::Full);
        }
        self.forks.push(fork);
        Ok(())
    }

    pub fn observe(
        &mut self,
        id: ForkId,
        observation: ForkObservation,
    ) -> Result<ObservationResult, ForkError> {
        self.forks
            .iter_mut()
            .find(|fork| fork.id == id)
            .ok_or(ForkError::NotFound)
            .map(|fork| fork.observe(observation))
    }

    /// Applies a rebind atomically, removing only terminal decisions.
    pub fn evaluate_and_apply(
        &mut self,
        id: ForkId,
        bindings: &mut SlotMap,
        now_ms: u64,
        owner: OwnerResolution,
        discovery: Discovery<'_>,
    ) -> Result<ForkDecision, ForkError> {
        let index = self
            .forks
            .iter()
            .position(|fork| fork.id == id)
            .ok_or(ForkError::NotFound)?;
        let fork = &self.forks[index];
        let decision = fork.evaluate(bindings, now_ms, owner, discovery);
        match decision {
            ForkDecision::Rebind { child } => {
                rebind_slot_map(bindings, fork, child)?;
                self.forks.remove(index);
            }
            ForkDecision::BindingChanged
            | ForkDecision::Expired
            | ForkDecision::LifetimeChanged => {
                self.forks.remove(index);
            }
            ForkDecision::Deferred | ForkDecision::Ambiguous | ForkDecision::ClockRegressed => {}
        }
        Ok(decision)
    }
}

// Isolated adapter until SlotMap exposes a dedicated revision-checked fork move.
fn rebind_slot_map(
    bindings: &mut SlotMap,
    fork: &PendingFork,
    child: SessionId,
) -> Result<(), ForkError> {
    if bindings.revision() != fork.binding_revision
        || bindings.get(fork.slot).map(|binding| binding.session_id()) != Some(fork.source)
    {
        return Err(ForkError::BindingChanged);
    }
    if bindings.find_session(child).is_some() {
        return Err(ForkError::DuplicateSession);
    }
    let revision = bindings
        .revision()
        .checked_add(1)
        .ok_or(ForkError::RevisionExhausted)?;
    let mut binding = bindings
        .get(fork.slot)
        .ok_or(ForkError::BindingChanged)?
        .clone();
    binding.set_session_id(child);
    binding.set_fork_parent(Some(fork.source));
    binding.set_initial_idle_confirmed(false);

    let mut value =
        serde_json::to_value(&*bindings).map_err(|_| ForkError::BindingAdapterFailed)?;
    let object = value
        .as_object_mut()
        .ok_or(ForkError::BindingAdapterFailed)?;
    object.insert("revision".to_owned(), revision.into());
    let slots = object
        .get_mut("slots")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(ForkError::BindingAdapterFailed)?;
    let replacement = serde_json::to_value(binding).map_err(|_| ForkError::BindingAdapterFailed)?;
    if slots
        .insert(fork.slot.number().to_string(), replacement)
        .is_none()
    {
        return Err(ForkError::BindingAdapterFailed);
    }
    let updated = serde_json::from_value(value).map_err(|_| ForkError::BindingAdapterFailed)?;
    *bindings = updated;
    Ok(())
}
