//! Durable state operations over explicitly approved native private roots.

pub mod ownership;
pub mod root;
pub mod state_store;

pub use ownership::{OwnershipError, OwnershipErrorKind, RuntimeOwnerLease};
pub use root::{FileIdentity, PrivateRoot, RootError, RootErrorKind};
