use serde_json::json;
use uuid::Uuid;

use crate::domain::{
    ClockSample, ClockSource, MonotonicStamp, NativeProcessIdentity, RuntimeEpoch,
};

use super::super::{
    contracts::{Observation, PlatformError, PlatformErrorKind},
    inventory::{DeviceClass, Support, macos_hid_discovery, portable_inventory},
    observation::{
        ComPort, DesktopContext, DiagnosticReport, DiagnosticSections, ExecutableClass,
        ForegroundEvidence, ProcessCandidate, ProcessEvidence, ProcessInventory, SerialDiscovery,
        SerialSelection, portable_diagnostic_report,
    },
};

fn sample() -> ClockSample {
    ClockSample {
        source: ClockSource::LinuxMonotonic,
        stamp: MonotonicStamp {
            epoch: RuntimeEpoch::from_uuid(Uuid::from_u128(1)),
            ticks: 10,
            frequency: 10,
        },
    }
}

fn process() -> ProcessEvidence {
    ProcessEvidence {
        identity: NativeProcessIdentity {
            pid: 123,
            creation_time_100ns: 456,
            session_id: 3,
        },
        executable_class: ExecutableClass::RecognizedNativeTerminalHost,
        observed_at: sample(),
    }
}

#[test]
fn compile_time_inventory_never_claims_unintegrated_or_qualified_features() {
    let inventory = portable_inventory();
    assert!(inventory.read_only());
    assert!(!inventory.command_ready());
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert!(matches!(
            inventory.platform().safe_private_storage,
            Support::Compiled { .. }
        ));
    } else {
        assert!(matches!(
            inventory.platform().safe_private_storage,
            Support::Unavailable { .. }
        ));
    }

    if cfg!(any(windows, target_os = "linux", target_os = "macos")) {
        assert!(matches!(
            inventory.platform().cooperative_runtime_ownership,
            Support::Compiled { .. }
        ));
    } else {
        assert!(matches!(
            inventory.platform().cooperative_runtime_ownership,
            Support::Unavailable { .. }
        ));
    }
    assert!(matches!(
        inventory.platform().lifecycle_notifications,
        Support::Unavailable { .. }
    ));
    if cfg!(windows) {
        assert!(matches!(
            inventory
                .platform()
                .process_window_desktop_serial_observations,
            Support::Compiled { .. }
        ));
    } else {
        assert!(matches!(
            inventory
                .platform()
                .process_window_desktop_serial_observations,
            Support::Unavailable { .. }
        ));
    }
    assert!(matches!(inventory.actions(), Support::Unavailable { .. }));
    assert_eq!(inventory.devices().len(), 5);
    assert!(
        inventory
            .devices()
            .iter()
            .all(|device| !device.runtime_qualified())
    );
    let value = serde_json::to_value(inventory).unwrap();
    assert_eq!(value["schema"], 1);
    assert_eq!(value["read_only"], true);
    assert_eq!(value["command_ready"], false);
    for row in value["devices"].as_array().unwrap() {
        assert_eq!(row["runtime_qualified"], false);
    }
}

