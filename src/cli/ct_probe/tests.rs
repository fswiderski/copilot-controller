use std::{cell::Cell, collections::VecDeque};

use super::*;
use probe::{
    ProbeErrorKind, ReadResult, SerialSettings, StaticLineMode, Transport, TransportError,
};

fn callback_with(
    payload: Value,
    native: impl FnOnce(Options, &AtomicBool) -> Result<ProbeReport, ProbeFailure>,
) -> Result<Value, worker::DiagnosticCallbackError> {
    super::callback_with(payload, || Ok(()), native)
}

struct FakeTransport {
    reads: VecDeque<Vec<u8>>,
    now: u64,
    fail: bool,
}

impl Transport for FakeTransport {
    fn now_ms(&mut self) -> u64 {
        self.now
    }

    fn write(&mut self, bytes: &[u8], _: u64, _: &AtomicBool) -> Result<usize, TransportError> {
        if self.fail {
            return Err(TransportError {
                kind: ProbeErrorKind::BusyOrDenied,
                native_code: Some(5),
            });
        }
        Ok(bytes.len())
    }

    fn read(
        &mut self,
        buffer: &mut [u8],
        _: u64,
        _: &AtomicBool,
    ) -> Result<ReadResult, TransportError> {
        if let Some(bytes) = self.reads.pop_front() {
            buffer[..bytes.len()].copy_from_slice(&bytes);
            Ok(ReadResult::Data(bytes.len()))
        } else {
            self.now += 1;
            Ok(ReadResult::Idle)
        }
    }

    fn serial_settings(&self) -> Option<SerialSettings> {
        Some(SerialSettings {
            baud_rate: 256_000,
            data_bits: 8,
            parity: "none".into(),
            stop_bits: 1,
            binary: true,
            flow_control: "none".into(),
            nul_stripping: false,
            dtr: StaticLineMode::Off,
            rts: StaticLineMode::On,
        })
    }
}

struct PreparedTransport {
    reads: VecDeque<Vec<u8>>,
    now: u64,
    writes: u32,
    fail_write_at: Option<u32>,
    phases: Vec<probe::PhaseProgress>,
}

impl Transport for PreparedTransport {
    fn now_ms(&mut self) -> u64 {
        self.now
    }

    fn write(&mut self, bytes: &[u8], _: u64, _: &AtomicBool) -> Result<usize, TransportError> {
        self.writes = self.writes.saturating_add(1);
        if self.fail_write_at == Some(self.writes) {
            return Err(TransportError {
                kind: ProbeErrorKind::NativeFailure,
                native_code: Some(22),
            });
        }
        Ok(bytes.len())
    }

    fn read(
        &mut self,
        buffer: &mut [u8],
        _: u64,
        _: &AtomicBool,
    ) -> Result<ReadResult, TransportError> {
        if let Some(bytes) = self.reads.pop_front() {
            buffer[..bytes.len()].copy_from_slice(&bytes);
            Ok(ReadResult::Data(bytes.len()))
        } else {
            self.now += 1;
            Ok(ReadResult::Idle)
        }
    }

    fn progress_origin_ms(&self) -> Option<u64> {
        Some(0)
    }

    fn initial_phases(&self) -> &[probe::PhaseProgress] {
        &self.phases
    }

    fn port_opened_at_ms(&self) -> Option<u32> {
        Some(18)
    }

    fn port_configured_at_ms(&self) -> Option<u32> {
        Some(36)
    }

    fn serial_settings(&self) -> Option<SerialSettings> {
        Some(SerialSettings {
            baud_rate: 256_000,
            data_bits: 8,
            parity: "none".into(),
            stop_bits: 1,
            binary: true,
            flow_control: "none".into(),
            nul_stripping: false,
            dtr: StaticLineMode::Off,
            rts: StaticLineMode::Off,
        })
    }
}

