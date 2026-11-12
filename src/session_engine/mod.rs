//! Offline session ingestion, fork bookkeeping, and structural policy.
//!
//! These APIs consume supplied evidence only. They neither discover sessions nor
//! authenticate a host, deliver input, access devices, or record a journal.
//! Enable serde_json's `raw_value` feature for exact numeric ingress.

pub mod event;
pub mod fork;
pub mod hook;
pub mod policy;
pub mod reducer;
pub mod tail;

mod strict;

#[cfg(test)]
mod tests;
