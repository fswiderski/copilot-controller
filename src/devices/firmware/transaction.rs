use std::ffi::OsString;
use std::time::Duration;

use super::FirmwareError;
use super::artifacts::{
    ArtifactPin, FLASH_SIZE, IMAGE_PIN, PARTITION_END, PARTITION_START, SETTINGS_END,
    SETTINGS_SIZE, SETTINGS_START, TOOL_PIN, verify_artifact, verify_image_extent,
};
use super::backend::{
    MaintenanceBackend, ToolInvocation, ToolOperation, ToolOutput, WorkerPlatform,
};
use super::identity::{
    DescriptorPin, USB_DESCRIPTOR_PINS, bootloader_report, open_qualified, query_version,
    require_application, require_bound_bootloader, wait_for_application, wait_for_bootloader,
};
use super::options::{UpdateOptions, validate_options};
use super::report::{UpdateOutcome, UpdatePhase, UpdateReport};
pub(crate) use super::worker::run_maintenance_worker;
use super::workspace::Workspace;

const OUTPUT_LIMIT: usize = 4_194_304;

#[derive(Clone, Copy)]
struct ModelPins<'a> {
    feature_enabled: bool,
    image: ArtifactPin<'a>,
    tool: ArtifactPin<'a>,
    descriptors: &'a [DescriptorPin<'a>],
}

fn backend_error(error: impl std::fmt::Display) -> FirmwareError {
    FirmwareError::new(error.to_string())
}

pub(crate) fn run_transaction_model<B: MaintenanceBackend>(
    options: &UpdateOptions,
    backend: &mut B,
) -> UpdateOutcome {
    run_model_with_pins(
        options,
        backend,
        ModelPins {
            feature_enabled: cfg!(feature = "micro-firmware"),
            image: IMAGE_PIN,
            tool: TOOL_PIN,
            descriptors: USB_DESCRIPTOR_PINS,
        },
    )
}

