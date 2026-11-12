//! Portable, read-only native-service contracts and bounded diagnostics.
//!
//! The domain identities and monotonic stamps are shared with the foundation.
//! Diagnostic child execution is crate-private and is not a request API.
//!
//! Integration requires `serde` with derive, `serde_json`, `uuid` with v4, and
//! `libc` on Linux/macOS, plus the foundation's `crate::domain` reexports.
//! The enclosing crate must expose this module and wire its internal child
//! supervisor only to preselected diagnostic operations.
//!
//! Windows observation and Job-worker backends, worker routing/allowlisting,
//! safe-root runtime ownership, lifecycle notifications, device backends, and
//! maintenance imports are not implemented here. A group or direct-child
//! cleanup is not action-writer quiescence or a descendant-escape guarantee.

pub mod child;
pub mod clock;
pub mod contracts;
pub mod identity;
pub mod inventory;
pub mod observation;
pub mod protocol;

mod strict;
mod uuid_wire;

#[cfg(test)]
mod tests;