fn fake_native(
    options: Options,
    cancelled: &AtomicBool,
    fail: bool,
) -> Result<ProbeReport, ProbeFailure> {
    let mut reads = VecDeque::from([
        b"HTTP/1.1 101 Switching Protocols\nUpgrade: websocket\nConnection: Upgrade\n\n".to_vec(),
        vec![0x82, 4, 4, 3, 1, b'C'],
        vec![0x82, 6, 6, 7, 2, 1, 2, 3],
    ]);
    if options.test_pattern {
        for (opcode, transaction) in [(0x10, 3), (0x0f, 4), (0x10, 5), (0x0f, 6)] {
            reads.push_back(vec![0x82, 3, 3, opcode, transaction]);
        }
    }
    reads.push_back(vec![0x82, 5, 5, 0, 1, 0, 1]);
    let fake = FakeTransport {
        reads,
        now: 0,
        fail,
    };
    // Only the injected protocol engine runs. Mark its fixture as native evidence
    // to exercise coordinator validation; this is not a native/hardware test.
    match probe::run(options, fake, cancelled) {
        Ok(mut report) => {
            report.evidence = Evidence::NativeTransportObservation;
            report.progress.evidence = Evidence::NativeTransportObservation;
            Ok(report)
        }
        Err(mut error) => {
            error.progress.evidence = Evidence::NativeTransportObservation;
            Err(error)
        }
    }
}

fn prepared_native(
    options: Options,
    cancelled: &AtomicBool,
    fail_write_at: Option<u32>,
) -> Result<ProbeReport, ProbeFailure> {
    let transport = PreparedTransport {
        reads: VecDeque::from([
            b"HTTP/1.1 101 Switching Protocols\nUpgrade: websocket\nConnection: keep-alive, Upgrade\n\n".to_vec(),
            vec![0x82, 4, 4, 3, 1, b'C'],
            vec![0x82, 6, 6, 7, 2, 1, 2, 3],
            vec![0x82, 5, 5, 0, 1, 0, 1],
        ]),
        now: 100,
        writes: 0,
        fail_write_at,
        phases: vec![
            probe::PhaseProgress {
                phase: probe::Phase::Discovery,
                started_ms: 12,
                elapsed_ms: 3,
            },
            probe::PhaseProgress {
                phase: probe::Phase::Open,
                started_ms: 18,
                elapsed_ms: 6,
            },
            probe::PhaseProgress {
                phase: probe::Phase::AssociationCheck,
                started_ms: 26,
                elapsed_ms: 4,
            },
            probe::PhaseProgress {
                phase: probe::Phase::Configure,
                started_ms: 36,
                elapsed_ms: 9,
            },
        ],
    };
    match probe::run(options, transport, cancelled) {
        Ok(mut report) => {
            report.evidence = Evidence::NativeTransportObservation;
            report.progress.evidence = Evidence::NativeTransportObservation;
            Ok(report)
        }
        Err(mut error) => {
            error.progress.evidence = Evidence::NativeTransportObservation;
            Err(error)
        }
    }
}

fn worker_result(payload: Value) -> worker::DiagnosticResult {
    worker::DiagnosticResult {
        worker: worker::WorkerIdentity {
            pid: 7,
            creation_time_100ns: 42,
        },
        payload,
    }
}

fn payload(options: &Options, fail: bool) -> Value {
    callback_with(
        serde_json::to_value(options).unwrap(),
        |options, cancelled| fake_native(options, cancelled, fail),
    )
    .unwrap()
}

