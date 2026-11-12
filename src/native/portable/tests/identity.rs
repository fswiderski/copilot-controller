use serde_json::{Value, json};
use uuid::Uuid;

use crate::domain::{NativeProcessIdentity, OpaqueId, RuntimeEpoch, TargetIdentity};

use super::super::identity::{DeliveryOutcome, Registration, RunScope, ScopedOutcome};

fn epoch(value: u128) -> RuntimeEpoch {
    RuntimeEpoch::from_uuid(Uuid::from_u128(value))
}

fn opaque(value: &str) -> OpaqueId {
    OpaqueId::new(value).unwrap()
}

fn scope() -> RunScope {
    RunScope {
        target: TargetIdentity::IntegratedHost {
            host_epoch: epoch(1),
            target: opaque("target"),
            attachment: opaque("attachment"),
        },
        provider_epoch: epoch(2),
        run: Uuid::from_u128(3),
    }
}

#[test]
fn opaque_ids_use_exact_utf8_length_and_unicode_control_rules() {
    for bad in [
        "".to_owned(),
        "x".repeat(257),
        "line\nbreak".to_owned(),
        "\u{85}".to_owned(),
    ] {
        assert!(OpaqueId::new(bad.clone()).is_err());
        assert!(serde_json::from_value::<OpaqueId>(Value::String(bad)).is_err());
    }
    for good in [
        "x".repeat(256),
        "😺".repeat(64),
        " punctuation / ".to_owned(),
        "\u{200b}".to_owned(),
    ] {
        let id = OpaqueId::new(good.clone()).unwrap();
        assert_eq!(id.as_str(), good);
        assert_eq!(serde_json::from_value::<OpaqueId>(json!(good)).unwrap(), id);
    }
    assert!(OpaqueId::new(format!("{}a", "😺".repeat(64))).is_err());
}

#[test]
fn target_schemas_are_distinct_and_reject_extra_fields() {
    let integrated = scope().target;
    let serialized = serde_json::to_value(&integrated).unwrap();
    assert_eq!(serialized["backend"], "integrated_host");
    assert!(serialized.get("process").is_none());
    assert!(serialized.get("window").is_none());
    assert_eq!(
        serde_json::from_value::<TargetIdentity>(serialized.clone()).unwrap(),
        integrated
    );
    let mut extra = serialized;
    extra["registration"] = json!("not-a-target-field");
    assert!(serde_json::from_value::<TargetIdentity>(extra).is_err());

    let native = TargetIdentity::WindowsNative {
        process: NativeProcessIdentity {
            pid: 0,
            creation_time_100ns: 0,
            session_id: 0,
        },
        window: 0,
    };
    let mut value = serde_json::to_value(&native).unwrap();
    assert_eq!(
        serde_json::from_value::<TargetIdentity>(value.clone()).unwrap(),
        native
    );
    value["process"]["extra"] = json!(1);
    assert!(serde_json::from_value::<TargetIdentity>(value).is_err());
    let nil_host = TargetIdentity::IntegratedHost {
        host_epoch: epoch(0),
        target: opaque(" "),
        attachment: opaque("!"),
    };
    assert_eq!(
        serde_json::from_value::<TargetIdentity>(serde_json::to_value(&nil_host).unwrap()).unwrap(),
        nil_host
    );
    let registration = Registration(opaque("registration"));
    assert_eq!(
        serde_json::to_value(registration).unwrap(),
        json!("registration")
    );
}

#[test]
fn unknown_outcome_blocks_only_the_complete_unchanged_scope() {
    let original = scope();
    let outcome = ScopedOutcome {
        scope: original.clone(),
        intent: Uuid::from_u128(4),
        outcome: DeliveryOutcome::Unknown,
    };
    assert!(outcome.blocks_scope(&original));
    let mut candidates = Vec::new();
    let mut changed = original.clone();
    changed.provider_epoch = epoch(5);
    candidates.push(changed);
    let mut changed = original.clone();
    changed.run = Uuid::from_u128(6);
    candidates.push(changed);
    for target in [
        TargetIdentity::IntegratedHost {
            host_epoch: epoch(7),
            target: opaque("target"),
            attachment: opaque("attachment"),
        },
        TargetIdentity::IntegratedHost {
            host_epoch: epoch(1),
            target: opaque("other"),
            attachment: opaque("attachment"),
        },
        TargetIdentity::IntegratedHost {
            host_epoch: epoch(1),
            target: opaque("target"),
            attachment: opaque("other"),
        },
        TargetIdentity::WindowsNative {
            process: NativeProcessIdentity {
                pid: 1,
                creation_time_100ns: 2,
                session_id: 3,
            },
            window: 4,
        },
    ] {
        let mut changed = original.clone();
        changed.target = target;
        candidates.push(changed);
    }
    for candidate in candidates {
        assert!(!outcome.blocks_scope(&candidate));
        assert!(outcome.blocks_scope(&original));
    }
    for resolution in [DeliveryOutcome::Completed, DeliveryOutcome::NotAdmitted] {
        let mut resolved = outcome.clone();
        resolved.outcome = resolution;
        assert!(!resolved.blocks_scope(&original));
    }
    let mut other_intent = outcome.clone();
    other_intent.intent = Uuid::from_u128(99);
    assert!(other_intent.blocks_scope(&original));

    let mut encoded = serde_json::to_value(&outcome).unwrap();
    assert_eq!(
        serde_json::from_value::<ScopedOutcome>(encoded.clone()).unwrap(),
        outcome
    );
    encoded["scope"]["extra"] = json!(true);
    assert!(serde_json::from_value::<ScopedOutcome>(encoded).is_err());
    let mut encoded = serde_json::to_value(&outcome).unwrap();
    encoded["extra"] = json!(true);
    assert!(serde_json::from_value::<ScopedOutcome>(encoded).is_err());
    let target = serde_json::to_value(&original.target).unwrap();
    assert!(
        serde_json::from_value::<RunScope>(json!([
            target,
            epoch(2).to_string(),
            Uuid::from_u128(3).to_string()
        ]))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ScopedOutcome>(json!([
            serde_json::to_value(original).unwrap(),
            Uuid::from_u128(4).to_string(),
            "unknown"
        ]))
        .is_err()
    );
}

#[test]
fn native_lifetime_and_window_each_participate_in_scope_equality() {
    let mut original = scope();
    original.target = TargetIdentity::WindowsNative {
        process: NativeProcessIdentity {
            pid: 123,
            creation_time_100ns: 456,
            session_id: 3,
        },
        window: 42,
    };
    let outcome = ScopedOutcome {
        scope: original.clone(),
        intent: Uuid::from_u128(4),
        outcome: DeliveryOutcome::Unknown,
    };
    for field in 0..4 {
        let mut changed = original.clone();
        if let TargetIdentity::WindowsNative { process, window } = &mut changed.target {
            match field {
                0 => process.pid += 1,
                1 => process.creation_time_100ns += 1,
                2 => process.session_id += 1,
                _ => *window += 1,
            }
        }
        assert!(!outcome.blocks_scope(&changed));
    }
    assert!(outcome.blocks_scope(&original));
}
