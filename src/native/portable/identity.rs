use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::RuntimeEpoch;
pub use crate::domain::{NativeProcessIdentity, OpaqueId, OpaqueIdError, TargetIdentity};

use super::strict::strict_record;

/// A registration is not a target identity or an authority grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Registration(pub OpaqueId);

strict_record! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    pub struct RunScope as RawRunScope {
        pub target: TargetIdentity,
        pub provider_epoch: RuntimeEpoch,
        #[serde(with = "super::uuid_wire")]
        pub run: Uuid,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOutcome {
    NotAdmitted,
    Completed,
    Unknown,
}

strict_record! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    pub struct ScopedOutcome as RawScopedOutcome {
        pub scope: RunScope,
        #[serde(with = "super::uuid_wire")]
        pub intent: Uuid,
        pub outcome: DeliveryOutcome,
    }
}

impl ScopedOutcome {
    pub fn blocks_scope(&self, candidate: &RunScope) -> bool {
        self.outcome == DeliveryOutcome::Unknown && self.scope == *candidate
    }
}
