use std::ffi::OsString;
use std::fs;
use std::time::Duration;

use super::super::UpdatePhase;
use super::super::artifacts::{SETTINGS_END, SETTINGS_START, sha256};
use super::super::backend::ToolOperation;
use super::super::transaction::{check_flash_size, check_security};
use super::common::{CandidateMutation, Fault, OpenedMutation, Run, run};

const ORDER: [ToolOperation; 9] = [
    ToolOperation::Version,
    ToolOperation::Security,
    ToolOperation::FlashId,
    ToolOperation::ReadBackup,
    ToolOperation::VerifyBackup,
    ToolOperation::WriteImage,
    ToolOperation::VerifyImage,
    ToolOperation::ReadSettings,
    ToolOperation::Reset,
];

fn failed(run: &Run, phase: UpdatePhase) {
    assert!(run.fixture.root.exists());
    assert!(!run.outcome.update_succeeded, "{:?}", run.backend.fault);
    assert_eq!(run.outcome.report.phase, UpdatePhase::Failed);
    assert_eq!(
        run.outcome.report.failure_phase,
        Some(phase),
        "{:?}: {:?}",
        run.backend.fault,
        run.outcome.error
    );
    assert!(!run.outcome.command_ready);
    assert!(!run.outcome.hardware_qualified);
    assert!(
        run.outcome
            .error
            .as_ref()
            .is_some_and(|error| !error.is_empty())
    );
    assert!(run.outcome.validate_supervisor().is_ok());
}