#[test]
fn invalid_cli_options_never_reach_worker() {
    let cases: &[&[&str]] = &[
        &["--port", "COM1"],
        &["--command", "anything"],
        &["--path", "anything"],
        &["--selector", "anything"],
        &["--label", "anything"],
        &["--schema_version", "1"],
        &["--schema-version", "2"],
        &["--schema-version", "257"],
        &["--total-timeout-ms", "999"],
        &["--total-timeout-ms", "8001"],
        &["--io-timeout-ms", "99"],
        &["--io-timeout-ms", "1501"],
        &[
            "--total-timeout-ms",
            "1000",
            "--io-timeout-ms",
            "1001",
            "--capture-ms",
            "0",
        ],
        &["--capture-ms", "5001"],
        &["--total-timeout-ms", "1000", "--capture-ms", "1001"],
        &["--max-events", "0"],
        &["--max-events", "129"],
        &["--max-events", "65537"],
        &["--max-received-bytes", "255"],
        &["--max-received-bytes", "32769"],
        &["--capture-ms", "4294967296"],
        &["--capture-ms", "-1"],
        &["--capture-ms", "+1"],
        &["--capture-ms", "1.0"],
        &["--capture-ms", "NaN"],
        &["--capture-ms", " 1"],
        &["--capture-ms", ""],
        &["--capture-ms"],
        &["--test-pattern", "false"],
        &["--test-pattern", "--test-pattern"],
        &["--capture-ms", "0", "--capture-ms", "1"],
        &["--capture-ms", "0", "--unknown"],
    ];
    let launches = Cell::new(0);
    for case in cases {
        let args: Vec<_> = std::iter::once("ct-probe")
            .chain(case.iter().copied())
            .map(str::to_owned)
            .collect();
        let result = crate::cli::parse(&args).and_then(|command| {
            let crate::cli::Command::CtProbe(options) = command else {
                panic!("wrong route");
            };
            execute_with(options, |_, _| {
                launches.set(launches.get() + 1);
                panic!("invalid CLI reached worker");
            })
        });
        let error: Value = serde_json::from_str(&result.err().unwrap()).unwrap();
        assert_eq!(error["error"]["kind"], "invalid_options", "{case:?}");
        assert_eq!(error["error"]["stage"], "options");
    }
    assert_eq!(launches.get(), 0);
    let result = execute_with(
        Options {
            schema_version: 0,
            ..Options::default()
        },
        |_, _| panic!("unvalidated programmatic options reached worker"),
    );
    assert!(result.is_err());
}

#[test]
fn complete_cli_options_and_defaults_have_bounded_supervision() {
    assert_eq!(parse(&[]).unwrap(), Options::default());
    for (args, expected) in [
        (
            vec![
                "--schema-version",
                "1",
                "--total-timeout-ms",
                "1000",
                "--io-timeout-ms",
                "100",
                "--capture-ms",
                "0",
                "--max-events",
                "1",
                "--max-received-bytes",
                "256",
                "--test-pattern",
            ],
            Options {
                schema_version: 1,
                total_timeout_ms: 1000,
                io_timeout_ms: 100,
                capture_ms: 0,
                max_events: 1,
                max_received_bytes: 256,
                test_pattern: true,
            },
        ),
        (
            vec![
                "--total-timeout-ms",
                "8000",
                "--io-timeout-ms",
                "1500",
                "--capture-ms",
                "5000",
                "--max-events",
                "128",
                "--max-received-bytes",
                "32768",
            ],
            Options {
                io_timeout_ms: 1500,
                capture_ms: 5000,
                ..Options::default()
            },
        ),
    ] {
        let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
        let options = parse(&args).unwrap();
        assert_eq!(options, expected);
        let result = execute_with(options.clone(), |request, budget| {
            assert_eq!(request.kind, KIND);
            assert_eq!(request.payload, serde_json::to_value(&expected).unwrap());
            assert_eq!(
                budget,
                Duration::from_millis(
                    u64::from(expected.total_timeout_ms)
                        + 2_000
                        + u64::from(expected.io_timeout_ms.min(250))
                ),
            );
            assert!(budget <= Duration::from_millis(10_250));
            Err(protocol_error())
        });
        assert!(result.is_err());
    }
}