fn run_model_with_pins<B: MaintenanceBackend>(
    options: &UpdateOptions,
    backend: &mut B,
    pins: ModelPins<'_>,
) -> UpdateOutcome {
    let mut report = UpdateReport::default();
    if let Err(error) = validate_options(options) {
        return UpdateOutcome::failed(report, error.to_string());
    }
    report.phase = UpdatePhase::Preflight;
    match run_inner(options, backend, &mut report, pins) {
        Err(error) => UpdateOutcome::failed(report, error.to_string()),
        Ok(()) => {
            let outcome = UpdateOutcome::success(report.clone());
            match outcome.validate_supervisor() {
                Ok(()) => outcome,
                Err(error) => UpdateOutcome::failed(report, format!("invalid success: {error}")),
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn run_synthetic_model<'a, B: MaintenanceBackend>(
    options: &UpdateOptions,
    backend: &mut B,
    feature_enabled: bool,
    image: ArtifactPin<'a>,
    tool: ArtifactPin<'a>,
    descriptors: &'a [DescriptorPin<'a>],
) -> UpdateOutcome {
    run_model_with_pins(
        options,
        backend,
        ModelPins {
            feature_enabled,
            image,
            tool,
            descriptors,
        },
    )
}

fn run_inner<B: MaintenanceBackend>(
    options: &UpdateOptions,
    backend: &mut B,
    report: &mut UpdateReport,
    pins: ModelPins<'_>,
) -> Result<(), FirmwareError> {
    if !pins.feature_enabled {
        return Err(FirmwareError::new(
            "maintenance requires the micro-firmware feature",
        ));
    }
    backend.check_cancelled().map_err(backend_error)?;
    let context = backend.worker_context().map_err(backend_error)?;
    if context.platform != WorkerPlatform::MacosAarch64 {
        return Err(FirmwareError::new(
            "maintenance worker requires macOS ARM64",
        ));
    }
    if context.process_id == 0 || context.process_id != context.process_group_id {
        return Err(FirmwareError::new(
            "maintenance worker must be its process-group leader",
        ));
    }
    backend.check_cancelled().map_err(backend_error)?;
    let _lease = backend.acquire_lease().map_err(backend_error)?;
    backend.check_cancelled().map_err(backend_error)?;
    let image = verify_artifact(&options.image, pins.image)?;
    verify_image_extent(&image)?;
    let tool = verify_artifact(&options.esptool, pins.tool)?;
    backend.check_cancelled().map_err(backend_error)?;
    backend.set_private_umask(0o077).map_err(backend_error)?;

    report.phase = UpdatePhase::PreparingPrivateBackup;
    backend.check_cancelled().map_err(backend_error)?;
    backend
        .authorize_backup_path(&options.backup_dir)
        .map_err(backend_error)?;
    let workspace = Workspace::prepare(&options.backup_dir, &image, &tool)?;
    let mut hid_open = false;
    let result = run_in_workspace(
        options,
        backend,
        report,
        pins,
        &workspace,
        &image,
        &mut hid_open,
    );
    if hid_open {
        let _ = backend.close_hid();
    }
    result
}

fn os(value: &str) -> OsString {
    OsString::from(value)
}

fn invocation(operation: ToolOperation, workspace: &Workspace, port: &str) -> ToolInvocation {
    let mut arguments = Vec::new();
    if operation != ToolOperation::Version {
        let after = if operation == ToolOperation::Reset {
            "watchdog-reset"
        } else {
            "no-reset"
        };
        arguments.extend(
            [
                "--chip",
                "esp32s3",
                "--port",
                port,
                "--before",
                "no-reset",
                "--after",
                after,
                "--connect-attempts",
                "1",
            ]
            .into_iter()
            .map(os),
        );
    }
    let timeout = match operation {
        ToolOperation::Version => {
            arguments.push(os("version"));
            10
        }
        ToolOperation::Security => {
            arguments.push(os("get-security-info"));
            60
        }
        ToolOperation::FlashId => {
            arguments.push(os("flash-id"));
            60
        }
        ToolOperation::ReadBackup => {
            arguments.extend(["read-flash", "0", "0x1000000"].into_iter().map(os));
            arguments.push(workspace.path("full-flash.bin").into_os_string());
            240
        }
        ToolOperation::VerifyBackup => {
            arguments.extend(["verify-flash", "0"].into_iter().map(os));
            arguments.push(workspace.path("full-flash.bin").into_os_string());
            240
        }
        ToolOperation::WriteImage => {
            arguments.extend(
                [
                    "write-flash",
                    "--flash-mode",
                    "keep",
                    "--flash-freq",
                    "keep",
                    "--flash-size",
                    "keep",
                    "0",
                ]
                .into_iter()
                .map(os),
            );
            arguments.push(workspace.path("firmware.bin").into_os_string());
            180
        }
        ToolOperation::VerifyImage => {
            arguments.extend(["verify-flash", "0"].into_iter().map(os));
            arguments.push(workspace.path("firmware.bin").into_os_string());
            120
        }
        ToolOperation::ReadSettings => {
            arguments.extend(["read-flash", "0x810000", "0x220000"].into_iter().map(os));
            arguments.push(workspace.path("settings-after.bin").into_os_string());
            120
        }
        ToolOperation::Reset => {
            arguments.push(os("read-mac"));
            60
        }
    };
    ToolInvocation {
        operation,
        program: workspace.path("esptool"),
        arguments,
        environment: vec![
            (os("PATH"), os("/usr/bin:/bin:/usr/sbin:/sbin")),
            (os("TMPDIR"), workspace.path("tool-tmp").into_os_string()),
            (os("LC_ALL"), os("C")),
            (os("LANG"), os("C")),
            (os("TERM"), os("dumb")),
            (os("NO_COLOR"), os("1")),
        ],
        clear_environment: true,
        null_stdin: true,
        timeout: Duration::from_secs(timeout),
        stdout_limit: OUTPUT_LIMIT,
        stderr_limit: OUTPUT_LIMIT,
    }
}

struct ToolRunner<'a, B> {
    backend: &'a mut B,
    workspace: &'a Workspace,
    options: &'a UpdateOptions,
    report: &'a mut UpdateReport,
    pins: ModelPins<'a>,
    image: &'a [u8],
    location: Option<u64>,
}

impl<B: MaintenanceBackend> ToolRunner<'_, B> {
    fn call(&mut self, operation: ToolOperation) -> Result<String, FirmwareError> {
        self.call_with_backup(operation, None)
    }

    fn call_with_backup(
        &mut self,
        operation: ToolOperation,
        evidence: Option<(&[u8], &str)>,
    ) -> Result<String, FirmwareError> {
        if operation == ToolOperation::WriteImage && self.report.flash_write_attempted {
            return Err(FirmwareError::new("firmware write was already attempted"));
        }
        if operation == ToolOperation::VerifyImage && !self.report.flash_write_attempted {
            return Err(FirmwareError::new("firmware was not written"));
        }
        if operation == ToolOperation::ReadSettings && !self.report.firmware_verified {
            return Err(FirmwareError::new("firmware was not verified"));
        }
        if matches!(operation, ToolOperation::WriteImage | ToolOperation::Reset) {
            if !self.report.backup_verified {
                return Err(FirmwareError::new("backup has not been verified"));
            }
            if operation == ToolOperation::Reset
                && !(self.report.flash_write_attempted
                    && self.report.firmware_verified
                    && self.report.settings_preserved)
            {
                return Err(FirmwareError::new("reset prerequisites are incomplete"));
            }
            if evidence.is_none() {
                return Err(FirmwareError::new("durable backup evidence is missing"));
            }
        }
        self.backend.check_cancelled().map_err(backend_error)?;
        self.workspace.check_tool_tmp()?;
        let _ = self
            .workspace
            .verify_staged("esptool", 0o700, self.pins.tool)?;
        let port = self
            .options
            .port
            .to_str()
            .ok_or_else(|| FirmwareError::new("validated port is not UTF-8"))?;
        if operation != ToolOperation::Version {
            let location = self
                .location
                .ok_or_else(|| FirmwareError::new("USB location was not qualified"))?;
            require_bound_bootloader(self.backend, location, port)?;
        }
        if matches!(
            operation,
            ToolOperation::WriteImage | ToolOperation::VerifyImage
        ) && self
            .workspace
            .verify_staged("firmware.bin", 0o600, self.pins.image)?
            != self.image
        {
            return Err(FirmwareError::new("staged image changed before use"));
        }
        if let Some((backup, checksum)) = evidence {
            self.workspace.verify_backup_evidence(backup, checksum)?;
        }
        self.backend.check_cancelled().map_err(backend_error)?;
        let invocation = invocation(operation, self.workspace, port);
        if operation == ToolOperation::WriteImage {
            self.report.flash_write_attempted = true;
        }
        let output = self
            .backend
            .invoke_tool(&invocation)
            .map_err(backend_error)?;
        self.collect_output(operation, output)
    }

    fn collect_output(
        &self,
        operation: ToolOperation,
        output: ToolOutput,
    ) -> Result<String, FirmwareError> {
        if output.stdout.len() > OUTPUT_LIMIT || output.stderr.len() > OUTPUT_LIMIT {
            return Err(FirmwareError::new("tool output exceeded its limit"));
        }
        let mut combined = output.stdout;
        combined.extend_from_slice(&output.stderr);
        self.workspace
            .create_file(operation.log_name(), &combined, 0o600)?;
        if output.exit_code != 0 {
            return Err(FirmwareError::new(format!(
                "{operation:?} failed; see private log; no retry"
            )));
        }
        String::from_utf8(combined)
            .map_err(|_| FirmwareError::new(format!("{operation:?} output is not UTF-8")))
    }
}

