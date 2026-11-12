use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use serde_json::json;

use super::super::UpdatePhase;
use super::super::artifacts::{
    ArtifactPin, IMAGE_PIN, IMAGE_SIZE, SETTINGS_START, TOOL_PIN, TOOL_SIZE, sha256,
    verify_artifact, verify_image_extent,
};
use super::super::options::{CONFIRMATION, OptionError, validate_options};
use super::super::plan::{micro_capability, offline_plan, synthetic_plan, unavailable_capability};
use super::super::transaction::{run_maintenance_worker, run_transaction_model};
use super::common::{FakeBackend, Fault, Fixture, image_bytes, write_private};

#[test]
fn options_validate_in_order_without_backend_access() {
    let fixture = Fixture::new();
    let valid = fixture.options();
    assert_eq!(validate_options(&valid), Ok(()));

    let mut invalid = valid.clone();
    invalid.confirm = format!(" {CONFIRMATION}");
    invalid.image = PathBuf::from("relative-image");
    invalid.esptool = PathBuf::from("relative-tool");
    invalid.backup_dir = PathBuf::from("relative-backup");
    invalid.port = OsString::from("invalid");
    for (expected, repair) in [
        (OptionError::Confirmation, 0),
        (OptionError::ImageNotAbsolute, 1),
        (OptionError::ToolNotAbsolute, 2),
        (OptionError::BackupNotAbsolute, 3),
        (OptionError::InvalidPort, 4),
    ] {
        assert_eq!(validate_options(&invalid), Err(expected));
        let mut backend = FakeBackend::new(Fault::None, vec![0x61; 36]);
        let outcome = run_transaction_model(&invalid, &mut backend);
        assert_eq!(
            outcome.report.failure_phase,
            Some(UpdatePhase::ValidatingOptions)
        );
        assert!(!outcome.update_succeeded);
        assert!(backend.events.is_empty());
        assert!(backend.calls.is_empty());
        assert!(backend.hid_reports.is_empty());
        let worker = run_maintenance_worker(&invalid);
        assert_eq!(
            worker.report.failure_phase,
            Some(UpdatePhase::ValidatingOptions)
        );
        match repair {
            0 => invalid.confirm = CONFIRMATION.to_owned(),
            1 => invalid.image = valid.image.clone(),
            2 => invalid.esptool = valid.esptool.clone(),
            3 => invalid.backup_dir = valid.backup_dir.clone(),
            _ => invalid.port = valid.port.clone(),
        }
    }
    assert_eq!(validate_options(&invalid), Ok(()));

    for port in [
        "/dev/cu.usbmodem",
        "/dev/cu.usbmodemABC-1",
        "/dev/cu.usbmodemA/B",
        "/dev/cu.usbmodemA ",
        "/dev/cu.usbmodemå",
        "/dev/cu.usbmodem*",
        "/dev/cu.usbModemA",
        " /dev/cu.usbmodemA",
    ] {
        let mut options = valid.clone();
        options.port = port.into();
        assert_eq!(
            validate_options(&options),
            Err(OptionError::InvalidPort),
            "{port}"
        );
    }
    for confirmation in [
        "INSTALL MICRO 0.6.3\n",
        "install micro 0.6.3",
        "INSTALL MICRO 0.6.3 ",
    ] {
        let mut options = valid.clone();
        options.confirm = confirmation.to_owned();
        assert_eq!(validate_options(&options), Err(OptionError::Confirmation));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut options = valid;
        let mut port = b"/dev/cu.usbmodem".to_vec();
        port.push(0xff);
        options.port = OsString::from_vec(port);
        assert_eq!(validate_options(&options), Err(OptionError::InvalidPort));
    }
}

