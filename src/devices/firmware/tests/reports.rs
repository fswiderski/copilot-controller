use serde_json::{Value, json};

use super::super::{UpdateOutcome, UpdatePhase, UpdateReport};

fn successful_outcome() -> UpdateOutcome {
    UpdateOutcome::success(UpdateReport {
        phase: UpdatePhase::VerifyingRunningFirmware,
        failure_phase: None,
        bootloader_entry_attempted: true,
        backup_verified: true,
        flash_write_attempted: true,
        firmware_verified: true,
        settings_preserved: true,
        running_version: Some("0.6.3".to_owned()),
    })
}

fn rejects(value: &Value) {
    assert!(
        serde_json::from_value::<UpdateOutcome>(value.clone()).is_err(),
        "unexpectedly accepted {value}"
    );
}

#[test]
fn supervisor_deserialization_rejects_unknown_missing_and_forged_evidence() {
    let success = successful_outcome();
    assert!(success.validate_supervisor().is_ok());
    let value = serde_json::to_value(&success).unwrap();
    assert_eq!(
        serde_json::from_value::<UpdateOutcome>(value.clone()).unwrap(),
        success
    );
    let encoded = serde_json::to_string(&success).unwrap();
    let duplicate = encoded.replacen(
        "\"command_ready\":false",
        "\"command_ready\":false,\"command_ready\":false",
        1,
    );
    assert!(serde_json::from_str::<UpdateOutcome>(&duplicate).is_err());
    let duplicate_phase = encoded.replacen(
        "\"phase\":\"complete\"",
        "\"phase\":\"complete\",\"phase\":\"complete\"",
        1,
    );
    assert!(serde_json::from_str::<UpdateOutcome>(&duplicate_phase).is_err());
    let mut bad = value.clone();
    bad["extra"] = json!(1);
    rejects(&bad);
    let mut bad = value.clone();
    bad["report"]["extra"] = json!(1);
    rejects(&bad);
    for field in [
        "error",
        "update_succeeded",
        "command_ready",
        "hardware_qualified",
    ] {
        let mut bad = value.clone();
        bad.as_object_mut().unwrap().remove(field);
        rejects(&bad);
    }
    for field in [
        "phase",
        "failure_phase",
        "running_version",
        "backup_verified",
    ] {
        let mut bad = value.clone();
        bad["report"].as_object_mut().unwrap().remove(field);
        rejects(&bad);
    }
    for field in ["command_ready", "hardware_qualified"] {
        let mut bad = value.clone();
        bad[field] = json!(true);
        rejects(&bad);
    }
    for field in [
        "bootloader_entry_attempted",
        "backup_verified",
        "flash_write_attempted",
        "firmware_verified",
        "settings_preserved",
    ] {
        let mut bad = value.clone();
        bad["report"][field] = json!(false);
        rejects(&bad);
    }
    for wrong in [json!(null), json!("v0.6.3"), json!("0.6.4"), json!(3)] {
        let mut bad = value.clone();
        bad["report"]["running_version"] = wrong;
        rejects(&bad);
    }
    for phase in ["invented_phase", "failed", "writing_firmware"] {
        let mut bad = value.clone();
        bad["report"]["phase"] = json!(phase);
        rejects(&bad);
    }
    let mut bad = value.clone();
    bad["error"] = json!("claimed success despite failure");
    rejects(&bad);
    let mut bad = value;
    bad["report"]["failure_phase"] = json!("security");
    rejects(&bad);
}

#[test]
fn failure_reports_keep_strict_phases_and_progress() {
    let report = UpdateReport {
        phase: UpdatePhase::VerifyingRunningFirmware,
        bootloader_entry_attempted: true,
        backup_verified: true,
        flash_write_attempted: true,
        firmware_verified: true,
        settings_preserved: true,
        running_version: Some("v0.6.3".to_owned()),
        ..UpdateReport::default()
    };
    let failure = UpdateOutcome::failed(report, "final version mismatch");
    assert!(failure.validate_supervisor().is_ok());
    let value = serde_json::to_value(&failure).unwrap();
    assert_eq!(
        serde_json::from_value::<UpdateOutcome>(value.clone()).unwrap(),
        failure
    );
    let mut bad = value.clone();
    bad["report"]["failure_phase"] = json!("preflight");
    rejects(&bad);
    let mut bad = value.clone();
    bad["report"]["failure_phase"] = json!("complete");
    rejects(&bad);
    let mut bad = value.clone();
    bad["report"]["failure_phase"] = json!(null);
    rejects(&bad);
    let mut bad = value.clone();
    bad["error"] = json!(null);
    rejects(&bad);
    let mut bad = value;
    bad["report"]["flash_write_attempted"] = json!(false);
    rejects(&bad);
}
