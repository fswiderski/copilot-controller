//! Immutable provider snapshots and their presentation-only resolution.

use serde::{Deserialize, Serialize};

use crate::domain::{ClockSample, RefusalReason, RuntimeEpoch, Slot, Status};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Occupancy {
    Empty,
    Bound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Live,
    Stale,
    Exited,
    PidReused,
    Unresolved,
    Busy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selection {
    None,
    Pending,
    Confirmed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    Disarmed,
    ObserveOnly,
    ActionsGranted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayKind {
    Refused(RefusalReason),
    Accepted,
    Attention,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    pub kind: OverlayKind,
    pub generation: u64,
    pub started: ClockSample,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotSnapshot {
    pub occupancy: Occupancy,
    pub status: Status,
    pub availability: Availability,
    pub selection: Selection,
    pub cursor: bool,
    pub authority: Authority,
    pub overlay: Option<Overlay>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub epoch: RuntimeEpoch,
    pub generation: u64,
    pub connected: bool,
    /// Ordered by the shared one-based slots, exactly one entry per slot.
    pub slots: [SlotSnapshot; 6],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotError {
    NilExpectedEpoch,
    WrongEpoch,
    NonIncreasingGeneration,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NilExpectedEpoch => "the expected presentation epoch must not be nil",
            Self::WrongEpoch => "snapshot does not belong to the expected epoch",
            Self::NonIncreasingGeneration => "snapshot generation did not increase",
        })
    }
}

impl std::error::Error for SnapshotError {}

/// Replacing this holder with one for a fresh epoch is an explicit lifecycle cut.
#[derive(Clone, Debug)]
pub struct LatestSnapshot {
    expected_epoch: RuntimeEpoch,
    latest: Option<Snapshot>,
}

impl LatestSnapshot {
    pub fn new(expected_epoch: RuntimeEpoch) -> Result<Self, SnapshotError> {
        if expected_epoch.is_nil() {
            return Err(SnapshotError::NilExpectedEpoch);
        }
        Ok(Self {
            expected_epoch,
            latest: None,
        })
    }

    pub fn publish(&mut self, snapshot: Snapshot) -> Result<(), SnapshotError> {
        if snapshot.epoch != self.expected_epoch {
            return Err(SnapshotError::WrongEpoch);
        }
        if self
            .latest
            .as_ref()
            .is_some_and(|latest| snapshot.generation <= latest.generation)
        {
            return Err(SnapshotError::NonIncreasingGeneration);
        }
        self.latest = Some(snapshot);
        Ok(())
    }

    pub fn latest(&self) -> Option<&Snapshot> {
        self.latest.as_ref()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Token {
    Off,
    Idle,
    Working,
    Complete,
    Waiting,
    Error,
    Unknown,
    Empty,
    Stale,
    Exited,
    PidReused,
    Unresolved,
    Busy,
    Refused,
    Neutral,
}

impl From<Status> for Token {
    fn from(value: Status) -> Self {
        match value {
            Status::Off => Self::Off,
            Status::Idle => Self::Idle,
            Status::Working => Self::Working,
            Status::Complete => Self::Complete,
            Status::Waiting => Self::Waiting,
            Status::Error => Self::Error,
            Status::Unknown => Self::Unknown,
        }
    }
}

impl Token {
    pub const fn clears_context(self) -> bool {
        matches!(
            self,
            Self::Neutral
                | Self::Empty
                | Self::Stale
                | Self::Exited
                | Self::PidReused
                | Self::Unresolved
        )
    }

    pub const fn style(self) -> TokenStyle {
        use Token::*;
        let (rgb, fill, text, word, glyph) = match self {
            Off => ([0, 0, 0], [97, 97, 97], [255, 255, 255], "OFF", None),
            Idle => (
                [255, 255, 255],
                [255, 255, 255],
                [0, 0, 0],
                "IDLE",
                Some("state-idle"),
            ),
            Working => (
                [32, 112, 255],
                [32, 112, 255],
                [0, 0, 0],
                "WORKING",
                Some("state-working"),
            ),
            Complete => (
                [0, 208, 96],
                [0, 208, 96],
                [0, 0, 0],
                "DONE",
                Some("state-complete"),
            ),
            Waiting => (
                [255, 170, 0],
                [255, 170, 0],
                [0, 0, 0],
                "WAITING",
                Some("state-waiting"),
            ),
            Error => (
                [255, 48, 48],
                [255, 48, 48],
                [0, 0, 0],
                "ERROR",
                Some("state-error"),
            ),
            Unknown => (
                [96, 125, 139],
                [96, 125, 139],
                [0, 0, 0],
                "UNKNOWN",
                Some("state-unknown"),
            ),
            Empty => ([0, 0, 0], [48, 48, 48], [176, 176, 176], "EMPTY", None),
            Stale => ([93, 64, 55], [93, 64, 55], [255, 213, 79], "STALE", None),
            Exited => ([93, 64, 55], [93, 64, 55], [255, 213, 79], "EXITED", None),
            PidReused => (
                [93, 64, 55],
                [93, 64, 55],
                [255, 213, 79],
                "PID REUSED",
                None,
            ),
            Unresolved => (
                [93, 64, 55],
                [93, 64, 55],
                [255, 213, 79],
                "NOT FOUND",
                None,
            ),
            Busy => (
                [106, 27, 154],
                [106, 27, 154],
                [255, 255, 255],
                "BUSY",
                None,
            ),
            Refused => (
                [191, 54, 12],
                [191, 54, 12],
                [255, 255, 255],
                "REFUSED",
                None,
            ),
            Neutral => ([96, 125, 139], [96, 125, 139], [0, 0, 0], "OFFLINE", None),
        };
        TokenStyle {
            rgb,
            fill,
            text,
            word,
            glyph,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct TokenStyle {
    pub rgb: [u8; 3],
    pub fill: [u8; 3],
    pub text: [u8; 3],
    pub word: &'static str,
    pub glyph: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SelectionStyle {
    pub rgb: [u8; 3],
    pub word: &'static str,
    pub dashed: bool,
}

impl Selection {
    pub const fn style(self) -> Option<SelectionStyle> {
        match self {
            Self::None => None,
            Self::Confirmed => Some(SelectionStyle {
                rgb: [255, 255, 255],
                word: "SELECTED",
                dashed: false,
            }),
            Self::Pending => Some(SelectionStyle {
                rgb: [0, 229, 255],
                word: "PENDING",
                dashed: true,
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectRequest {
    Steady,
    SlowPulse,
    Attention,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SemanticSlot {
    pub slot: Slot,
    pub token: Token,
    pub selection: Selection,
    pub cursor: bool,
    pub authority: Authority,
    pub overlay: Option<OverlayKind>,
    pub effect: EffectRequest,
}

/// Human-readable refusal detail, separate from the REFUSED status word.
pub const fn refusal_word(reason: RefusalReason) -> &'static str {
    match reason {
        RefusalReason::Disabled => "DISABLED",
        RefusalReason::Unsupported => "UNSUPPORTED",
        RefusalReason::UnknownUi => "UNKNOWN UI",
        RefusalReason::StaleState => "STALE",
        RefusalReason::ClockMismatch => "CLOCK",
        RefusalReason::WrongTarget => "WRONG TARGET",
        RefusalReason::ChangedBinding => "REBOUND",
        RefusalReason::ChangedGeneration => "OUTDATED",
        RefusalReason::ChangedRun => "NEW RUN",
        RefusalReason::ChangedDraft => "DRAFT EDIT",
        RefusalReason::ChangedDialog => "NEW DIALOG",
        RefusalReason::CommandDraft => "COMMAND",
        RefusalReason::EmptyDraft => "EMPTY DRAFT",
        RefusalReason::Busy => "BUSY",
        RefusalReason::BroadApproval => "TOO BROAD",
        RefusalReason::Duplicate => "DUPLICATE",
        RefusalReason::IndeterminateDelivery => "UNCERTAIN",
        RefusalReason::PendingInput => "INPUT WAIT",
        RefusalReason::Unavailable => "UNAVAILABLE",
        RefusalReason::UnsafeContext => "UNSAFE",
        RefusalReason::ExpiredIntent => "EXPIRED",
    }
}

impl SemanticSlot {
    pub fn refusal_detail(self) -> Option<&'static str> {
        match (self.token, self.overlay) {
            (Token::Refused, Some(OverlayKind::Refused(reason))) => Some(refusal_word(reason)),
            _ => None,
        }
    }
}

fn overlay_is_current(overlay: Overlay, snapshot: &Snapshot, now: ClockSample) -> bool {
    if overlay.generation != snapshot.generation || now.elapsed_since(overlay.started).is_err() {
        return false;
    }
    let delta = now.stamp.ticks - overlay.started.stamp.ticks;
    u128::from(delta) < u128::from(now.stamp.frequency) * 2
}

/// Resolve tokens and independently de-ambiguate selection without mutating the provider.
pub fn resolve(snapshot: &Snapshot, now: ClockSample) -> [SemanticSlot; 6] {
    let clock_valid =
        !snapshot.epoch.is_nil() && now.stamp.epoch == snapshot.epoch && now.stamp.frequency != 0;
    let count = |selection| {
        snapshot
            .slots
            .iter()
            .filter(|slot| slot.selection == selection)
            .count()
    };
    let ambiguous_confirmed = count(Selection::Confirmed) > 1;
    let ambiguous_pending = count(Selection::Pending) > 1;

    std::array::from_fn(|index| {
        let source = snapshot.slots[index];
        let overlay = source
            .overlay
            .filter(|overlay| clock_valid && overlay_is_current(*overlay, snapshot, now))
            .map(|overlay| overlay.kind);
        let token = if !snapshot.connected || !clock_valid {
            Token::Neutral
        } else if source.occupancy == Occupancy::Empty {
            Token::Empty
        } else {
            match source.availability {
                Availability::Stale => Token::Stale,
                Availability::Exited => Token::Exited,
                Availability::PidReused => Token::PidReused,
                Availability::Unresolved => Token::Unresolved,
                Availability::Live | Availability::Busy => {
                    if matches!(overlay, Some(OverlayKind::Refused(_))) {
                        Token::Refused
                    } else if source.availability == Availability::Busy {
                        Token::Busy
                    } else {
                        source.status.into()
                    }
                }
            }
        };
        let ambiguous = match source.selection {
            Selection::Confirmed => ambiguous_confirmed,
            Selection::Pending => ambiguous_pending,
            Selection::None => false,
        };
        let clear = token.clears_context();
        let selection = if clear || ambiguous {
            Selection::None
        } else {
            source.selection
        };
        let visible_overlay = if clear { None } else { overlay };
        let effect = if clear {
            EffectRequest::Steady
        } else if matches!(token, Token::Refused | Token::Waiting | Token::Error)
            || visible_overlay == Some(OverlayKind::Attention)
        {
            EffectRequest::Attention
        } else if selection == Selection::Confirmed || token == Token::Working {
            EffectRequest::SlowPulse
        } else {
            EffectRequest::Steady
        };
        SemanticSlot {
            slot: Slot::ALL[index],
            token,
            selection,
            cursor: source.cursor && !clear && !ambiguous,
            authority: if clear || ambiguous {
                Authority::Disarmed
            } else {
                source.authority
            },
            overlay: visible_overlay,
            effect,
        }
    })
}
