use std::io::{self, Cursor, Read};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Serialize, Serializer, ser::Error as _};
use serde_json::{Value, json};
use uuid::Uuid;

use super::protocol::{
    self, Outcome, RequestEnvelope, RequestEnvelopeRef, ResponseEnvelope, VERSION,
};
use super::supervision::{Progress, SupervisorGate};
use super::*;

static CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);

fn test_callback(payload: Value) -> Result<Value, DiagnosticCallbackError> {
    CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
    Ok(payload)
}

fn failing_callback(_: Value) -> Result<Value, DiagnosticCallbackError> {
    Err(DiagnosticCallbackError)
}

fn panicking_callback(_: Value) -> Result<Value, DiagnosticCallbackError> {
    panic!("private callback detail")
}

fn panicking_payload_callback(_: Value) -> Result<Value, DiagnosticCallbackError> {
    struct PanicOnDrop;
    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("private panic-payload destructor");
        }
    }
    std::panic::panic_any(PanicOnDrop);
}

static CALLBACKS: [DiagnosticCallback; 3] = [
    DiagnosticCallback::new("windows_process", test_callback),
    DiagnosticCallback::new("health", test_callback),
    DiagnosticCallback::new("_1.-", test_callback),
];

fn allowlist() -> DiagnosticAllowlist {
    DiagnosticAllowlist::new(&CALLBACKS).unwrap()
}

fn request(kind: &str) -> DiagnosticRequest {
    DiagnosticRequest {
        kind: kind.to_owned(),
        payload: json!({}),
    }
}

fn nonce() -> Uuid {
    Uuid::from_u128(42)
}

fn worker() -> WorkerIdentity {
    WorkerIdentity {
        pid: 7,
        creation_time_100ns: 123,
    }
}

fn response() -> ResponseEnvelope {
    ResponseEnvelope {
        version: VERSION,
        request_nonce: nonce(),
        worker: worker(),
        outcome: Outcome::Ok {
            payload: json!({"answer": 42}),
        },
    }
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut result = (body.len() as u32).to_le_bytes().to_vec();
    result.extend_from_slice(body);
    result
}

struct OneByte<R>(R);

impl<R: Read> Read for OneByte<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let length = buffer.len().min(1);
        self.0.read(&mut buffer[..length])
    }
}

#[test]
fn frame_handles_one_byte_reads_and_exact_eof() {
    let request = request("_1.-");
    let encoded = protocol::encode_frame(&RequestEnvelopeRef {
        version: VERSION,
        request_nonce: nonce(),
        request: &request,
    })
    .unwrap();
    let decoded: RequestEnvelope =
        protocol::read_frame(&mut OneByte(Cursor::new(encoded))).unwrap();
    protocol::validate_request(&decoded, &allowlist()).unwrap();
    assert_eq!(decoded.request.kind, request.kind);
    assert_eq!(decoded.request.payload, request.payload);
    assert_eq!(decoded.request_nonce, nonce());
}

#[test]
fn response_encoding_is_inclusive_and_failure_returns_a_correlated_compact_frame() {
    let identity = WorkerIdentity {
        pid: u32::MAX,
        creation_time_100ns: u64::MAX,
    };
    let nonce = Uuid::from_u128(u128::MAX);
    let empty =
        protocol::encode_response(&Outcome::Ok { payload: json!("") }, nonce, identity).unwrap();
    let length = MAX_FRAME_BODY_BYTES - (empty.len() - 4);
    let exact = protocol::encode_response(
        &Outcome::Ok {
            payload: json!("x".repeat(length)),
        },
        nonce,
        identity,
    )
    .unwrap();
    assert_eq!(exact.len(), MAX_FRAME_BODY_BYTES + 4);
    let decoded: ResponseEnvelope = protocol::read_frame(&mut Cursor::new(exact)).unwrap();
    assert!(matches!(
        protocol::validate_response(decoded, nonce, identity).unwrap(),
        Outcome::Ok { .. }
    ));

    let too_large = Outcome::Ok {
        payload: json!("x".repeat(length + 1)),
    };
    let bounded = protocol::encode_response(&too_large, nonce, identity).unwrap();
    assert!(bounded.len() < 256);
    let decoded: ResponseEnvelope = protocol::read_frame(&mut Cursor::new(bounded)).unwrap();
    let outcome = protocol::validate_response(decoded, nonce, identity).unwrap();
    assert!(matches!(outcome, Outcome::ResponseEncodingFailed));
    let mut progress = Progress {
        send: Some(Ok(())),
        response: Some(Ok(outcome)),
        stderr: Some(Ok(())),
        exit: Some(Ok(0)),
    };
    assert_eq!(
        progress.decide(false, false),
        Some(Err(WorkerErrorKind::FrameLimit))
    );

    struct Broken;
    impl Serialize for Broken {
        fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(S::Error::custom("private serialization failure"))
        }
    }
    let bounded = protocol::encode_response(&Broken, nonce, identity).unwrap();
    assert!(!String::from_utf8_lossy(&bounded).contains("private"));
    let decoded: ResponseEnvelope = protocol::read_frame(&mut Cursor::new(bounded)).unwrap();
    assert!(matches!(
        protocol::validate_response(decoded, nonce, identity).unwrap(),
        Outcome::ResponseEncodingFailed,
    ));
}