#[test]
fn macos_inventory_metadata_depends_on_the_matching_family_feature() {
    for (micro, akp03) in [(false, false), (true, false), (false, true), (true, true)] {
        for (class, enabled) in [
            (DeviceClass::PrimaryUsbController, micro),
            (DeviceClass::RelatedWirelessController, micro),
            (DeviceClass::AlternateSecondRevisionProfile, akp03),
        ] {
            let support = macos_hid_discovery(class, micro, akp03);
            assert_eq!(
                support,
                Support::Unavailable {
                    reason: if enabled {
                        "metadata_only_inside_explicit_hid_probe"
                    } else {
                        "feature_disabled"
                    },
                }
            );
        }
        for class in [
            DeviceClass::AlternateOriginalProfile,
            DeviceClass::SerialControlSurface,
        ] {
            assert_eq!(
                macos_hid_discovery(class, micro, akp03),
                Support::Unavailable {
                    reason: "device_backend_not_integrated"
                },
            );
        }
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    for device in portable_inventory().devices() {
        assert_eq!(
            device.discovery,
            macos_hid_discovery(
                device.class,
                cfg!(feature = "micro-hid"),
                cfg!(feature = "akp03-hid")
            )
        );
    }
}

#[test]
fn serial_control_surface_inventory_stays_unintegrated_for_discovery_and_transport() {
    let inventory = portable_inventory();
    let serial = inventory
        .devices()
        .iter()
        .find(|device| device.class == DeviceClass::SerialControlSurface)
        .unwrap();
    assert_eq!(
        serial.discovery,
        Support::Unavailable {
            reason: "device_backend_not_integrated"
        }
    );
    assert_eq!(
        serial.diagnostic_transport,
        Support::Unavailable {
            reason: "device_backend_not_integrated"
        }
    );
}

#[test]
fn positive_evidence_cannot_promote_diagnostic_flags() {
    let evidence = process();
    let inventory = ProcessInventory::new(
        1,
        vec![ProcessCandidate {
            snapshot_pid: 123,
            snapshot_class: ExecutableClass::RecognizedNativeTerminalHost,
            lifetime: Observation::Observed(evidence.identity),
        }],
    )
    .unwrap();
    let port: ComPort = "com17".parse().unwrap();
    let serial = SerialDiscovery::from_validated(1, 1, vec![port], true).unwrap();
    let report = DiagnosticReport::from_sections(DiagnosticSections {
        clock: Observation::Observed(sample()),
        current_process: Observation::Observed(evidence.clone()),
        processes: Observation::Observed(inventory),
        foreground: Observation::Observed(ForegroundEvidence {
            window: 42,
            owner_thread_id: 50,
            owner: evidence,
        }),
        desktop: Observation::Observed(DesktopContext {
            session_id: 3,
            active_console_session_id: Some(3),
            active: true,
            local: true,
            interactive: true,
            default_desktop: true,
            observed_at: sample(),
        }),
        serial: Observation::Observed(serial),
    });
    assert!(report.read_only());
    assert!(!report.command_ready());
    assert!(!report.w2_qualified());
    let value = serde_json::to_value(report).unwrap();
    assert_eq!(value["read_only"], true);
    assert_eq!(value["command_ready"], false);
    assert_eq!(value["w2_qualified"], false);
    assert_eq!(value["serial"]["evidence"]["selected"], "COM17");
}

#[test]
fn unavailable_observations_are_not_empty_successes_and_errors_are_static() {
    let error = PlatformError::new(PlatformErrorKind::AccessDenied, "process_open", Some(5));
    assert_eq!(error.to_string(), "process_open: AccessDenied (0x5)");
    let unavailable: Observation<ProcessInventory> = Observation::Unavailable(error);
    assert_eq!(
        serde_json::to_value(unavailable).unwrap(),
        json!({
            "status": "unavailable",
            "evidence": {
                "kind": "access_denied",
                "operation": "process_open",
                "native_code": 5
            }
        })
    );
    let report = portable_diagnostic_report(RuntimeEpoch::from_uuid(Uuid::from_u128(1)));
    let value = serde_json::to_value(report).unwrap();
    #[cfg(not(windows))]
    for section in [
        "current_process",
        "processes",
        "foreground",
        "desktop",
        "serial",
    ] {
        assert_eq!(value[section]["status"], "unavailable");
        assert_eq!(value[section]["evidence"]["kind"], "unsupported_platform");
        assert_eq!(value[section]["evidence"]["operation"], "windows_platform");
    }
    assert_eq!(value["read_only"], true);
    assert_eq!(value["command_ready"], false);
    assert_eq!(value["w2_qualified"], false);
}

#[test]
fn serial_observations_retain_only_valid_normalized_ports() {
    for (input, expected) in [
        ("COM3", "COM3"),
        ("com17", "COM17"),
        ("COM65535", "COM65535"),
    ] {
        assert_eq!(input.parse::<ComPort>().unwrap().to_string(), expected);
    }
    for input in [
        "COM0",
        "COM03",
        "COM65536",
        "COM+3",
        "COM3extra",
        " COM3",
        "COM",
        "\\\\.\\COM3",
        "COM３",
    ] {
        assert!(input.parse::<ComPort>().is_err(), "{input}");
    }
    let one: ComPort = "COM3".parse().unwrap();
    let duplicate = SerialDiscovery::from_validated(2, 2, vec![one, one], true).unwrap();
    assert_eq!(duplicate.selection(), SerialSelection::Ambiguous);
    assert_eq!(duplicate.selected(), None);
    let incomplete = SerialDiscovery::from_validated(2, 2, vec![one], true).unwrap();
    assert_eq!(incomplete.selection(), SerialSelection::Incomplete);
    assert_eq!(incomplete.selected(), None);
    assert_eq!(
        SerialDiscovery::from_validated(0, 0, vec![], false)
            .unwrap()
            .selection(),
        SerialSelection::Incomplete
    );
    assert_eq!(
        SerialDiscovery::from_validated(0, 0, vec![], true)
            .unwrap()
            .selection(),
        SerialSelection::NotFound
    );
}
