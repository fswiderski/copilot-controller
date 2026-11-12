use crate::domain::Status;

use super::super::{
    event::{
        Event, EventKind, MAX_EVENT_BYTES, ParseError, parse_event, parse_fork_message,
        parse_iso_timestamp, parse_timestamp_json,
    },
    hook::{
        Admission, AdmissionContext, HEARTBEAT_MAX_AGE_MS, HOOK_CAPACITY, Hook, HookInbox,
        HookKind, HookParseError, InboxError, PendingKind, parse_hook,
    },
    reducer::Reducer,
};
use super::session;

fn event(timestamp_ms: u64, kind: EventKind) -> Event {
    Event { timestamp_ms, kind }
}

fn hook(id: u128, timestamp: u64, kind: HookKind, pending: Option<PendingKind>) -> Hook {
    Hook::new(session(id), timestamp, kind, pending).expect("valid synthetic hook")
}

#[test]
fn e01_timestamp_boundaries_and_known_offsets() {
    for (timestamp, expected) in [
        ("2026-10-14T10:00:00.123Z", 1_791_972_000_123),
        ("2026-10-14T12:30:00.123456+02:30", 1_791_972_000_123),
        ("1970-01-01T00:00:00Z", 0),
        ("1970-01-01T01:00:00+01:00", 0),
        ("1969-12-31T23:59:59-00:01", 0), // The local year itself is out of range.
    ] {
        if timestamp.starts_with("1969") {
            assert_eq!(
                parse_iso_timestamp(timestamp),
                Err(ParseError::InvalidTimestamp)
            );
        } else {
            assert_eq!(parse_iso_timestamp(timestamp), Ok(expected), "{timestamp}");
        }
    }
    assert_eq!(
        parse_iso_timestamp("2000-02-29T23:59:59.1Z"),
        parse_iso_timestamp("2000-02-29T23:59:59.100000000Z")
    );
    assert_eq!(
        parse_iso_timestamp("2024-02-29T00:00:00.123999999Z").unwrap() % 1_000,
        123
    );
    assert!(parse_iso_timestamp("9999-12-31T23:59:59.999-23:59").is_ok());
    for invalid in [
        "1970-01-01T00:00:00+00:01",
        "1969-12-31T23:59:59Z",
        "1900-02-29T00:00:00Z",
        "2100-02-29T00:00:00Z",
        "2023-02-29T00:00:00Z",
        "2024-04-31T00:00:00Z",
        "2024-00-01T00:00:00Z",
        "2024-01-00T00:00:00Z",
        "2024-01-01t00:00:00Z",
        "2024-01-01T24:00:00Z",
        "2024-01-01T00:60:00Z",
        "2024-01-01T00:00:60Z",
        "2024-01-01T00:00:00",
        "2024-01-01T00:00:00z",
        "2024-01-01T00:00:00.Z",
        "2024-01-01T00:00:00.1234567890Z",
        "2024-01-01T00:00:00+24:00",
        "2024-01-01T00:00:00-23:60",
        "2024-01-01T00:00:00+0200",
        "２０２４-01-01T00:00:00Z",
    ] {
        assert_eq!(
            parse_iso_timestamp(invalid),
            Err(ParseError::InvalidTimestamp),
            "{invalid}"
        );
    }
    for (json, expected) in [
        ("0", 0),
        ("123", 123),
        ("-0", 0),
        ("123.999", 123),
        ("9.9e1", 99),
        ("1e-9999", 0),
        ("-0e9999", 0),
        ("9007199254740991.0", 9_007_199_254_740_991),
        ("18446744073709551615", u64::MAX),
    ] {
        assert_eq!(
            parse_timestamp_json(json.as_bytes()),
            Ok(expected),
            "{json}"
        );
    }
    for invalid in [
        "-1",
        "-0.1",
        "-1e-9999",
        "9007199254740992.0",
        "18446744073709551616",
        "1e309",
        "true",
        "null",
        "\"sensitive timestamp\"",
    ] {
        assert_eq!(
            parse_timestamp_json(invalid.as_bytes()),
            Err(ParseError::InvalidTimestamp)
        );
    }
}