#[test]
fn callback_payload_budget_fits_the_worst_case_response_envelope() {
    let identity = WorkerIdentity {
        pid: u32::MAX,
        creation_time_100ns: u64::MAX,
    };
    let nonce = Uuid::from_u128(u128::MAX);
    let outcome = Outcome::Ok {
        payload: json!("x".repeat(MAX_CALLBACK_PAYLOAD_BYTES - 2)),
    };
    let frame = protocol::encode_response(&outcome, nonce, identity).unwrap();
    assert!(frame.len() <= MAX_FRAME_BODY_BYTES + 4);
    let decoded: ResponseEnvelope = protocol::read_frame(&mut Cursor::new(frame)).unwrap();
    assert!(matches!(
        protocol::validate_response(decoded, nonce, identity).unwrap(),
        Outcome::Ok { .. }
    ));
}

#[test]
fn duplicate_keys_are_rejected_before_callback_payload_becomes_value() {
    for body in [
        br#"{"kind":"ct_serial_probe","payload":{"capture_ms":0,"capture_ms":1}}"#.as_slice(),
        br#"{"payload":{"capture_ms":0,"capture_\u006ds":1}}"#,
        br#"{"payload":[{"schema_version":1,"schema_version":2}]}"#,
        br#"{"outcome":{"status":"report","report":{"pattern":"not_requested","pattern":"private"}}}"#,
    ] {
        assert_eq!(
            protocol::read_frame::<_, Value>(&mut Cursor::new(frame(body))),
            Err(WorkerErrorKind::Protocol),
        );
    }
    let body =
        br#"{"payload":[null,true,false,-1,1.25,184467440737095516160,"text",{"capture_ms":0}]}"#;
    let decoded = protocol::read_frame::<_, Value>(&mut Cursor::new(frame(body))).unwrap();
    assert_eq!(decoded, serde_json::from_slice::<Value>(body).unwrap());
}

#[test]
fn health_registry_rejects_ct_while_explicit_worker_registry_accepts_it() {
    let health = crate::cli::health_allowlist().unwrap();
    let combined = crate::cli::diagnostic_allowlist().unwrap();
    let envelope = RequestEnvelope {
        version: VERSION,
        request_nonce: nonce(),
        request: DiagnosticRequest {
            kind: "ct_serial_probe".into(),
            payload: serde_json::to_value(crate::devices::ct::probe::Options::default()).unwrap(),
        },
    };
    assert!(health.find("health").is_some());
    assert!(health.find("ct_serial_probe").is_none());
    assert_eq!(
        protocol::validate_request(&envelope, &health),
        Err(WorkerErrorKind::InvalidRequest),
    );
    // Admission rejects CT before platform dispatch, including on Windows.
    let error = probe(
        &envelope.request,
        Duration::from_secs(3),
        &AtomicBool::new(false),
        &health,
    )
    .unwrap_err();
    assert_eq!(error.kind, WorkerErrorKind::InvalidRequest);
    assert_eq!(error.worker, None);
    assert!(combined.find("health").is_some());
    assert!(combined.find("ct_serial_probe").is_some());
    protocol::validate_request(&envelope, &combined).unwrap();
}