fn accepted_tool_version(output: &str) -> bool {
    output
        .lines()
        .map(str::trim)
        .any(|line| matches!(line, "5.6.0" | "esptool v5.6.0"))
}

pub(crate) fn check_security(output: &str) -> Result<(), FirmwareError> {
    if output.contains("Secure Boot: Enabled") || output.contains("Flash Encryption: Enabled") {
        return Err(FirmwareError::new("enabled security feature detected"));
    }
    let lines: Vec<&str> = output.lines().map(str::trim).collect();
    if ![
        "Secure Boot: Disabled",
        "Flash Encryption: Disabled",
        "Flags: 0x00000000 (0b0)",
    ]
    .iter()
    .all(|required| lines.contains(required))
    {
        return Err(FirmwareError::new(
            "security output lacks the exact disabled-feature evidence",
        ));
    }
    Ok(())
}

pub(crate) fn check_flash_size(output: &str) -> Result<(), FirmwareError> {
    let sizes: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("Detected flash size:"))
        .collect();
    if sizes.as_slice() != ["Detected flash size: 16MB"] {
        return Err(FirmwareError::new(
            "flash size output must contain exactly one 16MB line",
        ));
    }
    Ok(())
}

fn close_hid<B: MaintenanceBackend>(backend: &mut B, open: &mut bool) -> Result<(), FirmwareError> {
    *open = false;
    backend.close_hid().map_err(backend_error)
}