#[test]
fn e01_presence_limits_strictness_and_minimization() {
    for (value, present) in [
        ("null", false),
        ("false", false),
        ("0", false),
        ("-0.0", false),
        ("0e9999", false),
        ("\"\"", false),
        ("[]", false),
        ("{}", false),
        ("[ \t ]", false),
        ("{ \r\n }", false),
        ("true", true),
        ("-1", true),
        ("0.1", true),
        ("1e-9999", true),
        ("-1e-9999", true),
        ("1e9999", true),
        ("\" \"", true),
        ("[null]", true),
        ("{\"sensitive\":\"value\"}", true),
    ] {
        let body = format!(
            "{{\"type\":\"assistant.message\",\"timestamp\":4,\"data\":{{\"content\":{value},\"toolRequests\":{value}}}}}"
        );
        let parsed = parse_event(body.as_bytes()).unwrap();
        assert_eq!(
            parsed.event.kind,
            EventKind::AssistantMessage {
                content_present: present,
                tool_requests_present: present,
            },
            "{value}"
        );
        assert!(!format!("{parsed:?}").contains("sensitive"));
    }
    let raw = br#"{"unknown":{"directory":"private-directory","nested":["conversation body"]},"type":"tool.execution_start","timestamp":7,"data":{"toolName":"ask_user","args":"private-arguments"}}"#;
    let parsed = parse_event(raw).unwrap();
    assert_eq!(
        parsed.event.kind,
        EventKind::ToolExecutionStart { asks_user: true }
    );
    let debug = format!("{parsed:?}");
    for secret in [
        "private-directory",
        "conversation body",
        "private-arguments",
        "ask_user",
    ] {
        assert!(!debug.contains(secret));
    }
    assert_eq!(
        parse_event(br#"{"type":"new.secret.type","timestamp":4}"#)
            .unwrap()
            .event
            .kind,
        EventKind::Unknown
    );
    assert_eq!(
        parse_event(br#"{"type":"tool.execution_start","timestamp":1,"data":{"toolName":1e9999}}"#)
            .unwrap()
            .event
            .kind,
        EventKind::ToolExecutionStart { asks_user: false }
    );
    for invalid in [
        br#"{"type":"abort","type":"abort","timestamp":1}"#.as_slice(),
        br#"{"type":"abort","timestamp":1,"timestamp":2}"#,
        br#"{"type":"abort"}"#,
        br#"{"type":5,"timestamp":1}"#,
        br#"["abort",1]"#,
        br#"{"type":"abort","timestamp":1,"data":[]}"#,
        br#"{"type":"assistant.message","timestamp":1,"data":{"content":1,"content":2}}"#,
        br#"{"type":"abort","timestamp":1} {"type":"abort","timestamp":2}"#,
    ] {
        assert_eq!(parse_event(invalid), Err(ParseError::InvalidRecord));
    }
    let mut invalid_utf8 = br#"{"type":"abort","timestamp":1,"ignored":""#.to_vec();
    invalid_utf8.push(0xff);
    invalid_utf8.extend_from_slice(br#""}"#);
    assert_eq!(parse_event(&invalid_utf8), Err(ParseError::InvalidRecord));
    let mut maximum = br#"{"type":"abort","timestamp":0}"#.to_vec();
    maximum.resize(MAX_EVENT_BYTES, b' ');
    assert!(parse_event(&maximum).is_ok());
    maximum.push(b' ');
    assert_eq!(parse_event(&maximum), Err(ParseError::TooLarge));
}

#[test]
fn e01_fork_notices_discard_labels_and_require_complete_grammar() {
    let child = session(42).to_string();
    let message = format!("Forked this session into \"private label\" ({child}).");
    assert_eq!(parse_fork_message(&message), Some(session(42)));
    let body = serde_json::json!({
        "type": "session.info",
        "timestamp": 81,
        "data": {"infoType":"fork", "message":message, "directory":"private-directory"}
    });
    let parsed = parse_event(&serde_json::to_vec(&body).unwrap()).unwrap();
    assert_eq!(parsed.event.kind, EventKind::Unknown);
    assert_eq!(parsed.fork_child, Some(session(42)));
    assert!(!format!("{parsed:?}").contains("private"));
    for invalid in [
        format!("Forked this session into \"\" ({child})."),
        format!("Forked this session into \"two\nlines\" ({child})."),
        format!("Forked this session into \"two\u{000b}lines\" ({child})."),
        format!("Forked this session into \"two\u{2028}lines\" ({child})."),
        format!("Forked this session into \"name\" ({child}). trailing"),
        format!(
            "Forked this session into \"name\" ({}).",
            child.replace('-', "")
        ),
        format!("Forked this session into \"name\" ({{{child}}})."),
        format!("Forked this session into \"name\" ({child})"),
    ] {
        assert_eq!(parse_fork_message(&invalid), None);
    }
    let wrong_type = serde_json::json!({
        "type":"other", "timestamp":1, "data":{"infoType":"fork", "message":message}
    });
    assert_eq!(
        parse_event(&serde_json::to_vec(&wrong_type).unwrap())
            .unwrap()
            .fork_child,
        None
    );
}

#[test]
fn e02_reducer_orders_meaningful_events_runs_and_completion() {
    let mut state = Reducer::new();
    assert_eq!(state.status(), Status::Idle);
    assert_eq!(state.run_token(), None);
    state.apply_event(event(10, EventKind::UserMessage));
    assert_eq!(
        (state.status(), state.run_token()),
        (Status::Working, Some(10))
    );
    state.apply_event(event(11, EventKind::AssistantTurnStart));
    state.apply_event(event(
        12,
        EventKind::AssistantMessage {
            content_present: true,
            tool_requests_present: true,
        },
    ));
    state.apply_event(event(13, EventKind::AssistantTurnEnd));
    assert_eq!(state.status(), Status::Working);
    assert_eq!(state.meaningful_event_ms(), 12);
    state.apply_event(event(
        14,
        EventKind::AssistantMessage {
            content_present: true,
            tool_requests_present: false,
        },
    ));
    state.apply_event(event(15, EventKind::AssistantTurnEnd));
    assert_eq!(state.status(), Status::Complete);
    state.apply_hook(hook(1, 100, HookKind::PostToolUse, None));
    assert_eq!(state.status(), Status::Complete);
    assert_eq!(state.meaningful_event_ms(), 15);
    assert_eq!(state.last_hook_ms(), 100);
    state.apply_event(event(1_000, EventKind::Unknown));
    state.apply_event(event(16, EventKind::SessionError));
    assert_eq!(
        (state.status(), state.meaningful_event_ms()),
        (Status::Error, 16)
    );
    assert!(state.final_candidate());
    state.apply_event(event(15, EventKind::Abort));
    assert_eq!(state.status(), Status::Error);
    state.apply_event(event(16, EventKind::UserMessage));
    assert_eq!(
        (state.status(), state.run_token()),
        (Status::Working, Some(16))
    );
    assert!(!state.final_candidate());
    state.apply_event(event(16, EventKind::Abort));
    assert_eq!(
        (state.status(), state.final_candidate()),
        (Status::Idle, false)
    );
    state.apply_event(event(17, EventKind::TaskComplete));
    let before = state;
    state.acknowledge_completion();
    assert_eq!(state.status(), Status::Idle);
    assert_eq!(state.meaningful_event_ms(), before.meaningful_event_ms());
    assert_eq!(state.run_token(), before.run_token());
    state.acknowledge_completion();
    assert_eq!(state.status(), Status::Idle);
}

#[test]
fn e02_pending_and_hook_side_effects_preserve_unrelated_state() {
    let mut state = Reducer::new();
    state.apply_event(event(5, EventKind::UserMessage));
    state.apply_event(event(6, EventKind::ToolExecutionStart { asks_user: true }));
    assert_eq!(state.status(), Status::Waiting);
    assert_eq!(state.pending_kind(), Some(PendingKind::ElicitationDialog));
    assert_eq!(state.pending_at_ms(), Some(6));
    state.apply_event(event(7, EventKind::ToolExecutionComplete));
    assert_eq!(state.status(), Status::Working);
    assert_eq!(state.pending_kind(), None);
    assert_eq!(state.pending_at_ms(), Some(6));
    assert_eq!(state.run_token(), Some(5));
    state.apply_hook(hook(1, 8, HookKind::UserPromptSubmitted, None));
    assert_eq!(state.run_token(), Some(8));
    state.apply_hook(hook(
        1,
        9,
        HookKind::Notification,
        Some(PendingKind::PermissionPrompt),
    ));
    assert_eq!(state.pending_at_ms(), Some(9));
    state.apply_hook(hook(1, 20, HookKind::Notification, None));
    assert_eq!(state.status(), Status::Waiting);
    assert_eq!(state.meaningful_event_ms(), 9);
    assert_eq!(state.last_hook_ms(), 20);
    state.apply_hook(hook(1, 8, HookKind::ErrorOccurred, None));
    assert_eq!(state.status(), Status::Waiting);
    state.apply_hook(hook(1, 10, HookKind::AgentStop, None));
    assert_eq!(state.status(), Status::Complete);
    assert_eq!(state.pending_at_ms(), Some(9));
    state.apply_hook(hook(1, 11, HookKind::SessionEnd, None));
    assert_eq!(state.status(), Status::Off);
    state.apply_hook(hook(1, 12, HookKind::SessionStart, None));
    assert_eq!(state.status(), Status::Idle);
    state.apply_event(event(100, EventKind::TaskComplete));
    state.apply_hook(hook(1, 50, HookKind::SessionEnd, None));
    assert_eq!(state.status(), Status::Complete);
    assert_eq!(state.meaningful_event_ms(), 100);
    assert_eq!(state.last_hook_ms(), 50);
    state.invalidate();
    assert_eq!(state.status(), Status::Unknown);
    assert_eq!(state.meaningful_event_ms(), 0);
    assert_eq!(state.last_hook_ms(), 0);
    assert_eq!(state.pending_at_ms(), None);
    assert_eq!(state.run_token(), None);
}

#[test]
fn e04_hook_payload_and_persistence_are_minimized_and_strict() {
    let id = session(9).to_string().to_uppercase();
    let body = format!(
        "{{\"sessionId\":\"{id}\",\"timestamp\":\"2026-10-14T10:00:00.123Z\",\"notification_type\":\"permission_prompt\",\"message\":\"secret notification\",\"cwd\":\"private-dir\"}}"
    );
    let notification = parse_hook("notification", body.as_bytes()).unwrap();
    assert_eq!(notification.session_id(), session(9));
    assert_eq!(notification.timestamp_ms(), 1_791_972_000_123);
    assert_eq!(
        notification.notification(),
        Some(PendingKind::PermissionPrompt)
    );
    let persisted = serde_json::to_string(&notification).unwrap();
    assert!(!persisted.contains("secret"));
    assert!(!persisted.contains("private-dir"));
    assert_eq!(
        serde_json::from_str::<Hook>(&persisted).unwrap(),
        notification
    );
    assert_eq!(
        parse_hook("agentStop", body.as_bytes())
            .unwrap()
            .notification(),
        None
    );
    assert_eq!(
        parse_hook("unsupported-with-secret", body.as_bytes()),
        Err(HookParseError::UnknownKind)
    );
    let unknown = format!(
        "{{\"sessionId\":\"{id}\",\"timestamp\":3,\"notification_type\":\"private other\"}}"
    );
    assert_eq!(
        parse_hook("notification", unknown.as_bytes())
            .unwrap()
            .notification(),
        None
    );
    for invalid in [
        format!(
            "{{\"sessionId\":\"{}\",\"timestamp\":1}}",
            id.replace('-', "")
        ),
        format!("{{\"sessionId\":\"{id}\",\"timestamp\":-1}}"),
        format!("{{\"sessionId\":\"{id}\",\"timestamp\":1,\"timestamp\":2}}"),
        format!("{{\"sessionId\":\"{id}\"}}"),
    ] {
        assert_eq!(
            parse_hook("sessionStart", invalid.as_bytes()),
            Err(HookParseError::InvalidRecord)
        );
    }
    let mut invalid_utf8 =
        format!("{{\"sessionId\":\"{id}\",\"timestamp\":1,\"ignored\":\"").into_bytes();
    invalid_utf8.push(0xff);
    invalid_utf8.extend_from_slice(br#""}"#);
    assert_eq!(
        parse_hook("sessionStart", &invalid_utf8),
        Err(HookParseError::InvalidRecord)
    );
    let id = session(9);
    for invalid in [
        serde_json::json!({"session_id":id, "timestamp_ms":1, "kind":"agentStop"}),
        serde_json::json!({"session_id":id, "timestamp_ms":1, "kind":"agentStop", "notification":"permission_prompt"}),
        serde_json::json!({"session_id":id, "timestamp_ms":1, "kind":"agentStop", "notification":null, "extra":"private"}),
    ] {
        assert!(serde_json::from_value::<Hook>(invalid).is_err());
    }
    assert!(
        serde_json::from_str::<Hook>(
            r#"["00000000-0000-0000-0000-000000000009",1,"agentStop",null]"#
        )
        .is_err()
    );
    let bad_kind = format!(
        "{{\"session_id\":\"{id}\",\"timestamp_ms\":1,\"kind\":\"private-secret\",\"notification\":null}}"
    );
    assert!(
        !serde_json::from_str::<Hook>(&bad_kind)
            .unwrap_err()
            .to_string()
            .contains("private-secret")
    );
    let bad_field = format!(
        "{{\"session_id\":\"{id}\",\"timestamp_ms\":1,\"kind\":\"agentStop\",\"notification\":null,\"private-secret\":1}}"
    );
    assert!(
        !serde_json::from_str::<Hook>(&bad_field)
            .unwrap_err()
            .to_string()
            .contains("private-secret")
    );
    assert!(
        !serde_json::from_str::<HookInbox>("\"private-secret\"")
            .unwrap_err()
            .to_string()
            .contains("private-secret")
    );
    assert_eq!(
        parse_hook(
            "notification",
            format!("{{\"sessionId\":\"{id}\",\"timestamp\":1,\"notification_type\":1e9999}}")
                .as_bytes()
        )
        .unwrap()
        .notification(),
        None
    );
}

#[test]
fn e04_admission_capacity_deduplication_and_stable_session_drain() {
    let mut inbox = HookInbox::default();
    let first = hook(1, 0, HookKind::SessionStart, None);
    let base = AdmissionContext {
        bound_session: Some(session(1)),
        stopped: false,
        heartbeat_ms: Some(0),
        now_ms: HEARTBEAT_MAX_AGE_MS,
    };
    assert_eq!(
        inbox.admit(
            first,
            AdmissionContext {
                bound_session: None,
                ..base
            }
        ),
        Err(InboxError::Unbound)
    );
    assert_eq!(
        inbox.admit(
            first,
            AdmissionContext {
                stopped: true,
                ..base
            }
        ),
        Err(InboxError::Stopped)
    );
    assert_eq!(
        inbox.admit(
            first,
            AdmissionContext {
                heartbeat_ms: None,
                ..base
            }
        ),
        Err(InboxError::MissingHeartbeat)
    );
    assert_eq!(
        inbox.admit(
            first,
            AdmissionContext {
                heartbeat_ms: Some(base.now_ms + 1),
                ..base
            }
        ),
        Err(InboxError::FutureHeartbeat)
    );
    assert_eq!(
        inbox.admit(
            first,
            AdmissionContext {
                now_ms: base.now_ms + 1,
                ..base
            }
        ),
        Err(InboxError::StaleHeartbeat)
    );
    for timestamp in 0..HOOK_CAPACITY as u64 {
        assert_eq!(
            inbox.admit(hook(1, timestamp, HookKind::SessionStart, None), base),
            Ok(Admission::Added)
        );
    }
    assert_eq!(inbox.admit(first, base), Ok(Admission::Duplicate));
    assert_eq!(
        inbox.admit(hook(1, 999, HookKind::SessionStart, None), base),
        Err(InboxError::Full)
    );
    let serialized = serde_json::to_vec(&inbox).unwrap();
    assert_eq!(HookInbox::from_json(&serialized).unwrap(), inbox);
    let mut too_many: Vec<Hook> = inbox.iter().copied().collect();
    too_many.push(hook(1, 999, HookKind::SessionStart, None));
    assert_eq!(
        HookInbox::from_json(&serde_json::to_vec(&too_many).unwrap()),
        Err(InboxError::InvalidRecord)
    );
    let duplicates = vec![first, first];
    assert_eq!(
        HookInbox::from_json(&serde_json::to_vec(&duplicates).unwrap()),
        Err(InboxError::InvalidRecord)
    );

    let mut inbox = HookInbox::default();
    let other = AdmissionContext {
        bound_session: Some(session(2)),
        ..base
    };
    for item in [
        hook(1, 10, HookKind::SessionEnd, None),
        hook(2, 1, HookKind::SessionStart, None),
        hook(1, 5, HookKind::UserPromptSubmitted, None),
        hook(1, 10, HookKind::SessionStart, None),
    ] {
        inbox
            .admit(
                item,
                if item.session_id() == session(1) {
                    base
                } else {
                    other
                },
            )
            .unwrap();
    }
    let mut reducer = Reducer::new();
    assert_eq!(inbox.drain_session(session(1), &mut reducer), 3);
    assert_eq!(
        (reducer.status(), reducer.meaningful_event_ms()),
        (Status::Idle, 10)
    );
    assert_eq!(reducer.last_hook_ms(), 10);
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox.iter().next().unwrap().session_id(), session(2));
}
