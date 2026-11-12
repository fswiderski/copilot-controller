//! Micro protocol, input, LED, snapshot, and bounded diagnostic policies.
//!
//! The codec APIs consume supplied bytes, snapshots, evidence, and clock samples.
//! The explicit `probe` module is separate and uses injected transports for its
//! state machine; it never dispatches input or establishes physical qualification.
//!
//! Dependencies: `serde` (derive), `serde_json` (with `arbitrary_precision` and
//! `raw_value`), `unicode-general-category` 1.1 (Unicode 16), and `ryu`.
//!
pub mod calibration;
pub mod enumeration;
pub mod error;
pub mod health;
pub mod input;
pub mod led;
pub mod probe;
pub mod rpc;
pub mod value;
pub mod wire;

mod json;

#[cfg(test)]
mod tests;