fn run_in_workspace<B: MaintenanceBackend>(
    options: &UpdateOptions,
    backend: &mut B,
    report: &mut UpdateReport,
    pins: ModelPins<'_>,
    workspace: &Workspace,
    image: &[u8],
    hid_open: &mut bool,
) -> Result<(), FirmwareError> {
    let mut runner = ToolRunner {
        backend,
        workspace,
        options,
        report,
        pins,
        image,
        location: None,
    };
    runner.report.phase = UpdatePhase::ToolVersion;
    if !accepted_tool_version(&runner.call(ToolOperation::Version)?) {
        return Err(FirmwareError::new("unreviewed tool version"));
    }

    runner.report.phase = UpdatePhase::QualifyingUsbTarget;
    let location = require_application(runner.backend)?;
    open_qualified(runner.backend, hid_open, pins.descriptors, location)?;
    let version = query_version(runner.backend)?;
    if !matches!(version.as_str(), "v0.1.50" | "0.6.3") {
        return Err(FirmwareError::new(format!(
            "unreviewed starting firmware version: {version}"
        )));
    }
    if require_application(runner.backend)? != location {
        return Err(FirmwareError::new(
            "application USB location changed before bootloader entry",
        ));
    }
    workspace.check_tool_tmp()?;
    let _ = workspace.verify_staged("esptool", 0o700, pins.tool)?;
    if workspace.verify_staged("firmware.bin", 0o600, pins.image)? != image {
        return Err(FirmwareError::new(
            "staged image changed before bootloader entry",
        ));
    }

    runner.report.phase = UpdatePhase::Bootloader;
    let bootloader = bootloader_report()?;
    runner.backend.check_cancelled().map_err(backend_error)?;
    runner.report.bootloader_entry_attempted = true;
    let count = runner
        .backend
        .write_hid(&bootloader)
        .map_err(backend_error)?;
    if count != 64 {
        return Err(FirmwareError::new(format!(
            "bootloader report write was short: {count}"
        )));
    }
    close_hid(runner.backend, hid_open)?;
    let port = options
        .port
        .to_str()
        .ok_or_else(|| FirmwareError::new("validated port is not UTF-8"))?;
    wait_for_bootloader(runner.backend, location, port)?;
    runner.location = Some(location);

    runner.report.phase = UpdatePhase::Security;
    check_security(&runner.call(ToolOperation::Security)?)?;
    runner.report.phase = UpdatePhase::FlashSize;
    check_flash_size(&runner.call(ToolOperation::FlashId)?)?;

    runner.report.phase = UpdatePhase::ReadingBackup;
    let _ = runner.call(ToolOperation::ReadBackup)?;
    let backup = workspace.read_private("full-flash.bin", FLASH_SIZE)?;
    if backup[PARTITION_START..PARTITION_END] != image[PARTITION_START..PARTITION_END] {
        return Err(FirmwareError::new(
            "backup partition sector differs from the reviewed image",
        ));
    }

    runner.report.phase = UpdatePhase::PersistingBackup;
    runner.backend.check_cancelled().map_err(backend_error)?;
    let checksum = workspace.persist_backup(runner.backend, &backup)?;

    runner.report.phase = UpdatePhase::VerifyingBackup;
    let _ = runner.call(ToolOperation::VerifyBackup)?;
    if workspace.read_private("full-flash.bin", FLASH_SIZE)? != backup
        || workspace.verify_staged("firmware.bin", 0o600, pins.image)? != image
    {
        return Err(FirmwareError::new(
            "backup or staged image changed during verification",
        ));
    }
    runner.report.backup_verified = true;

    runner.report.phase = UpdatePhase::WritingFirmware;
    let _ = runner.call_with_backup(ToolOperation::WriteImage, Some((&backup, &checksum)))?;
    runner.report.phase = UpdatePhase::VerifyingFirmware;
    let _ = runner.call(ToolOperation::VerifyImage)?;
    runner.report.firmware_verified = true;

    runner.report.phase = UpdatePhase::VerifyingSettings;
    let _ = runner.call(ToolOperation::ReadSettings)?;
    let settings = workspace.read_private("settings-after.bin", SETTINGS_SIZE as u64)?;
    if settings != backup[SETTINGS_START..SETTINGS_END] {
        return Err(FirmwareError::new("preserved settings span changed"));
    }
    runner.report.settings_preserved = true;

    runner.report.phase = UpdatePhase::Resetting;
    let _ = runner.call_with_backup(ToolOperation::Reset, Some((&backup, &checksum)))?;
    runner.report.phase = UpdatePhase::VerifyingRunningFirmware;
    wait_for_application(runner.backend, location)?;
    open_qualified(runner.backend, hid_open, pins.descriptors, location)?;
    let final_version = query_version(runner.backend)?;
    runner.report.running_version = Some(final_version.clone());
    if final_version != "0.6.3" {
        return Err(FirmwareError::new(format!(
            "unexpected running firmware version: {final_version}"
        )));
    }
    if require_application(runner.backend)? != location {
        return Err(FirmwareError::new(
            "application USB location changed during final verification",
        ));
    }
    runner.backend.check_cancelled().map_err(backend_error)?;
    workspace.verify_backup_evidence(&backup, &checksum)?;
    close_hid(runner.backend, hid_open)?;
    runner.backend.check_cancelled().map_err(backend_error)?;
    Ok(())
}