#[test]
fn production_ct_callback_is_allowlisted_and_invalid_payload_is_not_success_shaped() {
    let registry = crate::cli::diagnostic_allowlist().unwrap();
    let callback = registry
        .find("ct_serial_probe")
        .expect("static CT callback");
    let request = RequestEnvelope {
        version: VERSION,
        request_nonce: nonce(),
        request: DiagnosticRequest {
            kind: "ct_serial_probe".into(),
            payload: json!({"port": "COM1"}),
        },
    };
    protocol::validate_request(&request, &registry).unwrap();
    // Invalid options stop before containment checks or native device access.
    let outcome = protocol::invoke_callback(callback, request.request.payload);
    assert!(matches!(outcome, Outcome::CallbackFailed));
    assert_eq!(
        serde_json::to_value(outcome).unwrap(),
        json!({"status": "callback_failed"})
    );
    assert!(registry.find("command").is_none());
}

#[test]
fn hid_registries_have_kind_only_authority_and_named_route_is_private() {
    let health = crate::cli::health_allowlist().unwrap();
    let micro = crate::cli::micro_probe_allowlist().unwrap();
    let akp03 = crate::cli::akp03_probe_allowlist().unwrap();
    let combined = crate::cli::diagnostic_allowlist().unwrap();
    assert_eq!(micro.route, DiagnosticWorkerRoute::Shared);
    assert_eq!(akp03.route, DiagnosticWorkerRoute::Akp03);
    for (registry, accepted) in [
        (&health, "health"),
        (&micro, "micro_hid_probe"),
        (&akp03, "akp03_hid_probe"),
    ] {
        for kind in [
            "health",
            "ct_serial_probe",
            "micro_hid_probe",
            "akp03_hid_probe",
        ] {
            let envelope = RequestEnvelope {
                version: VERSION,
                request_nonce: nonce(),
                request: DiagnosticRequest {
                    kind: kind.into(),
                    payload: Value::Null,
                },
            };
            assert_eq!(registry.find(kind).is_some(), kind == accepted);
            if kind == accepted {
                protocol::validate_request(&envelope, registry).unwrap();
            } else {
                assert_eq!(
                    protocol::validate_request(&envelope, registry),
                    Err(WorkerErrorKind::InvalidRequest),
                );
                // Wrong-kind rejection happens before any platform/worker I/O.
                let error = probe(
                    &envelope.request,
                    Duration::from_secs(3),
                    &AtomicBool::new(false),
                    registry,
                )
                .unwrap_err();
                assert_eq!(error.kind, WorkerErrorKind::InvalidRequest);
                assert_eq!(error.worker, None);
            }
        }
    }
    for kind in ["micro_hid_probe", "akp03_hid_probe"] {
        let callback = combined.find(kind).expect("compiled HID callback");
        let outcome = protocol::invoke_callback(callback, json!({"command": "private"}));
        assert!(matches!(outcome, Outcome::CallbackFailed));
    }
    assert!(combined.find("command").is_none());
    assert!(maybe_run_hidden_worker([HIDDEN_WORKER_ARGUMENT], &akp03).is_none());
    assert!(maybe_run_hidden_worker([AKP03_HIDDEN_WORKER_ARGUMENT], &combined).is_none());
    let error = maybe_run_hidden_worker([AKP03_HIDDEN_WORKER_ARGUMENT, "extra"], &akp03)
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind, WorkerErrorKind::InvalidRequest);
}

#[test]
fn framed_hid_options_reject_duplicate_keys_before_callback_admission() {
    for body in [
        br#"{"version":1,"request_nonce":"11111111-1111-4111-8111-111111111111","request":{"kind":"micro_hid_probe","payload":{"capture_ms":0,"query":false,"query":true,"read_keymap":false,"test_pattern":false,"health_check":false}}}"#.as_slice(),
        br#"{"version":1,"request_nonce":"11111111-1111-4111-8111-111111111111","request":{"kind":"akp03_hid_probe","payload":{"capture_ms":0,"initialize":false,"initialize":true,"test_pattern":false,"health_check":false}}}"#.as_slice(),
    ] {
        assert!(matches!(
            protocol::read_frame::<_, RequestEnvelope>(&mut Cursor::new(frame(body))),
            Err(WorkerErrorKind::Protocol),
        ));
    }
}