#[test]
fn all_prewrite_gates_fail_closed() {
    use Fault::*;
    use ToolOperation as Op;
    use UpdatePhase as Phase;
    let cases = vec![
        (FeatureDisabled, Phase::Preflight, false),
        (CancelBeforeContext, Phase::Preflight, false),
        (ContextError, Phase::Preflight, false),
        (UnsupportedPlatform, Phase::Preflight, false),
        (Unsupervised, Phase::Preflight, false),
        (Lease, Phase::Preflight, false),
        (ImageHash, Phase::Preflight, false),
        (ToolHash, Phase::Preflight, false),
        (ImageHeader, Phase::Preflight, false),
        (ImageExtent, Phase::Preflight, false),
        (Umask, Phase::Preflight, false),
        (UnsafeParent, Phase::PreparingPrivateBackup, false),
        (ExistingBackup, Phase::PreparingPrivateBackup, false),
        (ToolExit(Op::Version), Phase::ToolVersion, false),
        (ToolTimeout(Op::Version), Phase::ToolVersion, false),
        (ToolOverflow(Op::Version), Phase::ToolVersion, false),
        (ToolStderrOverflow(Op::Version), Phase::ToolVersion, false),
        (ToolCancelled(Op::Version), Phase::ToolVersion, false),
        (ToolVersion, Phase::ToolVersion, false),
        (ToolNonUtf8, Phase::ToolVersion, false),
        (ToolTempEvidence, Phase::ToolVersion, false),
        (InitialRegistryError, Phase::QualifyingUsbTarget, false),
        (InitialRegistryAbsent, Phase::QualifyingUsbTarget, false),
        (DuplicateApplication, Phase::QualifyingUsbTarget, false),
        (ZeroLocation, Phase::QualifyingUsbTarget, false),
        (RegistryTooDeep, Phase::QualifyingUsbTarget, false),
        (EnumerationError, Phase::QualifyingUsbTarget, false),
        (NoHid, Phase::QualifyingUsbTarget, false),
        (DuplicateHid, Phase::QualifyingUsbTarget, false),
        (OpenError, Phase::QualifyingUsbTarget, false),
        (OpenedIdentity, Phase::QualifyingUsbTarget, false),
        (Descriptor, Phase::QualifyingUsbTarget, false),
        (DescriptorLength, Phase::QualifyingUsbTarget, false),
        (DescriptorTooLong, Phase::QualifyingUsbTarget, false),
        (VersionWriteShort, Phase::QualifyingUsbTarget, false),
        (VersionReadError, Phase::QualifyingUsbTarget, false),
        (StalledVersionClock, Phase::QualifyingUsbTarget, false),
        (VersionFloatId, Phase::QualifyingUsbTarget, false),
        (VersionStringId, Phase::QualifyingUsbTarget, false),
        (VersionErrorKey, Phase::QualifyingUsbTarget, false),
        (VersionMalformed, Phase::QualifyingUsbTarget, false),
        (VersionBadFrame, Phase::QualifyingUsbTarget, false),
        (VersionNonObject, Phase::QualifyingUsbTarget, false),
        (TooManyReports, Phase::QualifyingUsbTarget, false),
        (StartVersion("v0.6.3"), Phase::QualifyingUsbTarget, false),
        (StartVersion("0.1.50"), Phase::QualifyingUsbTarget, false),
        (StartVersion("0.6.4"), Phase::QualifyingUsbTarget, false),
        (LocationChanged, Phase::QualifyingUsbTarget, false),
        (
            TamperToolAfter(Op::Version),
            Phase::QualifyingUsbTarget,
            false,
        ),
        (
            TamperImageAfter(Op::Version),
            Phase::QualifyingUsbTarget,
            false,
        ),
        (
            TamperToolDirectoryAfter(Op::Version),
            Phase::QualifyingUsbTarget,
            false,
        ),
        (BootWriteShort, Phase::Bootloader, true),
        (BootWriteError, Phase::Bootloader, true),
        (BootCloseError, Phase::Bootloader, true),
        (WrongBootPid, Phase::Bootloader, true),
        (WrongBootVid, Phase::Bootloader, true),
        (WrongBootLocation, Phase::Bootloader, true),
        (WrongBootPort, Phase::Bootloader, true),
        (DuplicateBootPort, Phase::Bootloader, true),
        (StalledClock, Phase::Bootloader, true),
        (BindingLostBefore(Op::Security), Phase::Security, true),
        (ToolExit(Op::Security), Phase::Security, true),
        (ToolTimeout(Op::Security), Phase::Security, true),
        (ToolOverflow(Op::Security), Phase::Security, true),
        (SecurityMissing, Phase::Security, true),
        (SecurityContradictory, Phase::Security, true),
        (ToolExit(Op::FlashId), Phase::FlashSize, true),
        (FlashSizeMissing, Phase::FlashSize, true),
        (FlashSizeWrong, Phase::FlashSize, true),
        (FlashSizeDuplicate, Phase::FlashSize, true),
        (ToolExit(Op::ReadBackup), Phase::ReadingBackup, true),
        (BackupMissing, Phase::ReadingBackup, true),
        (BackupShort, Phase::ReadingBackup, true),
        (PartitionMismatch, Phase::ReadingBackup, true),
        (ChecksumConflict, Phase::PersistingBackup, true),
        (ToolExit(Op::VerifyBackup), Phase::VerifyingBackup, true),
        (BackupChangedAfterVerify, Phase::VerifyingBackup, true),
        (ImageChangedAfterVerify, Phase::VerifyingBackup, true),
        (
            TamperToolAfter(Op::VerifyBackup),
            Phase::WritingFirmware,
            true,
        ),
        (
            TamperToolDirectoryAfter(Op::VerifyBackup),
            Phase::WritingFirmware,
            true,
        ),
        (
            BindingLostBefore(Op::WriteImage),
            Phase::WritingFirmware,
            true,
        ),
        (BackupChangedBeforeWrite, Phase::WritingFirmware, true),
        (ChecksumChangedBeforeWrite, Phase::WritingFirmware, true),
    ];
    #[cfg(unix)]
    let cases = {
        let mut cases = cases;
        cases.push((BackupPermissions, Phase::ReadingBackup, true));
        cases.push((BackupSymlink, Phase::ReadingBackup, true));
        cases.push((ExistingBackupSymlink, Phase::PreparingPrivateBackup, false));
        cases.push((StagedToolSymlink, Phase::QualifyingUsbTarget, false));
        cases
    };

    for (fault, phase, boot_attempted) in cases {
        let run = run(fault.clone(), false);
        failed(&run, phase);
        assert_eq!(
            run.outcome.report.bootloader_entry_attempted, boot_attempted,
            "{fault:?}"
        );
        assert!(!run.outcome.report.flash_write_attempted, "{fault:?}");
        assert!(!run.outcome.report.firmware_verified, "{fault:?}");
        assert!(!run.outcome.report.settings_preserved, "{fault:?}");
        assert!(
            !run.backend.operations().contains(&Op::WriteImage),
            "{fault:?}"
        );
        assert!(!run.backend.operations().contains(&Op::Reset), "{fault:?}");
        let boots = run
            .backend
            .hid_reports
            .iter()
            .filter(|packet| packet[3..].starts_with(br#"{"method":"sys.bootloader""#))
            .count();
        assert_eq!(boots, usize::from(boot_attempted), "{fault:?}");
        if fault == ExistingBackup {
            assert_eq!(
                fs::read(run.options.backup_dir.join("existing-evidence")).unwrap(),
                b"do not replace"
            );
        }
        #[cfg(unix)]
        if fault == ExistingBackupSymlink {
            assert_eq!(
                fs::read(
                    run.fixture
                        .root
                        .join("protected-existing/existing-evidence")
                )
                .unwrap(),
                b"do not replace"
            );
        }
    }
}

#[test]
fn each_hid_identity_dimension_is_checked_before_bootloader_entry() {
    let candidates = [
        CandidateMutation::Vendor,
        CandidateMutation::Product,
        CandidateMutation::UsagePage,
        CandidateMutation::Usage,
        CandidateMutation::Transport,
        CandidateMutation::Interface,
    ];
    let opened = [
        OpenedMutation::Vendor,
        OpenedMutation::Product,
        OpenedMutation::Interface,
        OpenedMutation::Transport,
        OpenedMutation::Path,
    ];
    for fault in candidates
        .into_iter()
        .map(Fault::Candidate)
        .chain(opened.into_iter().map(Fault::Opened))
    {
        let run = run(fault.clone(), false);
        failed(&run, UpdatePhase::QualifyingUsbTarget);
        assert!(!run.outcome.report.bootloader_entry_attempted, "{fault:?}");
        assert!(!run.outcome.report.flash_write_attempted, "{fault:?}");
        assert_eq!(run.backend.operations(), [ToolOperation::Version]);
    }
}

#[test]
fn hid_location_swaps_cannot_authorize_bootloader_or_final_version() {
    for final_phase in [false, true] {
        for location in [None, Some(0), Some(124)] {
            for fault in [
                Fault::CandidateLocation(location, final_phase),
                Fault::OpenedLocation(location, final_phase),
            ] {
                let run = run(fault.clone(), false);
                failed(
                    &run,
                    if final_phase {
                        UpdatePhase::VerifyingRunningFirmware
                    } else {
                        UpdatePhase::QualifyingUsbTarget
                    },
                );
                assert_eq!(
                    run.outcome.report.bootloader_entry_attempted, final_phase,
                    "{fault:?}"
                );
                assert_eq!(run.outcome.report.running_version, None, "{fault:?}");
                let version_queries = run
                    .backend
                    .hid_reports
                    .iter()
                    .filter(|packet| packet[3..].starts_with(br#"{"method":"sys.version""#))
                    .count();
                assert_eq!(version_queries, usize::from(final_phase), "{fault:?}");
            }
        }
    }
}

#[test]
fn failed_recovery_flush_never_authorizes_a_write() {
    for step in 1..=4 {
        let run = run(Fault::SyncFailure(step), false);
        failed(&run, UpdatePhase::PersistingBackup);
        assert_eq!(run.backend.sync_count, step);
        assert!(!run.outcome.report.backup_verified);
        assert!(!run.outcome.report.flash_write_attempted);
        assert!(
            !run.backend
                .operations()
                .contains(&ToolOperation::VerifyBackup)
        );
        assert!(
            !run.backend
                .operations()
                .contains(&ToolOperation::WriteImage)
        );
    }
}

#[test]
fn success_requires_every_gate_and_exact_tool_contract() {
    let run = run(Fault::None, true);
    let report = &run.outcome.report;
    assert!(run.outcome.update_succeeded, "{:?}", run.outcome.error);
    assert_eq!(report.phase, UpdatePhase::Complete);
    assert_eq!(report.failure_phase, None);
    assert!(report.bootloader_entry_attempted);
    assert!(report.backup_verified);
    assert!(report.flash_write_attempted);
    assert!(report.firmware_verified);
    assert!(report.settings_preserved);
    assert_eq!(report.running_version.as_deref(), Some("0.6.3"));
    assert!(!run.outcome.command_ready && !run.outcome.hardware_qualified);
    assert!(run.outcome.validate_supervisor().is_ok());
    assert_eq!(run.backend.operations(), ORDER);
    assert_eq!(run.backend.open_count, 2);
    assert_eq!(run.backend.close_count, 2);
    assert_eq!(run.backend.sync_count, 4);

    let workspace = fs::canonicalize(&run.options.backup_dir).unwrap();
    for (index, call) in run.backend.calls.iter().enumerate() {
        assert_eq!(call.operation, ORDER[index]);
        assert_eq!(call.program, workspace.join("esptool"));
        assert!(call.clear_environment && call.null_stdin);
        assert_eq!(call.stdout_limit, 4_194_304);
        assert_eq!(call.stderr_limit, 4_194_304);
        assert_eq!(
            call.environment,
            vec![
                (
                    OsString::from("PATH"),
                    OsString::from("/usr/bin:/bin:/usr/sbin:/sbin")
                ),
                (
                    OsString::from("TMPDIR"),
                    workspace.join("tool-tmp").into_os_string()
                ),
                (OsString::from("LC_ALL"), OsString::from("C")),
                (OsString::from("LANG"), OsString::from("C")),
                (OsString::from("TERM"), OsString::from("dumb")),
                (OsString::from("NO_COLOR"), OsString::from("1")),
            ]
        );
        let mut expected = Vec::<OsString>::new();
        if call.operation != ToolOperation::Version {
            expected.extend(
                [
                    "--chip",
                    "esp32s3",
                    "--port",
                    "/dev/cu.usbmodemTest123",
                    "--before",
                    "no-reset",
                    "--after",
                    if call.operation == ToolOperation::Reset {
                        "watchdog-reset"
                    } else {
                        "no-reset"
                    },
                    "--connect-attempts",
                    "1",
                ]
                .into_iter()
                .map(OsString::from),
            );
        }
        let (suffix, path, seconds) = match call.operation {
            ToolOperation::Version => (vec!["version"], None, 10),
            ToolOperation::Security => (vec!["get-security-info"], None, 60),
            ToolOperation::FlashId => (vec!["flash-id"], None, 60),
            ToolOperation::ReadBackup => (
                vec!["read-flash", "0", "0x1000000"],
                Some("full-flash.bin"),
                240,
            ),
            ToolOperation::VerifyBackup => (vec!["verify-flash", "0"], Some("full-flash.bin"), 240),
            ToolOperation::WriteImage => (
                vec![
                    "write-flash",
                    "--flash-mode",
                    "keep",
                    "--flash-freq",
                    "keep",
                    "--flash-size",
                    "keep",
                    "0",
                ],
                Some("firmware.bin"),
                180,
            ),
            ToolOperation::VerifyImage => (vec!["verify-flash", "0"], Some("firmware.bin"), 120),
            ToolOperation::ReadSettings => (
                vec!["read-flash", "0x810000", "0x220000"],
                Some("settings-after.bin"),
                120,
            ),
            ToolOperation::Reset => (vec!["read-mac"], None, 60),
        };
        expected.extend(suffix.into_iter().map(OsString::from));
        if let Some(path) = path {
            expected.push(workspace.join(path).into_os_string());
        }
        assert_eq!(call.arguments, expected);
        assert_eq!(call.timeout, Duration::from_secs(seconds));
        assert!(workspace.join(call.operation.log_name()).is_file());
    }
    let version_log = fs::read(workspace.join("Version.log")).unwrap();
    assert_eq!(version_log, b"esptool v5.6.0\nsynthetic stderr\n");

    let initial = run.backend.hid_reports[0];
    assert_eq!(&initial[..3], &[0x06, 0x02, 0x2f]);
    assert_eq!(
        &initial[3..50],
        br#"{"method":"sys.version","params":null,"id":101}"#
    );
    assert!(initial[50..].iter().all(|byte| *byte == 0));
    let boot = run.backend.hid_reports[1];
    let body = br#"{"method":"sys.bootloader","params":null,"id":102}"#;
    assert_eq!(&boot[..3], &[6, 2, body.len() as u8]);
    assert_eq!(&boot[3..3 + body.len()], body);
    assert!(boot[3 + body.len()..].iter().all(|byte| *byte == 0));
    assert_eq!(run.backend.hid_reports.len(), 3);

    let backup = fs::read(workspace.join("full-flash.bin")).unwrap();
    assert_eq!(backup.len(), 0x1000000);
    assert_eq!(
        fs::read_to_string(workspace.join("SHA256SUMS")).unwrap(),
        format!("{}  full-flash.bin\n", sha256(&backup))
    );
    assert_eq!(
        fs::read(workspace.join("settings-after.bin")).unwrap(),
        backup[SETTINGS_START..SETTINGS_END]
    );
    assert!(!workspace.join("firmware.bin").exists());
    assert!(!workspace.join("esptool").exists());
    assert!(!workspace.join("tool-tmp").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&workspace).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for name in [
            "full-flash.bin",
            "SHA256SUMS",
            "settings-after.bin",
            "Version.log",
        ] {
            assert_eq!(
                fs::metadata(workspace.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    let already_current = super::common::run(Fault::StartVersion("0.6.3"), false);
    assert!(already_current.outcome.update_succeeded);
}

#[test]
fn failures_after_write_preserve_flags_and_never_rollback_or_retry() {
    use Fault::*;
    use ToolOperation as Op;
    use UpdatePhase as Phase;
    let cases = [
        (
            ToolExit(Op::WriteImage),
            Phase::WritingFirmware,
            false,
            false,
            false,
        ),
        (
            ToolTimeout(Op::WriteImage),
            Phase::WritingFirmware,
            false,
            false,
            false,
        ),
        (
            ToolOverflow(Op::WriteImage),
            Phase::WritingFirmware,
            false,
            false,
            false,
        ),
        (
            ToolStderrOverflow(Op::WriteImage),
            Phase::WritingFirmware,
            false,
            false,
            false,
        ),
        (
            ToolCancelled(Op::WriteImage),
            Phase::WritingFirmware,
            false,
            false,
            false,
        ),
        (
            BindingLostBefore(Op::VerifyImage),
            Phase::VerifyingFirmware,
            false,
            false,
            false,
        ),
        (
            ToolExit(Op::VerifyImage),
            Phase::VerifyingFirmware,
            false,
            false,
            false,
        ),
        (
            ToolCancelled(Op::VerifyImage),
            Phase::VerifyingFirmware,
            false,
            false,
            false,
        ),
        (
            TamperImageAfter(Op::WriteImage),
            Phase::VerifyingFirmware,
            false,
            false,
            false,
        ),
        (
            ToolExit(Op::ReadSettings),
            Phase::VerifyingSettings,
            true,
            false,
            false,
        ),
        (
            ToolTimeout(Op::ReadSettings),
            Phase::VerifyingSettings,
            true,
            false,
            false,
        ),
        (
            SettingsMissing,
            Phase::VerifyingSettings,
            true,
            false,
            false,
        ),
        (SettingsShort, Phase::VerifyingSettings, true, false, false),
        (
            SettingsMismatch,
            Phase::VerifyingSettings,
            true,
            false,
            false,
        ),
        (ToolExit(Op::Reset), Phase::Resetting, true, true, true),
        (ToolTimeout(Op::Reset), Phase::Resetting, true, true, true),
        (
            BackupChangedBeforeReset,
            Phase::Resetting,
            true,
            true,
            false,
        ),
        (
            FinalAbsent,
            Phase::VerifyingRunningFirmware,
            true,
            true,
            true,
        ),
        (
            FinalLocation,
            Phase::VerifyingRunningFirmware,
            true,
            true,
            true,
        ),
        (
            FinalLocationAfterQuery,
            Phase::VerifyingRunningFirmware,
            true,
            true,
            true,
        ),
        (
            FinalDuplicate,
            Phase::VerifyingRunningFirmware,
            true,
            true,
            true,
        ),
        (FinalOpen, Phase::VerifyingRunningFirmware, true, true, true),
        (
            FinalDescriptor,
            Phase::VerifyingRunningFirmware,
            true,
            true,
            true,
        ),
        (
            FinalReadError,
            Phase::VerifyingRunningFirmware,
            true,
            true,
            true,
        ),
        (
            FinalVersion("v0.6.3"),
            Phase::VerifyingRunningFirmware,
            true,
            true,
            true,
        ),
        (
            BackupChangedAfterReset,
            Phase::VerifyingRunningFirmware,
            true,
            true,
            true,
        ),
        (
            FinalClose,
            Phase::VerifyingRunningFirmware,
            true,
            true,
            true,
        ),
        (
            CancelAfterFinalClose,
            Phase::VerifyingRunningFirmware,
            true,
            true,
            true,
        ),
    ];
    for (fault, phase, firmware_verified, settings_preserved, reset_called) in cases {
        let run = run(fault.clone(), false);
        failed(&run, phase);
        let report = &run.outcome.report;
        assert!(report.backup_verified, "{fault:?}");
        assert!(report.flash_write_attempted, "{fault:?}");
        assert_eq!(report.firmware_verified, firmware_verified, "{fault:?}");
        assert_eq!(report.settings_preserved, settings_preserved, "{fault:?}");
        let operations = run.backend.operations();
        assert_eq!(
            operations
                .iter()
                .filter(|op| **op == Op::WriteImage)
                .count(),
            1
        );
        assert_eq!(operations.contains(&Op::Reset), reset_called, "{fault:?}");
        assert!(operations.len() <= ORDER.len());
        let recovery = &run.options.backup_dir;
        assert!(recovery.join("full-flash.bin").is_file(), "{fault:?}");
        assert!(recovery.join("SHA256SUMS").is_file(), "{fault:?}");
        assert!(!recovery.join("esptool").exists(), "{fault:?}");
        assert!(!recovery.join("firmware.bin").exists(), "{fault:?}");
        if fault == SettingsMismatch {
            assert!(recovery.join("settings-after.bin").is_file());
            assert!(!operations.contains(&Op::Reset));
        }
        if matches!(
            fault,
            ToolExit(Op::ReadSettings) | ToolTimeout(Op::ReadSettings)
        ) {
            assert_eq!(
                fs::read(recovery.join("settings-after.bin")).unwrap(),
                b"partial synthetic settings evidence"
            );
        }
        if fault == FinalVersion("v0.6.3") {
            assert_eq!(report.running_version.as_deref(), Some("v0.6.3"));
        }
        if matches!(
            fault,
            FinalLocationAfterQuery | BackupChangedAfterReset | CancelAfterFinalClose
        ) {
            assert_eq!(report.running_version.as_deref(), Some("0.6.3"));
        }
        if matches!(fault, FinalOpen | FinalDescriptor | FinalReadError) {
            assert_eq!(
                run.backend.open_count, 2,
                "final opening/query must not retry"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn settings_symlink_is_retained_and_never_used_for_reset() {
    let run = run(Fault::SettingsSymlink, false);
    failed(&run, UpdatePhase::VerifyingSettings);
    assert!(run.outcome.report.flash_write_attempted);
    assert!(run.outcome.report.firmware_verified);
    assert!(!run.outcome.report.settings_preserved);
    assert!(
        fs::symlink_metadata(run.options.backup_dir.join("settings-after.bin"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!run.backend.operations().contains(&ToolOperation::Reset));
}

#[test]
fn tool_and_port_are_revalidated_before_each_operation() {
    for (index, previous) in ORDER[..ORDER.len() - 1].iter().copied().enumerate() {
        let run = run(Fault::TamperToolAfter(previous), false);
        assert!(!run.outcome.update_succeeded);
        assert_eq!(run.backend.operations(), ORDER[..=index]);
        assert!(!run.backend.operations().contains(&ToolOperation::Reset));
        if previous == ToolOperation::Version {
            assert!(!run.outcome.report.bootloader_entry_attempted);
        }
        assert!(run.outcome.validate_supervisor().is_ok());
    }
    for operation in ORDER[1..].iter().copied() {
        let run = run(Fault::BindingLostBefore(operation), false);
        assert!(!run.outcome.update_succeeded);
        assert!(!run.backend.operations().contains(&operation));
        assert!(!run.backend.operations().contains(&ToolOperation::Reset));
        assert!(run.outcome.validate_supervisor().is_ok());
        if operation == ToolOperation::WriteImage {
            assert!(run.outcome.report.backup_verified);
            assert!(!run.outcome.report.flash_write_attempted);
        }
    }
}

#[test]
fn partial_and_durable_recovery_evidence_survives_failure_until_fixture_cleanup() {
    let staging = run(Fault::ToolTempEvidence, false);
    failed(&staging, UpdatePhase::ToolVersion);
    assert_eq!(
        fs::read(
            staging
                .options
                .backup_dir
                .join("tool-tmp/synthetic-evidence")
        )
        .unwrap(),
        b"retain nonempty staging evidence"
    );
    assert!(staging.options.backup_dir.join("Version.log").is_file());
    assert!(!staging.options.backup_dir.join("firmware.bin").exists());
    assert!(!staging.options.backup_dir.join("esptool").exists());

    let partial = run(Fault::ToolExit(ToolOperation::ReadBackup), false);
    failed(&partial, UpdatePhase::ReadingBackup);
    assert_eq!(
        fs::read(partial.options.backup_dir.join("full-flash.bin")).unwrap(),
        b"partial synthetic recovery evidence"
    );
    assert_eq!(
        fs::read(partial.options.backup_dir.join("ReadBackup.log")).unwrap(),
        b"synthetic stdout\nsynthetic stderr\n"
    );
    assert!(!partial.options.backup_dir.join("SHA256SUMS").exists());

    let durable = run(Fault::ToolExit(ToolOperation::WriteImage), false);
    failed(&durable, UpdatePhase::WritingFirmware);
    let backup = fs::read(durable.options.backup_dir.join("full-flash.bin")).unwrap();
    let checksum = fs::read_to_string(durable.options.backup_dir.join("SHA256SUMS")).unwrap();
    assert_eq!(checksum, format!("{}  full-flash.bin\n", sha256(&backup)));
    assert!(
        durable
            .options
            .backup_dir
            .join("VerifyBackup.log")
            .is_file()
    );
    assert!(durable.options.backup_dir.join("WriteImage.log").is_file());
    assert!(!durable.backend.operations().contains(&ToolOperation::Reset));
}

#[test]
fn exact_security_and_size_parsing_rejects_conflicting_evidence() {
    let secure = "  Secure Boot: Disabled\nFlash Encryption: Disabled  \nFlags: 0x00000000 (0b0)\n";
    assert!(check_security(secure).is_ok());
    for bad in [
        "Secure Boot: Disabled\nFlash Encryption: Disabled\n",
        "Secure Boot: Disabled\nFlash Encryption: Disabled\nFlags: 0x0 (0b0)\n",
        "Secure Boot: Disabled\nFlash Encryption: Disabled\nFlags: 0x00000000 (0b0)\nSecure Boot: Enabled",
        "Secure Boot: Disabled\nFlash Encryption: Disabled\nFlags: 0x00000000 (0b0)\nFlash Encryption: Enabled",
    ] {
        assert!(check_security(bad).is_err());
    }
    assert!(check_flash_size("banner\n Detected flash size: 16MB \n").is_ok());
    for bad in [
        "",
        "Detected flash size: 8MB",
        "Detected flash size: 16MB\nDetected flash size: 16MB",
        "Detected flash size: 16MB\nDetected flash size: 8MB",
    ] {
        assert!(check_flash_size(bad).is_err());
    }
}