#[test]
fn callback_rejects_untyped_or_invalid_options_before_native() {
    for value in [
        Value::Null,
        json!([]),
        json!("private"),
        json!(1),
        json!({"port": "COM9"}),
        json!({"command": "private"}),
        json!({"path": "private"}),
        json!({"selector": "private"}),
        json!({"schema_version": 2}),
        json!({"capture_ms": 8001}),
        json!({"max_events": 0}),
        json!({"test_pattern": "true"}),
        json!({"io_timeout_ms": 1.0}),
        json!({"max_received_bytes": null}),
    ] {
        assert!(callback_with(value, |_, _| panic!("invalid payload reached native")).is_err());
    }
    assert!(
        super::callback_with(
            json!({"schema_version": 0}),
            || panic!("invalid options reached containment check"),
            |_, _| panic!("invalid options reached native"),
        )
        .is_err()
    );
    assert!(
        super::callback_with(
            json!({}),
            || Err(worker::DiagnosticCallbackError),
            |_, _| panic!("uncontained callback reached native"),
        )
        .is_err()
    );
}

#[test]
fn typed_native_report_and_failure_round_trip_without_success_on_failure() {
    let options = Options {
        capture_ms: 0,
        ..Options::default()
    };
    let success = payload(&options, false);
    let output = execute_with(options.clone(), |_, _| Ok(worker_result(success))).unwrap();
    let value: Value = serde_json::from_str(&output.render().unwrap()).unwrap();
    assert_eq!(value["command_ready"], false);
    assert_eq!(
        value["ct_probe"]["evidence"],
        "native_transport_observation"
    );
    assert_eq!(value["ct_probe"]["hardware_qualification"], "unqualified");
    assert_eq!(value["ct_probe"]["pattern"], "not_requested");
    assert_eq!(value["ct_probe"]["firmware_version"], json!([1, 2, 3]));
    let failure = payload(&options, true);
    assert_eq!(failure["status"], "probe_error");
    assert!(failure.get("report").is_none());
    let result = execute_with(options, |_, _| Ok(worker_result(failure.clone())));
    let error: Value = serde_json::from_str(&result.err().unwrap()).unwrap();
    assert_eq!(error, failure);
    assert_eq!(error["error"]["kind"], "busy_or_denied");
    assert_eq!(error["error"]["stage"], "handshake");
    assert_eq!(error["error"]["native_code"], 5);
}

#[test]
fn worker_failures_propagate_without_success_payload() {
    for kind in [
        worker::WorkerErrorKind::UnsupportedPlatform,
        worker::WorkerErrorKind::Callback,
        worker::WorkerErrorKind::TimedOut,
        worker::WorkerErrorKind::CleanupUnconfirmed,
    ] {
        let failure = worker::WorkerError {
            kind,
            worker: Some(worker_result(Value::Null).worker),
            replacement_allowed: kind != worker::WorkerErrorKind::CleanupUnconfirmed,
        };
        let result = execute_with(Options::default(), |_, _| Err(failure.clone()));
        let value: Value = serde_json::from_str(&result.err().unwrap()).unwrap();
        assert_eq!(value["status"], "worker_error");
        assert_eq!(value["error"], serde_json::to_value(failure).unwrap());
        assert!(value.get("ct_probe").is_none());
    }
}

#[test]
fn valid_input_and_explicit_pattern_round_trip_with_fake_transport_only() {
    let options = Options {
        capture_ms: 2,
        test_pattern: true,
        ..Options::default()
    };
    let report = payload(&options, false);
    assert_eq!(report["report"]["pattern"], "correlated_replies_only");
    assert_eq!(
        report["report"]["inputs"][0]["payload"],
        json!([5, 0, 1, 0, 1])
    );
    assert_eq!(report["report"]["progress"]["frames_received"], 7);
    let output = execute_with(options, |_, _| Ok(worker_result(report))).unwrap();
    let value: Value = serde_json::from_str(&output.render().unwrap()).unwrap();
    assert_eq!(value["command_ready"], false);
    assert_eq!(
        value["ct_probe"]["physical_input_qualification"],
        "unqualified"
    );
    assert_eq!(value["ct_probe"]["visual_acceptance"], "unqualified");
}