#[test]
fn frame_limits_and_truncation_are_distinct() {
    for length in [0_u32, 65_537, u32::MAX] {
        assert_eq!(
            protocol::read_frame::<_, Value>(&mut Cursor::new(length.to_le_bytes())),
            Err(WorkerErrorKind::FrameLimit)
        );
    }
    assert_eq!(
        protocol::read_frame::<_, Value>(&mut Cursor::new(vec![1, 0])),
        Err(WorkerErrorKind::Transport)
    );
    assert_eq!(
        protocol::read_frame::<_, Value>(&mut Cursor::new(
            [&4_u32.to_le_bytes()[..], b"{"].concat()
        )),
        Err(WorkerErrorKind::Transport)
    );
    assert_eq!(
        protocol::read_frame::<_, Value>(&mut Cursor::new(frame(b"{"))),
        Err(WorkerErrorKind::Protocol)
    );
    assert_eq!(
        protocol::read_frame::<_, Value>(&mut Cursor::new(b"not-a-frame")),
        Err(WorkerErrorKind::FrameLimit)
    );
}

#[test]
fn frame_rejects_all_trailing_bytes_and_tail_io_errors() {
    for tail in [vec![0], vec![b' '], frame(b"null")] {
        let mut bytes = frame(b"{}");
        bytes.extend(tail);
        assert_eq!(
            protocol::read_frame::<_, Value>(&mut Cursor::new(bytes)),
            Err(WorkerErrorKind::Protocol)
        );
    }
    struct TailError(Cursor<Vec<u8>>);
    impl Read for TailError {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.0.position() as usize == self.0.get_ref().len() {
                Err(io::Error::other("private native detail"))
            } else {
                self.0.read(buffer)
            }
        }
    }
    assert_eq!(
        protocol::read_frame::<_, Value>(&mut TailError(Cursor::new(frame(b"{}")))),
        Err(WorkerErrorKind::Transport)
    );
}

#[test]
fn stderr_is_discarded_with_an_inclusive_limit() {
    for (length, expected) in [
        (0, Ok(())),
        (MAX_DIAGNOSTIC_BYTES, Ok(())),
        (
            MAX_DIAGNOSTIC_BYTES + 1,
            Err(WorkerErrorKind::DiagnosticLimit),
        ),
    ] {
        assert_eq!(
            protocol::drain_diagnostics(&mut OneByte(Cursor::new(vec![b'x'; length]))),
            expected
        );
    }
    struct ErrorReader;
    impl Read for ErrorReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("private stderr failure"))
        }
    }
    assert_eq!(
        protocol::drain_diagnostics(&mut ErrorReader),
        Err(WorkerErrorKind::Transport)
    );
}

#[test]
fn encoding_is_bounded_including_the_entire_envelope() {
    let exact = Value::String("x".repeat(MAX_FRAME_BODY_BYTES - 2));
    let encoded = protocol::encode_frame(&exact).unwrap();
    assert_eq!(encoded.len(), MAX_FRAME_BODY_BYTES + 4);
    assert_eq!(
        protocol::read_frame::<_, Value>(&mut Cursor::new(encoded)).unwrap(),
        exact
    );
    assert_eq!(
        protocol::encode_frame(&Value::String("x".repeat(MAX_FRAME_BODY_BYTES - 1))),
        Err(WorkerErrorKind::FrameLimit)
    );
    let oversized_request = DiagnosticRequest {
        kind: "health".to_owned(),
        payload: Value::String("x".repeat(MAX_FRAME_BODY_BYTES)),
    };
    assert_eq!(
        protocol::encode_frame(&RequestEnvelopeRef {
            version: VERSION,
            request_nonce: nonce(),
            request: &oversized_request,
        }),
        Err(WorkerErrorKind::FrameLimit)
    );

    struct BrokenSerializer;
    impl Serialize for BrokenSerializer {
        fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(S::Error::custom("private serializer failure"))
        }
    }
    assert_eq!(
        protocol::encode_frame(&BrokenSerializer),
        Err(WorkerErrorKind::FrameLimit)
    );
}

