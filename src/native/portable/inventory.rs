//! Compile-time-only inventory for the portable block as currently integrated.

use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Support {
    Compiled { prerequisite: &'static str },
    Unavailable { reason: &'static str },
}

const fn unavailable(reason: &'static str) -> Support {
    Support::Unavailable { reason }
}

pub(super) fn macos_hid_discovery(
    class: DeviceClass,
    micro_enabled: bool,
    akp03_enabled: bool,
) -> Support {
    let enabled = match class {
        DeviceClass::PrimaryUsbController | DeviceClass::RelatedWirelessController => micro_enabled,
        DeviceClass::AlternateSecondRevisionProfile => akp03_enabled,
        _ => return unavailable("device_backend_not_integrated"),
    };
    if enabled {
        unavailable("metadata_only_inside_explicit_hid_probe")
    } else {
        unavailable("feature_disabled")
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PlatformSupport {
    pub shared_native_clock: Support,
    pub lifecycle_notifications: Support,
    pub safe_private_storage: Support,
    pub cooperative_runtime_ownership: Support,
    pub bounded_diagnostic_children: Support,
    pub durable_action_writer_quiescence: Support,
    pub process_window_desktop_serial_observations: Support,
    pub explicit_qualification_probes: Support,
    pub framed_job_workers: Support,
    pub framed_process_group_workers: Support,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostSupport {
    pub discovery: Support,
    pub authentication: Support,
    pub association: Support,
    pub confirmed_presentation: Support,
    pub same_run_stop: Support,
}

#[derive(Clone, Debug, Serialize)]
pub struct Hosts {
    pub integrated: HostSupport,
    pub default_terminal: HostSupport,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClass {
    PrimaryUsbController,
    RelatedWirelessController,
    AlternateOriginalProfile,
    AlternateSecondRevisionProfile,
    SerialControlSurface,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Usb,
    Bluetooth,
    Serial,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceSupport {
    pub class: DeviceClass,
    pub transport: Transport,
    pub discovery: Support,
    pub diagnostic_transport: Support,
    runtime_qualified: bool,
}

impl DeviceSupport {
    pub fn runtime_qualified(&self) -> bool {
        self.runtime_qualified
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MaintenanceSupport {
    pub primary_usb_controller: Support,
    pub other_device_families: Support,
}

#[derive(Clone, Debug, Serialize)]
pub struct CapabilityInventory {
    schema: u8,
    evidence: &'static str,
    os: &'static str,
    architecture: &'static str,
    read_only: bool,
    command_ready: bool,
    platform: PlatformSupport,
    hosts: Hosts,
    devices: Vec<DeviceSupport>,
    maintenance: MaintenanceSupport,
    actions: Support,
}

impl CapabilityInventory {
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    pub fn command_ready(&self) -> bool {
        self.command_ready
    }

    pub fn platform(&self) -> &PlatformSupport {
        &self.platform
    }

    pub fn hosts(&self) -> &Hosts {
        &self.hosts
    }

    pub fn devices(&self) -> &[DeviceSupport] {
        &self.devices
    }

    pub fn maintenance(&self) -> &MaintenanceSupport {
        &self.maintenance
    }

    pub fn actions(&self) -> Support {
        self.actions
    }
}

/// This inventory makes no native probe and does not claim unintegrated blocks.
pub fn portable_inventory() -> CapabilityInventory {
    let supported_os = cfg!(any(windows, target_os = "linux", target_os = "macos"));
    let clock = if supported_os {
        Support::Compiled {
            prerequisite: "nonnil_runtime_epoch_same_native_source_same_boot",
        }
    } else {
        unavailable("unsupported_platform")
    };
    let children = if cfg!(windows) {
        Support::Compiled {
            prerequisite: "direct_child_without_job_containment",
        }
    } else if cfg!(any(target_os = "linux", target_os = "macos")) {
        Support::Compiled {
            prerequisite: "process_group_without_parent_death_or_escape_guarantee",
        }
    } else {
        unavailable("unsupported_platform")
    };
    let windows_observations = if cfg!(windows) {
        Support::Compiled {
            prerequisite: "explicit_read_only_api_supervised_helper_where_required",
        }
    } else {
        unavailable("unsupported_platform")
    };
    let windows_workers = if cfg!(windows) {
        Support::Compiled {
            prerequisite: "contained_same_executable_diagnostic_allowlist",
        }
    } else {
        unavailable("unsupported_platform")
    };
    let platform = PlatformSupport {
        shared_native_clock: clock,
        lifecycle_notifications: unavailable("native_lifecycle_subscriptions_unimplemented"),
        safe_private_storage: if cfg!(any(target_os = "linux", target_os = "macos")) {
            Support::Compiled {
                prerequisite: "explicit_existing_private_root_native_filesystem_policy",
            }
        } else {
            unavailable("namespace_mutation_durability_unsupported")
        },
        cooperative_runtime_ownership: if supported_os {
            Support::Compiled {
                prerequisite: "explicit_existing_empty_shared_authority_anchor_library_api_only",
            }
        } else {
            unavailable("unsupported_platform")
        },
        bounded_diagnostic_children: children,
        durable_action_writer_quiescence: unavailable("action_writer_quiescence_unimplemented"),
        process_window_desktop_serial_observations: windows_observations,
        explicit_qualification_probes: windows_observations,
        framed_job_workers: windows_workers,
        framed_process_group_workers: if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Support::Compiled {
                prerequisite: "same_executable_health_or_opt_in_hid_parent_watchdog_no_escape_guarantee",
            }
        } else {
            unavailable("unsupported_platform")
        },
    };
    let integrated = HostSupport {
        discovery: unavailable("versioned_host_contract_required"),
        authentication: unavailable("owned_relay_and_native_secret_store_required"),
        association: unavailable("host_recipient_metadata_required"),
        confirmed_presentation: unavailable("host_confirmation_required"),
        same_run_stop: unavailable(
            "exact_recipient_stop_only_grant_and_pre_gesture_expiry_required",
        ),
    };
    let default_terminal = HostSupport {
        discovery: if cfg!(windows) {
            unavailable("offline_cli_does_not_discover_hosts")
        } else {
            unavailable("reviewed_host_discovery_and_permissions_required")
        },
        authentication: unavailable("cooperating_host_not_authenticated"),
        association: unavailable("exact_interactive_recipient_unproven"),
        confirmed_presentation: unavailable("foreground_is_not_confirmed_presentation"),
        same_run_stop: unavailable("same_run_stop_contract_unavailable"),
    };
    let device = |class, transport, diagnostic_transport| DeviceSupport {
        class,
        transport,
        discovery: if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            macos_hid_discovery(
                class,
                cfg!(feature = "micro-hid"),
                cfg!(feature = "akp03-hid"),
            )
        } else {
            unavailable("device_backend_not_integrated")
        },
        diagnostic_transport,
        runtime_qualified: false,
    };
    let macos_hid = |enabled| {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) && enabled {
            Support::Compiled {
                prerequisite: "explicit_hid_probe_private_worker_capability_exact_native_evidence_unverified_on_hardware",
            }
        } else {
            unavailable("device_backend_not_integrated")
        }
    };
    let devices = vec![
        device(
            DeviceClass::PrimaryUsbController,
            Transport::Usb,
            macos_hid(cfg!(feature = "micro-hid")),
        ),
        device(
            DeviceClass::RelatedWirelessController,
            Transport::Bluetooth,
            macos_hid(cfg!(feature = "micro-hid")),
        ),
        device(
            DeviceClass::AlternateOriginalProfile,
            Transport::Usb,
            unavailable("decode_only_original_profile"),
        ),
        device(
            DeviceClass::AlternateSecondRevisionProfile,
            Transport::Usb,
            macos_hid(cfg!(feature = "akp03-hid")),
        ),
        device(
            DeviceClass::SerialControlSurface,
            Transport::Serial,
            unavailable("device_backend_not_integrated"),
        ),
    ];
    CapabilityInventory {
        schema: 1,
        evidence: "compile_time_only_not_native_or_physical_acceptance",
        os: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        read_only: true,
        command_ready: false,
        platform,
        hosts: Hosts {
            integrated,
            default_terminal,
        },
        devices,
        maintenance: MaintenanceSupport {
            primary_usb_controller: unavailable(
                "offline_transaction_model_only_no_native_update_backend",
            ),
            other_device_families: unavailable("maintenance_unsupported_for_device_family"),
        },
        actions: unavailable("independent_authority_and_qualification_gates_required"),
    }
}
