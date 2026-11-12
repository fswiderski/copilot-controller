//! Offline firmware planning and a test-only maintenance transaction model.
//!
//! This module contains no native device, process, registry, lease, or supervisor
//! binding. In particular, a successful transaction against a fake backend is
//! not device qualification or authorization to perform maintenance.
//!
//! The worker entry point fails closed. Do not expose the injected model as a
//! CLI worker: native integration must independently provide the supervised
//! process group, cancellation and output bounds, shared lease, qualified USB
//! and registry bindings, and race-resistant approved APFS recovery roots.
//! Ancestor, ownership, ACL, and substitution checks are not supplied by this
//! synthetic model's workspace helper.
//! The backend must fully flush the backup, checksum, workspace and its parent
//! to permanent storage; on macOS this means F_FULLFSYNC with no fsync fallback.
//! The test backend's success is not proof of durability.
//!
//! The model and its backend are not available to external callers:
//! ```compile_fail
//! use copilot_controller::devices::firmware::MaintenanceBackend;
//! ```
//! ```compile_fail
//! use copilot_controller::devices::firmware::run_transaction_model;
//! ```
//!
//! Tests require `COPILOT_FIRMWARE_FIXTURE_ROOT` to explicitly name an existing
//! fixture root; each test creates and later removes only its own private child.

#![forbid(unsafe_code)]

mod artifacts;
#[cfg(test)]
mod backend;
#[cfg(test)]
mod identity;
mod options;
mod plan;
mod report;
#[cfg(test)]
mod transaction;
mod worker;
#[cfg(test)]
mod workspace;

pub use options::{CONFIRMATION, OptionError, UpdateOptions, validate_options};
pub use plan::{
    FirmwareCapability, FirmwarePlan, MaintenanceEvidence, MaintenancePath, MaintenancePlatform,
    MaintenanceTransport, micro_capability, offline_plan, unavailable_capability,
};
pub use report::{UpdateOutcome, UpdatePhase, UpdateReport};
pub use worker::run_maintenance_worker;

use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareError {
    message: String,
}

impl FirmwareError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FirmwareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FirmwareError {}

#[cfg(test)]
mod tests;
