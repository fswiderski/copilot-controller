//! Offline controller contracts, codecs, and explicitly invoked diagnostics.
//!
//! Configuration entry points enforce standalone byte limits. The same types
//! implement strict Serde deserialization for embedding in larger, separately
//! bounded records. There is no implicit discovery or action-delivery interface.
//! Native read-only observation APIs require explicit invocation; the CLI never
//! invokes serial/device discovery. Firmware execution remains unavailable.

pub mod cli;
pub mod devices;
pub mod domain;
pub mod native;
pub mod persistence;
pub mod presentation;
pub mod session_engine;