#[test]
fn requests_have_strict_schema_and_ascii_kind_rules() {
    for valid in ["a", "1", ".", "_1.-", &"z".repeat(64)] {
        assert!(protocol::valid_kind(valid), "{valid}");
    }
    for invalid in [
        "",
        "Windows",
        "health --command",
        "health/child",
        "with space",
        "é",
        &"z".repeat(65),
    ] {
        assert!(!protocol::valid_kind(invalid), "{invalid}");
        let error = probe(
            &request(invalid),
            Duration::from_secs(1),
            &AtomicBool::new(false),
            &allowlist(),
        )
        .unwrap_err();
        assert_eq!(error.kind, WorkerErrorKind::InvalidRequest);
        assert!(error.replacement_allowed);
        assert_eq!(error.worker, None);
    }
    assert!(
        serde_json::from_value::<DiagnosticRequest>(
            json!({"kind": "health", "payload": {}, "executable": "anything"})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<DiagnosticRequest>(json!({"kind": "health"})).is_err());
    assert!(
        serde_json::from_value::<RequestEnvelope>(json!({
            "version": 1, "request_nonce": nonce(), "request": request("health"),
            "executable": "anything"
        }))
        .is_err()
    );

    let mut envelope = RequestEnvelope {
        version: VERSION,
        request_nonce: nonce(),
        request: request("health"),
    };
    assert!(protocol::validate_request(&envelope, &allowlist()).is_ok());
    envelope.version = 2;
    assert_eq!(
        protocol::validate_request(&envelope, &allowlist()),
        Err(WorkerErrorKind::InvalidRequest)
    );
    envelope.version = VERSION;
    envelope.request_nonce = Uuid::nil();
    assert_eq!(
        protocol::validate_request(&envelope, &allowlist()),
        Err(WorkerErrorKind::InvalidRequest)
    );
    envelope.request_nonce = nonce();
    envelope.request.kind = "not_allowlisted".to_owned();
    assert_eq!(
        protocol::validate_request(&envelope, &allowlist()),
        Err(WorkerErrorKind::InvalidRequest)
    );

    static BAD: [DiagnosticCallback; 1] = [DiagnosticCallback::new("bad kind", test_callback)];
    static DUPLICATE: [DiagnosticCallback; 2] = [
        DiagnosticCallback::new("health", test_callback),
        DiagnosticCallback::new("health", test_callback),
    ];
    for entries in [&BAD[..], &DUPLICATE[..]] {
        assert_eq!(
            DiagnosticAllowlist::new(entries).err().unwrap().kind,
            WorkerErrorKind::InvalidRequest
        );
    }
}

#[test]
fn response_requires_exact_nonce_version_and_retained_lifetime() {
    assert!(protocol::validate_response(response(), nonce(), worker()).is_ok());
    let mut changed = response();
    changed.version += 1;
    assert!(matches!(
        protocol::validate_response(changed, nonce(), worker()),
        Err(WorkerErrorKind::Protocol)
    ));
    let mut changed = response();
    changed.request_nonce = Uuid::from_u128(43);
    assert!(matches!(
        protocol::validate_response(changed, nonce(), worker()),
        Err(WorkerErrorKind::Protocol)
    ));
    let mut changed = response();
    changed.worker.pid += 1;
    assert!(matches!(
        protocol::validate_response(changed, nonce(), worker()),
        Err(WorkerErrorKind::Protocol)
    ));
    let mut changed = response();
    changed.worker.creation_time_100ns += 1;
    assert!(matches!(
        protocol::validate_response(changed, nonce(), worker()),
        Err(WorkerErrorKind::Protocol)
    ));
}

#[test]
fn response_and_worker_are_strict_but_outcome_is_not_blanket_strict() {
    let base = json!({
        "version": 1, "request_nonce": nonce(), "worker": worker(),
        "outcome": {"status": "ok", "payload": {}, "ignored": "allowed"}
    });
    assert!(serde_json::from_value::<ResponseEnvelope>(base.clone()).is_ok());
    let mut extra_envelope = base.clone();
    extra_envelope["unexpected"] = json!(true);
    assert!(serde_json::from_value::<ResponseEnvelope>(extra_envelope).is_err());
    let mut extra_worker = base.clone();
    extra_worker["worker"]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<ResponseEnvelope>(extra_worker).is_err());
    let mut unknown_status = base.clone();
    unknown_status["outcome"]["status"] = json!("unknown");
    assert!(serde_json::from_value::<ResponseEnvelope>(unknown_status).is_err());
    let mut callback_failed = base;
    callback_failed["outcome"] = json!({"status": "callback_failed", "ignored": 1});
    assert!(serde_json::from_value::<ResponseEnvelope>(callback_failed).is_ok());
}