#[test]
fn forged_or_private_worker_payloads_never_become_cli_output() {
    let options = Options {
        capture_ms: 0,
        max_events: 1,
        ..Options::default()
    };
    let valid = payload(&options, false);
    let mut cases = vec![
        Value::Null,
        json!({}),
        json!({"status": "ok"}),
        json!({"status": "probe_error", "error": "private"}),
    ];
    for (pointer, replacement) in [
        ("/report/hardware_qualification", json!("qualified")),
        ("/report/evidence", json!("synthetic")),
        ("/report/schema_version", json!(2)),
        ("/report/http_101", json!(false)),
        ("/report/pattern", json!("private")),
        ("/report/identity_response_length", json!(65)),
        ("/report/serial_settings/parity", json!("private")),
        ("/report/serial_settings/flow_control", json!("private")),
        (
            "/report/progress/handshake_response_start",
            json!("private raw header"),
        ),
        ("/report/progress/phases", json!([])),
        ("/report/elapsed_ms", json!(11_000)),
        ("/report/received_bytes", json!(32_769)),
        (
            "/report/inputs",
            json!([{"elapsed_ms": 0, "payload": [4,3,1,67]}]),
        ),
        (
            "/report/inputs",
            json!([{"elapsed_ms": 0, "payload": [5,0,1,0,1]}, {"elapsed_ms": 0, "payload": [5,0,1,0,1]}]),
        ),
    ] {
        let mut value = valid.clone();
        *value.pointer_mut(pointer).unwrap() = replacement;
        cases.push(value);
    }
    let mut unknown = valid.clone();
    unknown["report"]["port"] = json!("private");
    cases.push(unknown);
    let mut unknown = valid.clone();
    unknown["report"]["progress"]["raw_identity"] = json!("private");
    cases.push(unknown);
    let mut unknown = payload(&options, true);
    unknown["error"]["raw_os_text"] = json!("private");
    cases.push(unknown);
    for value in cases {
        let result = execute_with(options.clone(), |_, _| Ok(worker_result(value)));
        let text = result.err().expect("forged payload became success");
        assert!(!text.contains("private"));
        let error: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(error["status"], "worker_error");
        assert_eq!(error["error"]["kind"], "protocol");
    }
    let mut private = fake_native(options.clone(), &AtomicBool::new(false), false).unwrap();
    private.progress.handshake_response_start = Some("private".into());
    assert!(callback_with(serde_json::to_value(options).unwrap(), |_, _| Ok(private)).is_err());
}

#[test]
fn callback_accepts_seeded_progress_with_origin_zero_and_nonzero_configure_start() {
    let options = Options {
        capture_ms: 0,
        ..Options::default()
    };
    let payload = callback_with(
        serde_json::to_value(&options).unwrap(),
        |opts, cancelled| prepared_native(opts, cancelled, None),
    )
    .expect("seeded report should remain valid");
    assert_eq!(payload["status"], "report");
    let elapsed = payload["report"]["progress"]["elapsed_ms"]
        .as_u64()
        .unwrap();
    for phase in payload["report"]["progress"]["phases"].as_array().unwrap() {
        let started = phase["started_ms"].as_u64().unwrap();
        let duration = phase["elapsed_ms"].as_u64().unwrap();
        assert!(
            started.saturating_add(duration) <= elapsed,
            "phase interval exceeded elapsed"
        );
    }
}

#[test]
fn callback_returns_typed_probe_error_when_seeded_transport_fails_after_progress_seed() {
    let options = Options {
        capture_ms: 0,
        ..Options::default()
    };
    let payload = callback_with(
        serde_json::to_value(&options).unwrap(),
        |opts, cancelled| prepared_native(opts, cancelled, Some(1)),
    )
    .expect("typed probe error should be accepted");
    assert_eq!(payload["status"], "probe_error");
    assert_eq!(payload["error"]["kind"], "native_failure");
    assert_eq!(payload["error"]["native_code"], 22);
}
