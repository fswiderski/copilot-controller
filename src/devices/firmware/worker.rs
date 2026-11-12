use super::options::{UpdateOptions, validate_options};
use super::report::{UpdateOutcome, UpdatePhase, UpdateReport};

/// Native maintenance remains unavailable until its transport, safe-root
/// binding, and process-group supervisor are implemented and reviewed.
pub fn run_maintenance_worker(options: &UpdateOptions) -> UpdateOutcome {
    let mut report = UpdateReport::default();
    if let Err(error) = validate_options(options) {
        return UpdateOutcome::failed(report, error.to_string());
    }
    report.phase = UpdatePhase::Preflight;
    let reason = if !cfg!(feature = "micro-firmware") {
        "maintenance requires the micro-firmware feature"
    } else if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "native maintenance is available only on macOS ARM64"
    } else {
        "native maintenance transport, safe-root binding, and supervisor are pending"
    };
    UpdateOutcome::failed(report, reason)
}
