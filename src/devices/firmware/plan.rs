use std::path::Path;

use serde::Serialize;

use super::FirmwareError;
use super::artifacts::{ArtifactPin, IMAGE_PIN, TOOL_PIN, verify_artifact, verify_image_extent};
use super::options::CONFIRMATION;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenancePath {
    Unavailable,
    OfficialEsp32Usb,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenancePlatform {
    MacosAarch64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceTransport {
    Usb,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceEvidence {
    NoVerifiedUpdatePath,
    ManualReferenceVerifiedAutomatedAcceptancePending,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FirmwareCapability {
    pub path: MaintenancePath,
    pub driver_available: bool,
    pub platform: Option<MaintenancePlatform>,
    pub transport: Option<MaintenanceTransport>,
    pub supported_version: Option<&'static str>,
    pub evidence: MaintenanceEvidence,
}

pub fn unavailable_capability() -> FirmwareCapability {
    FirmwareCapability {
        path: MaintenancePath::Unavailable,
        driver_available: false,
        platform: None,
        transport: None,
        supported_version: None,
        evidence: MaintenanceEvidence::NoVerifiedUpdatePath,
    }
}

/// Describes the reviewed path, not an available native maintenance driver.
pub fn micro_capability() -> FirmwareCapability {
    FirmwareCapability {
        path: MaintenancePath::OfficialEsp32Usb,
        driver_available: false,
        platform: Some(MaintenancePlatform::MacosAarch64),
        transport: Some(MaintenanceTransport::Usb),
        supported_version: Some("0.6.3"),
        evidence: MaintenanceEvidence::ManualReferenceVerifiedAutomatedAcceptancePending,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FirmwarePlan {
    pub read_only: bool,
    pub device_access: bool,
    pub command_ready: bool,
    pub firmware_update: FirmwareCapability,
    pub artifact_verified: bool,
    pub tool_verified: bool,
    pub confirmation: &'static str,
    pub release: &'static str,
    pub steps: [&'static str; 7],
}

const STEPS: [&str; 7] = [
    "Verify local artifacts and qualified USB identity.",
    "Enter bootloader once; bind explicit serial port to same location.",
    "Check ESP32-S3, 16MB, disabled security features.",
    "Read/verify full backup; compare partition sector.",
    "Write only reviewed merged image at zero.",
    "Verify image and unchanged settings.",
    "Watchdog reset; verify same-location USB firmware 0.6.3.",
];

fn plan_with_pins(
    image: &Path,
    tool: &Path,
    image_pin: ArtifactPin<'_>,
    tool_pin: ArtifactPin<'_>,
) -> Result<FirmwarePlan, FirmwareError> {
    let image_bytes = verify_artifact(image, image_pin)?;
    verify_image_extent(&image_bytes)?;
    let _ = verify_artifact(tool, tool_pin)?;
    Ok(FirmwarePlan {
        read_only: true,
        device_access: false,
        command_ready: false,
        firmware_update: micro_capability(),
        artifact_verified: true,
        tool_verified: true,
        confirmation: CONFIRMATION,
        release: "specs/MICRO_AND_FIRMWARE.md#103-local-artifact-input-requirements",
        steps: STEPS,
    })
}

#[cfg(test)]
pub(crate) fn synthetic_plan(
    image: &Path,
    tool: &Path,
    image_pin: ArtifactPin<'_>,
    tool_pin: ArtifactPin<'_>,
) -> Result<FirmwarePlan, FirmwareError> {
    plan_with_pins(image, tool, image_pin, tool_pin)
}

/// Verify only explicitly supplied local files; never discover or open a device.
pub fn offline_plan(image: &Path, tool: &Path) -> Result<FirmwarePlan, FirmwareError> {
    if !cfg!(feature = "micro-firmware") {
        return Err(FirmwareError::new(
            "offline firmware validation requires the micro-firmware feature",
        ));
    }
    plan_with_pins(image, tool, IMAGE_PIN, TOOL_PIN)
}
