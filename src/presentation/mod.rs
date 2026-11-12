//! Offline six-slot presentation and logical-input contracts.
//!
//! This module does not discover devices, write device reports, deliver input,
//! activate candidate bindings, or establish physical qualification. Evidence
//! supplied by an integrator is a scoped assertion, not a discovery result.
//!
//! Integrate this directory with `pub mod presentation` in the owning crate;
//! it uses `crate::domain` shared types plus `serde`, `serde_json`, and `sha2`
//! 0.10. Artwork is compiled in and verified against its bundled manifest.
//! `semantic` resolves immutable snapshots; `evidence` and `profiles` describe
//! exact scopes; `realization` prepares untransported views and AKP03 SVG;
//! `input` validates roles and normalizes/admits discrete events; `assignment`
//! returns sanitized candidates only.
//!
//! Device transports, native evidence acquisition, and private-root export are
//! deliberately not implemented here. `PrivateExportRoot` is a contract for
//! the separately integrated native root service; absence is an explicit error.
//! Original AKP03 press observations have no invented releases, the CT adapter
//! drops touch, and the Micro strict adapter does not emit encoder presses,
//! microphone events, or D-pad motion. The separate legacy Mic coalescer emits
//! only a boolean with its own 400 ms interval. A tracker requires explicit
//! reset after disconnect, mode/clock cut, or held-control overflow. Neither a
//! demo nor an offline assignment can establish qualification or command readiness.
//! Screen output requires exact-scope physical evidence for each rendered feature.
//! A clock-invalid release does not clear a tainted gesture; a valid matched
//! release or explicit reset is required before turns can be admitted again.

pub mod artwork;
pub mod assignment;
pub mod evidence;
pub mod input;
pub mod profiles;
pub mod realization;
pub mod semantic;

#[cfg(test)]
mod tests;