#[test]
fn timeout_and_prelaunch_cancellation_precede_request_validation() {
    let cancelled = AtomicBool::new(true);
    for timeout in [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_secs(120) + Duration::from_nanos(1),
    ] {
        let error = probe(
            &request("health --command"),
            timeout,
            &cancelled,
            &allowlist(),
        )
        .err()
        .unwrap();
        assert_eq!(error.kind, WorkerErrorKind::InvalidTimeout);
        assert!(error.replacement_allowed);
        assert_eq!(error.worker, None);
    }
    let error = probe(
        &request("health --command"),
        Duration::from_millis(1),
        &cancelled,
        &allowlist(),
    )
    .err()
    .unwrap();
    assert_eq!(error.kind, WorkerErrorKind::Cancelled);
    assert!(error.replacement_allowed);
    assert_eq!(error.worker, None);

    cancelled.store(false, Ordering::Release);
    let error = probe(
        &request("health --command"),
        Duration::from_millis(1),
        &cancelled,
        &allowlist(),
    )
    .err()
    .unwrap();
    assert_eq!(error.kind, WorkerErrorKind::InvalidRequest);
    assert!(error.replacement_allowed);
}

#[test]
fn oversized_request_fails_before_platform_dispatch() {
    let oversized = DiagnosticRequest {
        kind: "health".to_owned(),
        payload: Value::String("x".repeat(MAX_FRAME_BODY_BYTES)),
    };
    let error = probe(
        &oversized,
        Duration::from_secs(1),
        &AtomicBool::new(false),
        &allowlist(),
    )
    .err()
    .unwrap();
    assert_eq!(error.kind, WorkerErrorKind::FrameLimit);
    assert_eq!(error.worker, None);
    assert!(error.replacement_allowed);
}

#[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
#[test]
fn unsupported_platform_does_not_invoke_callbacks() {
    CALLBACK_COUNT.store(0, Ordering::SeqCst);
    for (kind, timeout) in [
        ("windows_process", Duration::from_millis(1)),
        ("_1.-", Duration::from_secs(120)),
    ] {
        let error = probe(
            &request(kind),
            timeout,
            &AtomicBool::new(false),
            &allowlist(),
        )
        .err()
        .unwrap();
        assert_eq!(error.kind, WorkerErrorKind::UnsupportedPlatform);
        assert_eq!(error.worker, None);
        assert!(error.replacement_allowed);
    }
    let worker_result = maybe_run_hidden_worker([HIDDEN_WORKER_ARGUMENT], &allowlist())
        .unwrap()
        .unwrap_err();
    assert_eq!(worker_result.kind, WorkerErrorKind::UnsupportedPlatform);
    assert_eq!(CALLBACK_COUNT.load(Ordering::SeqCst), 0);
}

#[test]
fn hidden_route_requires_exactly_one_private_argument() {
    assert!(maybe_run_hidden_worker(["public-command"], &allowlist()).is_none());
    assert!(maybe_run_hidden_worker(std::iter::empty::<&str>(), &allowlist()).is_none());
    let error = maybe_run_hidden_worker([HIDDEN_WORKER_ARGUMENT, "extra"], &allowlist())
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind, WorkerErrorKind::InvalidRequest);
}

