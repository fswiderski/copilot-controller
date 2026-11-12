use serde::{Deserialize, Deserializer, Serialize, de};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    ValidatingOptions,
    Preflight,
    PreparingPrivateBackup,
    ToolVersion,
    QualifyingUsbTarget,
    Bootloader,
    Security,
    FlashSize,
    ReadingBackup,
    PersistingBackup,
    VerifyingBackup,
    WritingFirmware,
    VerifyingFirmware,
    VerifyingSettings,
    Resetting,
    VerifyingRunningFirmware,
    Complete,
    Failed,
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateReport {
    pub phase: UpdatePhase,
    #[serde(deserialize_with = "required_nullable")]
    pub failure_phase: Option<UpdatePhase>,
    pub bootloader_entry_attempted: bool,
    pub backup_verified: bool,
    pub flash_write_attempted: bool,
    pub firmware_verified: bool,
    pub settings_preserved: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub running_version: Option<String>,
}

impl Default for UpdateReport {
    fn default() -> Self {
        Self {
            phase: UpdatePhase::ValidatingOptions,
            failure_phase: None,
            bootloader_entry_attempted: false,
            backup_verified: false,
            flash_write_attempted: false,
            firmware_verified: false,
            settings_preserved: false,
            running_version: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UpdateOutcome {
    pub update_succeeded: bool,
    pub command_ready: bool,
    pub hardware_qualified: bool,
    pub report: UpdateReport,
    pub error: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutcomeWire {
    update_succeeded: bool,
    command_ready: bool,
    hardware_qualified: bool,
    report: UpdateReport,
    #[serde(deserialize_with = "required_nullable")]
    error: Option<String>,
}

impl<'de> Deserialize<'de> for UpdateOutcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = OutcomeWire::deserialize(deserializer)?;
        let outcome = Self {
            update_succeeded: wire.update_succeeded,
            command_ready: wire.command_ready,
            hardware_qualified: wire.hardware_qualified,
            report: wire.report,
            error: wire.error,
        };
        outcome.validate_supervisor().map_err(de::Error::custom)?;
        Ok(outcome)
    }
}

impl UpdateOutcome {
    pub(crate) fn failed(mut report: UpdateReport, error: impl Into<String>) -> Self {
        report.failure_phase = Some(report.phase);
        report.phase = UpdatePhase::Failed;
        Self {
            update_succeeded: false,
            command_ready: false,
            hardware_qualified: false,
            report,
            error: Some(error.into()),
        }
    }

    #[cfg(test)]
    pub(crate) fn success(mut report: UpdateReport) -> Self {
        report.phase = UpdatePhase::Complete;
        Self {
            update_succeeded: true,
            command_ready: false,
            hardware_qualified: false,
            report,
            error: None,
        }
    }

    /// Validate the supervisor's invariant even for an outcome constructed in-process.
    pub fn validate_supervisor(&self) -> Result<(), &'static str> {
        let report = &self.report;
        if self.command_ready || self.hardware_qualified {
            return Err("maintenance must not claim readiness or hardware qualification");
        }
        if (report.backup_verified && !report.bootloader_entry_attempted)
            || (report.flash_write_attempted && !report.backup_verified)
            || (report.firmware_verified && !report.flash_write_attempted)
            || (report.settings_preserved && !report.firmware_verified)
        {
            return Err("maintenance progress flags are inconsistent");
        }
        if self.update_succeeded {
            if report.phase != UpdatePhase::Complete
                || report.failure_phase.is_some()
                || self.error.is_some()
                || !report.bootloader_entry_attempted
                || !report.backup_verified
                || !report.flash_write_attempted
                || !report.firmware_verified
                || !report.settings_preserved
                || report.running_version.as_deref() != Some("0.6.3")
            {
                return Err("claimed success lacks required maintenance evidence");
            }
        } else {
            let failure = report
                .failure_phase
                .ok_or("failed outcome lacks its failure phase")?;
            if report.phase != UpdatePhase::Failed
                || failure >= UpdatePhase::Complete
                || self.error.as_deref().is_none_or(str::is_empty)
                || (report.bootloader_entry_attempted && failure < UpdatePhase::Bootloader)
                || (report.backup_verified && failure < UpdatePhase::VerifyingBackup)
                || (report.flash_write_attempted && failure < UpdatePhase::WritingFirmware)
                || (report.firmware_verified && failure < UpdatePhase::VerifyingFirmware)
                || (report.settings_preserved && failure < UpdatePhase::VerifyingSettings)
                || (report.running_version.is_some()
                    && (failure < UpdatePhase::VerifyingRunningFirmware
                        || !report.settings_preserved))
            {
                return Err("failed outcome contains impossible maintenance progress");
            }
        }
        Ok(())
    }
}