#[test]
fn pinned_local_files_and_offline_schema_are_exact() {
    let fixture = Fixture::new();
    let options = fixture.options();
    let image = image_bytes(IMAGE_SIZE as usize);
    let tool = vec![0x47; TOOL_SIZE as usize];
    let image_digest = sha256(&image);
    let tool_digest = sha256(&tool);
    let image_pin = ArtifactPin {
        size: image.len() as u64,
        sha256: &image_digest,
    };
    let tool_pin = ArtifactPin {
        size: tool.len() as u64,
        sha256: &tool_digest,
    };
    write_private(&options.image, &image);
    write_private(&options.esptool, &tool);

    let plan = synthetic_plan(&options.image, &options.esptool, image_pin, tool_pin).unwrap();
    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        json!({
            "read_only": true,
            "device_access": false,
            "command_ready": false,
            "firmware_update": {
                "path": "official_esp32_usb",
                "driver_available": false,
                "platform": "macos_aarch64",
                "transport": "usb",
                "supported_version": "0.6.3",
                "evidence": "manual_reference_verified_automated_acceptance_pending"
            },
            "artifact_verified": true,
            "tool_verified": true,
            "confirmation": "INSTALL MICRO 0.6.3",
            "release": "specs/MICRO_AND_FIRMWARE.md#103-local-artifact-input-requirements",
            "steps": [
                "Verify local artifacts and qualified USB identity.",
                "Enter bootloader once; bind explicit serial port to same location.",
                "Check ESP32-S3, 16MB, disabled security features.",
                "Read/verify full backup; compare partition sector.",
                "Write only reviewed merged image at zero.",
                "Verify image and unchanged settings.",
                "Watchdog reset; verify same-location USB firmware 0.6.3."
            ]
        })
    );
    assert!(!micro_capability().driver_available);
    assert_eq!(
        serde_json::to_value(unavailable_capability()).unwrap(),
        json!({
            "path": "unavailable",
            "driver_available": false,
            "platform": null,
            "transport": null,
            "supported_version": null,
            "evidence": "no_verified_update_path"
        })
    );

    // Both files have the production lengths but only self-authored bytes.
    assert!(verify_artifact(&options.image, IMAGE_PIN).is_err());
    assert!(verify_artifact(&options.esptool, TOOL_PIN).is_err());
    let error = offline_plan(&options.image, &options.esptool).unwrap_err();
    if cfg!(feature = "micro-firmware") {
        assert!(error.to_string().contains("pinned SHA-256"));
    } else {
        assert!(error.to_string().contains("micro-firmware"));
    }
    assert!(!options.backup_dir.exists());
}

#[test]
fn rejects_size_hash_header_extent_and_path_substitutions() {
    let fixture = Fixture::new();
    let original = image_bytes(0x9001);
    let digest = sha256(&original);
    let pin = ArtifactPin {
        size: original.len() as u64,
        sha256: &digest,
    };
    let good = fixture.root.join("good");
    write_private(&good, &original);
    assert_eq!(verify_artifact(&good, pin).unwrap(), original);

    for (name, bytes) in [
        ("short", original[..original.len() - 1].to_vec()),
        ("grown", [original.as_slice(), &[0x31]].concat()),
        ("changed", {
            let mut changed = original.clone();
            changed[100] ^= 1;
            changed
        }),
    ] {
        let path = fixture.root.join(name);
        write_private(&path, &bytes);
        assert!(verify_artifact(&path, pin).is_err(), "{name}");
    }
    assert!(verify_artifact(&fixture.root, pin).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link = fixture.root.join("synthetic-link");
        symlink(&good, &link).unwrap();
        assert!(verify_artifact(&link, pin).is_err());
    }
    let mut header = original.clone();
    header[0] = 0;
    assert!(verify_image_extent(&header).is_err());
    let exactly_before_settings = image_bytes(SETTINGS_START);
    assert!(verify_image_extent(&exactly_before_settings).is_ok());
    let beyond = image_bytes(SETTINGS_START + 1);
    assert!(verify_image_extent(&beyond).is_err());
    assert_eq!((IMAGE_SIZE as usize + 4095) & !4095, 0x21d000);
}

#[test]
fn unavailable_worker_fails_after_validation_without_creating_a_workspace() {
    let fixture = Fixture::new();
    let options = fixture.options();
    let outcome = run_maintenance_worker(&options);
    assert!(!outcome.update_succeeded);
    assert_eq!(outcome.report.failure_phase, Some(UpdatePhase::Preflight));
    assert!(!outcome.report.bootloader_entry_attempted);
    assert!(!options.backup_dir.exists());
    assert!(outcome.validate_supervisor().is_ok());

    let missing_image = fixture.root.join("does-not-exist");
    let missing_tool = fixture.root.join("also-missing");
    if !cfg!(feature = "micro-firmware") {
        let error = offline_plan(&missing_image, &missing_tool).unwrap_err();
        assert!(error.to_string().contains("micro-firmware"));
    }
    assert!(!fs::exists(missing_image).unwrap());
}