#[test]
fn callback_failures_and_panics_expose_only_generic_outcomes() {
    for callback in [
        failing_callback,
        panicking_callback,
        panicking_payload_callback,
    ] {
        let outcome = protocol::invoke_callback(callback, json!("private payload"));
        let encoded = serde_json::to_string(&outcome).unwrap();
        assert_eq!(encoded, r#"{"status":"callback_failed"}"#);
        assert!(!encoded.contains("private"));
    }
    let error = WorkerError::after_launch(WorkerErrorKind::Callback, Some(worker()), true, true);
    assert_eq!(error.to_string(), "diagnostic worker: Callback");
    assert!(!format!("{error:?}").contains("private"));
}

#[test]
fn completion_waits_for_every_pipe_and_exit_and_preserves_failures() {
    let mut progress = Progress {
        exit: Some(Ok(7)),
        ..Progress::default()
    };
    assert!(progress.decide(false, false).is_none());
    progress.send = Some(Ok(()));
    progress.response = Some(Err(WorkerErrorKind::Protocol));
    assert_eq!(
        progress.decide(false, false).unwrap(),
        Err(WorkerErrorKind::Protocol)
    );
    let mut progress = Progress {
        send: Some(Ok(())),
        response: Some(Ok(Outcome::Ok { payload: json!(42) })),
        stderr: None,
        exit: Some(Ok(7)),
    };
    assert!(progress.decide(false, false).is_none());
    progress.stderr = Some(Ok(()));
    assert_eq!(
        progress.decide(false, false).unwrap(),
        Err(WorkerErrorKind::WorkerFailed)
    );
    let mut progress = Progress {
        send: Some(Ok(())),
        response: Some(Ok(Outcome::CallbackFailed)),
        stderr: Some(Ok(())),
        exit: Some(Ok(0)),
    };
    assert_eq!(
        progress.decide(false, false).unwrap(),
        Err(WorkerErrorKind::Callback)
    );
    let mut progress = Progress {
        send: Some(Ok(())),
        response: Some(Ok(Outcome::Ok { payload: json!(42) })),
        stderr: Some(Ok(())),
        exit: Some(Ok(0)),
    };
    assert_eq!(progress.decide(false, false).unwrap(), Ok(json!(42)));
}

#[test]
fn running_cancellation_wins_over_deadline() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&cancelled);
    let thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        signal.store(true, Ordering::Release);
    });
    let started = Instant::now();
    let mut progress = Progress::default();
    let result = loop {
        if let Some(result) = progress.decide(
            cancelled.load(Ordering::Acquire),
            started.elapsed() > Duration::from_secs(1),
        ) {
            break result;
        }
        thread::sleep(Duration::from_millis(1));
    };
    thread.join().unwrap();
    assert_eq!(result, Err(WorkerErrorKind::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        Progress::default().decide(true, true).unwrap(),
        Err(WorkerErrorKind::Cancelled)
    );
    assert_eq!(
        Progress::default().decide(false, true).unwrap(),
        Err(WorkerErrorKind::TimedOut)
    );
}

#[test]
fn active_poisoned_and_sticky_fences_fail_closed() {
    let gate = SupervisorGate::new();
    let active = gate.acquire().unwrap();
    assert!(matches!(gate.acquire(), Err(WorkerErrorKind::Busy)));
    gate.block();
    drop(active);
    assert!(matches!(
        gate.acquire(),
        Err(WorkerErrorKind::SupervisionBlocked)
    ));

    let poisoned = Arc::new(SupervisorGate::new());
    let other = Arc::clone(&poisoned);
    let _ = thread::spawn(move || {
        let _active = other.acquire().unwrap();
        panic!("poison test");
    })
    .join();
    assert!(matches!(
        poisoned.acquire(),
        Err(WorkerErrorKind::SupervisionBlocked)
    ));
}

#[test]
fn cleanup_uncertainty_overrides_error_and_forbids_replacement() {
    for (exited, joined) in [(false, false), (false, true), (true, false)] {
        let error =
            WorkerError::after_launch(WorkerErrorKind::Cancelled, Some(worker()), exited, joined);
        assert_eq!(error.kind, WorkerErrorKind::CleanupUnconfirmed);
        assert!(!error.replacement_allowed);
        assert_eq!(error.worker, Some(worker()));
    }
    let error = WorkerError::after_launch(WorkerErrorKind::TimedOut, Some(worker()), true, true);
    assert_eq!(error.kind, WorkerErrorKind::TimedOut);
    assert!(error.replacement_allowed);
    for kind in [WorkerErrorKind::Busy, WorkerErrorKind::SupervisionBlocked] {
        assert!(!WorkerError::before_launch(kind).replacement_allowed);
    }
}
