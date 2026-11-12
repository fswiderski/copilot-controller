use super::*;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).into()).collect()
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

fn success(options: &Options) -> Outcome {
    match options {
        Options::Micro(options) => Outcome::Micro(micro::MicroProbeResult {
            probe_succeeded: true,
            read_only: !options.query,
            report: micro::Progress {
                phase: "complete".into(),
                opened: true,
                descriptor_verified: true,
                output_attempts: usize::from(options.query),
                ..Default::default()
            },
            ..Default::default()
        }),
        Options::Akp03(options) => Outcome::Akp03(akp03::Akp03ProbeResult {
            probe_succeeded: true,
            read_only: !options.initialize,
            report: akp03::Progress {
                phase: "complete".into(),
                opened: true,
                descriptor_verified: true,
                initialized: options.initialize,
                ..Default::default()
            },
            ..Default::default()
        }),
    }
}

#[test]
fn invalid_public_and_programmatic_options_never_launch_workers() {
    let common: &[&[&str]] = &[
        &["--capture-ms"],
        &["--capture-ms", "30001"],
        &["--capture-ms", "4294967296"],
        &["--capture-ms", "-1"],
        &["--capture-ms", "+1"],
        &["--capture-ms", "1.0"],
        &["--capture-ms", "NaN"],
        &["--capture-ms", " 1"],
        &["--capture-ms", "１"],
        &["--capture-ms", ""],
        &["--capture-ms", "0", "--capture-ms", "1"],
        &["--test-pattern"],
        &["--test-pattern", "--test-pattern"],
        &["--health-check", "--health-check"],
        &["--health-check", "false"],
        &["--capture_ms", "0"],
        &["--port", "private"],
        &["--path", "private"],
        &["--command", "private"],
        &["--selector", "private"],
        &["--schema-version", "1"],
    ];
    let micro_only: &[&[&str]] = &[
        &["--read-keymap"],
        &["--query", "--query"],
        &["--query", "--health-check"],
        &["--query", "--read-keymap", "--health-check"],
        &["--query", "--test-pattern", "--health-check"],
        &["--initialize"],
    ];
    let akp03_only: &[&[&str]] = &[
        &["--initialize", "--initialize"],
        &["--initialize", "--health-check"],
        &["--initialize", "--test-pattern", "--health-check"],
        &["--query"],
        &["--read-keymap"],
    ];
    for (name, cases) in [("micro-probe", micro_only), ("akp03-probe", akp03_only)] {
        for case in common.iter().chain(cases) {
            let invocation: Vec<_> = std::iter::once(name)
                .chain(case.iter().copied())
                .map(str::to_owned)
                .collect();
            assert!(crate::cli::parse(&invocation).is_err(), "{invocation:?}");
        }
    }
    for options in [
        Options::Micro(micro::MicroProbeOptions {
            read_keymap: true,
            ..Default::default()
        }),
        Options::Micro(micro::MicroProbeOptions {
            capture_ms: 30_001,
            ..Default::default()
        }),
        Options::Akp03(akp03::Akp03ProbeOptions {
            test_pattern: true,
            ..Default::default()
        }),
        Options::Akp03(akp03::Akp03ProbeOptions {
            capture_ms: 30_001,
            ..Default::default()
        }),
    ] {
        assert!(execute_with(options, |_, _| panic!("invalid options launched a worker")).is_err());
    }
}

#[test]
fn public_defaults_opt_ins_and_supervision_match_device_contracts() {
    assert_eq!(micro::MicroProbeOptions::default().capture_ms, 0);
    assert_eq!(akp03::Akp03ProbeOptions::default().capture_ms, 0);
    for (is_micro, invocation, milliseconds, expected) in [
        (
            true,
            vec![],
            22_000,
            json!({
                "capture_ms": 2000, "query": false, "read_keymap": false,
                "test_pattern": false, "health_check": false,
            }),
        ),
        (
            false,
            vec![],
            17_000,
            json!({
                "capture_ms": 2000, "initialize": false,
                "test_pattern": false, "health_check": false,
            }),
        ),
        (
            true,
            vec![
                "--capture-ms",
                "30000",
                "--query",
                "--read-keymap",
                "--test-pattern",
            ],
            55_000,
            json!({
                "capture_ms": 30000, "query": true, "read_keymap": true,
                "test_pattern": true, "health_check": false,
            }),
        ),
        (
            false,
            vec!["--capture-ms", "30000", "--initialize", "--test-pattern"],
            45_000,
            json!({
                "capture_ms": 30000, "initialize": true,
                "test_pattern": true, "health_check": false,
            }),
        ),
        (
            true,
            vec!["--capture-ms", "0"],
            20_000,
            json!({
                "capture_ms": 0, "query": false, "read_keymap": false,
                "test_pattern": false, "health_check": false,
            }),
        ),
        (
            false,
            vec!["--capture-ms", "0"],
            15_000,
            json!({
                "capture_ms": 0, "initialize": false,
                "test_pattern": false, "health_check": false,
            }),
        ),
        (
            true,
            vec!["--health-check", "--capture-ms", "30000"],
            3_000,
            json!({
                "capture_ms": 30000, "query": false, "read_keymap": false,
                "test_pattern": false, "health_check": true,
            }),
        ),
        (
            false,
            vec!["--health-check"],
            3_000,
            json!({
                "capture_ms": 2000, "initialize": false,
                "test_pattern": false, "health_check": true,
            }),
        ),
    ] {
        let options = parse(&args(&invocation), is_micro).unwrap();
        assert_eq!(options.payload().unwrap(), expected);
        let payload = if options.health_check() {
            options.health_outcome().payload().unwrap()
        } else {
            success(&options).payload().unwrap()
        };
        execute_with(options, |request, budget| {
            assert_eq!(request.kind, if is_micro { MICRO_KIND } else { AKP03_KIND });
            assert_eq!(request.payload, expected);
            assert_eq!(budget, Duration::from_millis(milliseconds));
            Ok(worker_result(payload))
        })
        .unwrap();
    }
}

#[test]
fn callback_requires_complete_strict_options_before_containment_or_native_access() {
    for is_micro in [true, false] {
        let valid = parse(&[], is_micro).unwrap().payload().unwrap();
        let mut cases = vec![
            Value::Null,
            json!([]),
            json!("private"),
            json!(1),
            json!({}),
            json!({"command": "private"}),
        ];
        for field in valid.as_object().unwrap().keys() {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            cases.push(missing);
            let mut wrong_type = valid.clone();
            wrong_type[field] = json!("false");
            cases.push(wrong_type);
        }
        for replacement in [
            json!(null),
            json!(-1),
            json!(1.0),
            json!(30001),
            json!(4294967296u64),
        ] {
            let mut invalid = valid.clone();
            invalid["capture_ms"] = replacement;
            cases.push(invalid);
        }
        let mut unknown = valid.clone();
        unknown["path"] = json!("private");
        cases.push(unknown);
        let mut dependent = valid;
        dependent["test_pattern"] = json!(true);
        cases.push(dependent);
        for payload in cases {
            assert!(
                callback_with(
                    payload,
                    is_micro,
                    || panic!("invalid payload reached containment"),
                    |_| panic!("invalid payload reached native"),
                )
                .is_err()
            );
        }
    }
}

#[test]
fn health_never_reaches_containment_backend_construction_or_transport() {
    for is_micro in [true, false] {
        let options = parse(&args(&["--health-check"]), is_micro).unwrap();
        let payload = callback_with(
            options.payload().unwrap(),
            is_micro,
            || panic!("pure health need not check native containment"),
            |_| panic!("health must not construct backend or enumerate/open hardware"),
        )
        .unwrap();
        assert_eq!(payload, options.health_outcome().payload().unwrap());
        assert_eq!(payload["report"]["phase"], "health_check");
        assert_eq!(payload["report"]["opened"], false);
        assert!(payload["report"]["candidate"].is_null());
        assert_eq!(payload["report"]["output_reports"], 0);
        let output = execute_with(options, |_, budget| {
            assert_eq!(budget, Duration::from_secs(3));
            Ok(worker_result(payload))
        })
        .unwrap();
        let value: Value = serde_json::from_str(&output.render().unwrap()).unwrap();
        assert_eq!(value["probe_succeeded"], true);
        assert_eq!(value["command_ready"], false);
        assert_eq!(value["hardware_qualified"], false);
    }
}

#[test]
fn valid_live_callbacks_require_containment_before_native_access() {
    for is_micro in [true, false] {
        let options = parse(&[], is_micro).unwrap();
        assert!(
            callback_with(
                options.payload().unwrap(),
                is_micro,
                || Err(worker::DiagnosticCallbackError),
                |_| panic!("uncontained request reached native backend"),
            )
            .is_err()
        );
        let payload =
            callback_with(options.payload().unwrap(), is_micro, || Ok(()), success).unwrap();
        assert_eq!(payload, success(&options).payload().unwrap());
    }
}

#[test]
fn native_failure_is_preserved_as_non_success_and_worker_failure_has_no_report() {
    for is_micro in [true, false] {
        let flags: &[&str] = if is_micro {
            &["--query"]
        } else {
            &["--initialize"]
        };
        let options = parse(&args(flags), is_micro).unwrap();
        let failure = unavailable(&options, "native diagnostic failure".into())
            .payload()
            .unwrap();
        let result = execute_with(options.clone(), |_, _| Ok(worker_result(failure.clone())));
        let error: Value = serde_json::from_str(&result.err().unwrap()).unwrap();
        assert_eq!(error, failure);
        assert_eq!(error["probe_succeeded"], false);
        assert_eq!(error["command_ready"], false);
        assert_eq!(error["hardware_qualified"], false);
        assert_eq!(error["read_only"], is_micro);
        let partial = callback_with(
            options.payload().unwrap(),
            is_micro,
            || Ok(()),
            |options| {
                let mut outcome = success(options);
                match &mut outcome {
                    Outcome::Micro(result) => {
                        result.probe_succeeded = false;
                        result.error = Some("primary acquisition failure".into());
                        result.report.output_attempts = 3;
                        result.report.output_reports = 2;
                        result.report.output_bytes = 129;
                        result.report.write_failed = true;
                        result.report.cleanup_error = Some("cleanup failure".into());
                    }
                    Outcome::Akp03(result) => {
                        result.probe_succeeded = false;
                        result.error = Some("CONNECT failed after initialization".into());
                        result.report.phase = "initialize".into();
                        result.report.output_reports = 2;
                        result.report.output_bytes = 2057;
                    }
                }
                outcome
            },
        )
        .unwrap();
        let result = execute_with(options.clone(), |_, _| Ok(worker_result(partial.clone())));
        let error: Value = serde_json::from_str(&result.err().unwrap()).unwrap();
        assert_eq!(error, partial);
        assert_eq!(error["read_only"], false);
        for kind in [
            worker::WorkerErrorKind::UnsupportedPlatform,
            worker::WorkerErrorKind::TimedOut,
            worker::WorkerErrorKind::Callback,
            worker::WorkerErrorKind::CleanupUnconfirmed,
            worker::WorkerErrorKind::FrameLimit,
        ] {
            let result = execute_with(options.clone(), |_, _| {
                Err(worker::WorkerError {
                    kind,
                    worker: None,
                    replacement_allowed: kind != worker::WorkerErrorKind::CleanupUnconfirmed,
                })
            });
            let error: Value = serde_json::from_str(&result.err().unwrap()).unwrap();
            assert_eq!(error["status"], "worker_error");
            assert!(error.get("report").is_none());
        }
    }
}

#[test]
fn inclusive_capture_and_input_limits_are_not_off_by_one() {
    for is_micro in [true, false] {
        let options = parse(&[], is_micro).unwrap();
        let mut outcome = success(&options);
        match &mut outcome {
            Outcome::Micro(result) => {
                result.report.captures = vec![micro::Capture::default(); 128];
                result.report.inputs = vec![
                    micro::ObservedInput {
                        message: json!({}),
                        ..Default::default()
                    };
                    128
                ];
            }
            Outcome::Akp03(result) => {
                result.report.captures = vec![akp03::Capture::default(); 128];
            }
        }
        let payload = outcome.payload().unwrap();
        execute_with(options, |_, _| Ok(worker_result(payload))).unwrap();
    }
}

#[test]
fn malformed_or_forged_worker_results_fail_closed_without_echoing_payload() {
    for is_micro in [true, false] {
        let options = parse(&[], is_micro).unwrap();
        let valid = success(&options).payload().unwrap();
        let mut cases = vec![
            Value::Null,
            json!([]),
            json!({}),
            json!({"error": "private"}),
        ];
        let mut sequence = valid.clone();
        sequence["report"] = json!([]);
        cases.push(sequence);
        for field in valid.as_object().unwrap().keys() {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            cases.push(missing);
        }
        for field in valid["report"].as_object().unwrap().keys() {
            let mut missing = valid.clone();
            missing["report"].as_object_mut().unwrap().remove(field);
            cases.push(missing);
        }
        for (pointer, replacement) in [
            ("/command_ready", json!(true)),
            ("/hardware_qualified", json!(true)),
            ("/read_only", json!(false)),
            ("/probe_succeeded", json!("true")),
            ("/error", json!("private")),
            ("/report/output_bytes", json!(-1)),
            ("/report/elapsed_ms", json!(1.0)),
            ("/report/candidate", json!({"path": "private"})),
        ] {
            let mut invalid = valid.clone();
            *invalid.pointer_mut(pointer).unwrap() = replacement;
            cases.push(invalid);
        }
        let capture = if is_micro {
            serde_json::to_value(micro::Capture::default()).unwrap()
        } else {
            serde_json::to_value(akp03::Capture::default()).unwrap()
        };
        let mut excessive = valid.clone();
        excessive["report"]["captures"] = json!(vec![capture.clone(); 129]);
        cases.push(excessive);
        let mut unknown_capture = capture;
        unknown_capture["private"] = json!(true);
        let mut invalid = valid.clone();
        invalid["report"]["captures"] = json!([unknown_capture]);
        cases.push(invalid);
        if !is_micro {
            for field in ["decoded", "decode_error"] {
                let mut capture = serde_json::to_value(akp03::Capture::default()).unwrap();
                capture.as_object_mut().unwrap().remove(field);
                let mut invalid = valid.clone();
                invalid["report"]["captures"] = json!([capture]);
                cases.push(invalid);
            }
        }
        if is_micro {
            let mut excessive = valid.clone();
            excessive["report"]["inputs"] = json!(vec![micro::ObservedInput::default(); 129]);
            cases.push(excessive);
            for field in ["slot_requested", "command_key_observed", "joystick"] {
                let mut input = serde_json::to_value(micro::ObservedInput {
                    message: json!({}),
                    ..Default::default()
                })
                .unwrap();
                input.as_object_mut().unwrap().remove(field);
                let mut invalid = valid.clone();
                invalid["report"]["inputs"] = json!([input]);
                cases.push(invalid);
            }
            let mut invalid = valid.clone();
            invalid["report"]["inputs"] = json!([{
                "elapsed_ms": 0, "message": [], "slot_requested": null,
                "command_key_observed": null, "mic_observed": false, "joystick": null,
            }]);
            cases.push(invalid);
        }
        let mut unknown = valid.clone();
        unknown["private"] = json!(true);
        cases.push(unknown);
        let mut unknown = valid;
        unknown["report"]["private"] = json!(true);
        cases.push(unknown);
        let mut limit = success(&options).payload().unwrap();
        limit["response_limit"] = json!({
            "omitted_fields": [], "captures_dropped": 1,
            "inputs_dropped": 0, "serialization_failed": false,
        });
        // Omission metadata cannot turn incomplete export into probe success.
        cases.push(limit.clone());
        limit["probe_succeeded"] = json!(false);
        limit["error"] = json!("response limit");
        // Dropped captures require the corresponding truncation flag.
        cases.push(limit.clone());
        limit["report"]["capture_truncated"] = json!(true);
        for invalid in [
            json!({"omitted_fields": [], "captures_dropped": 129, "inputs_dropped": 0, "serialization_failed": false}),
            json!({"omitted_fields": ["error", "error"], "captures_dropped": 1, "inputs_dropped": 0, "serialization_failed": false}),
            json!({"omitted_fields": ["private"], "captures_dropped": 1, "inputs_dropped": 0, "serialization_failed": false}),
            json!({"omitted_fields": [], "captures_dropped": 1, "inputs_dropped": 0}),
            json!({"omitted_fields": [], "captures_dropped": 1, "inputs_dropped": 0, "serialization_failed": false, "private": true}),
        ] {
            let mut value = limit.clone();
            value["response_limit"] = invalid;
            cases.push(value);
        }
        for payload in cases {
            let error = execute_with(options.clone(), |_, _| Ok(worker_result(payload)))
                .err()
                .unwrap();
            assert!(!error.contains("private"));
            let error: Value = serde_json::from_str(&error).unwrap();
            assert_eq!(error["status"], "worker_error");
            assert_eq!(error["error"]["kind"], "protocol");
        }
        let health = parse(&args(&["--health-check"]), is_micro).unwrap();
        let mut forged = health.health_outcome().payload().unwrap();
        forged["report"]["opened"] = json!(true);
        assert!(execute_with(health, |_, _| Ok(worker_result(forged))).is_err());
    }
}

#[test]
fn callback_cannot_emit_forged_native_outcomes() {
    for is_micro in [true, false] {
        let options = parse(&[], is_micro).unwrap();
        assert!(
            callback_with(
                options.payload().unwrap(),
                is_micro,
                || Ok(()),
                |options| {
                    let mut outcome = success(options);
                    match &mut outcome {
                        Outcome::Micro(result) => result.hardware_qualified = true,
                        Outcome::Akp03(result) => result.command_ready = true,
                    }
                    outcome
                },
            )
            .is_err()
        );
    }
}

#[test]
#[cfg(any(not(feature = "micro-hid"), not(feature = "akp03-hid")))]
fn disabled_device_specific_features_fail_without_constructing_a_backend() {
    #[cfg(not(feature = "micro-hid"))]
    {
        let options = Options::Micro(micro::MicroProbeOptions::default());
        let outcome = native_outcome(&options);
        assert!(!outcome.succeeded());
        assert!(outcome.valid(&options));
        assert_eq!(outcome.payload().unwrap()["report"]["opened"], false);
    }
    #[cfg(not(feature = "akp03-hid"))]
    {
        let options = Options::Akp03(akp03::Akp03ProbeOptions::default());
        let outcome = native_outcome(&options);
        assert!(!outcome.succeeded());
        assert!(outcome.valid(&options));
        assert_eq!(outcome.payload().unwrap()["report"]["opened"], false);
    }
}

fn assert_bounded_response(payload: &Value) {
    let envelope = json!({
        "version": 1, "request_nonce": "ffffffff-ffff-ffff-ffff-ffffffffffff",
        "worker": { "pid": u32::MAX, "creation_time_100ns": u64::MAX },
        "outcome": { "status": "ok", "payload": payload },
    });
    assert!(serde_json::to_vec(&envelope).unwrap().len() <= worker::MAX_FRAME_BODY_BYTES);
    assert!(serde_json::to_vec(payload).unwrap().len() <= worker::MAX_CALLBACK_PAYLOAD_BYTES);
}

#[test]
fn max_size_akp03_captures_return_bounded_explicit_failure_with_whole_prefix() {
    let options = parse(&args(&["--initialize", "--test-pattern"]), false).unwrap();
    let original: Vec<_> = (0..128)
        .map(|elapsed_ms| akp03::Capture {
            elapsed_ms,
            bytes: 512,
            hex: "41".repeat(512),
            decoded: Some(crate::devices::akp03::input::DecodedInput::Acknowledgment),
            decode_error: None,
        })
        .collect();
    let payload = callback_with(
        options.payload().unwrap(),
        false,
        || Ok(()),
        |options| {
            let mut outcome = success(options);
            let Outcome::Akp03(result) = &mut outcome else {
                panic!("wrong kind")
            };
            result.report.captures = original.clone();
            result.report.output_reports = 25;
            result.report.output_bytes = 25 * 1025;
            result.report.patterns_written = 6;
            outcome
        },
    )
    .unwrap();
    assert_bounded_response(&payload);
    let result: akp03::Akp03ProbeResult = serde_json::from_value(payload.clone()).unwrap();
    assert!(!result.probe_succeeded);
    assert!(!result.command_ready && !result.hardware_qualified && !result.read_only);
    assert!(result.report.initialized && result.report.capture_truncated);
    assert_eq!(result.report.output_reports, 25);
    assert_eq!(result.report.output_bytes, 25 * 1025);
    assert_eq!(result.report.patterns_written, 6);
    let limit = result.response_limit.unwrap();
    assert!(!limit.serialization_failed);
    assert!(limit.captures_dropped > 0);
    assert_eq!(limit.captures_dropped + result.report.captures.len(), 128);
    assert_eq!(
        result.report.captures,
        original[..result.report.captures.len()]
    );
    let error = execute_with(options, |_, _| Ok(worker_result(payload.clone())))
        .err()
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&error).unwrap(), payload);
}

#[test]
fn oversized_micro_semantic_values_are_omitted_explicitly_after_cleanup() {
    use crate::devices::probe_result::OmittedField;
    let options = parse(&args(&["--query", "--read-keymap", "--test-pattern"]), true).unwrap();
    for cleanup_succeeded in [false, true] {
        let payload = callback_with(
            options.payload().unwrap(),
            true,
            || Ok(()),
            |options| {
                let mut outcome = success(options);
                let Outcome::Micro(result) = &mut outcome else {
                    panic!("wrong kind")
                };
                result.report.device_status =
                    Some(json!({"data": "s".repeat(worker::MAX_FRAME_BODY_BYTES)}));
                result.report.keymap =
                    Some(json!({"data": "k".repeat(worker::MAX_FRAME_BODY_BYTES)}));
                result.report.inputs = vec![micro::ObservedInput {
                    message: json!({"data": "i".repeat(worker::MAX_FRAME_BODY_BYTES)}),
                    ..Default::default()
                }];
                result.report.led_test_attempted = true;
                result.report.led_clear_acknowledged = cleanup_succeeded;
                result.report.output_attempts = 12;
                result.report.output_reports = 12;
                result.report.output_bytes = 12 * 64;
                if !cleanup_succeeded {
                    result.probe_succeeded = false;
                    result.error = Some("primary acquisition error".into());
                    result.report.cleanup_error = Some("clear failed".into());
                }
                outcome
            },
        )
        .unwrap();
        assert_bounded_response(&payload);
        let result: micro::MicroProbeResult = serde_json::from_value(payload.clone()).unwrap();
        assert!(!result.probe_succeeded && !result.read_only);
        assert!(!result.command_ready && !result.hardware_qualified);
        assert!(result.report.device_status.is_none() && result.report.keymap.is_none());
        assert!(result.report.inputs.is_empty());
        assert!(result.report.led_test_attempted);
        assert_eq!(result.report.led_clear_acknowledged, cleanup_succeeded);
        assert_eq!(result.report.output_attempts, 12);
        assert_eq!(result.report.output_reports, 12);
        assert_eq!(result.report.output_bytes, 12 * 64);
        if !cleanup_succeeded {
            assert!(
                result
                    .error
                    .as_deref()
                    .unwrap()
                    .starts_with("primary acquisition error; ")
            );
            assert_eq!(result.report.cleanup_error.as_deref(), Some("clear failed"));
        }
        let limit = result.response_limit.unwrap();
        assert!(limit.omitted_fields.contains(&OmittedField::DeviceStatus));
        assert!(limit.omitted_fields.contains(&OmittedField::Keymap));
        assert_eq!(limit.inputs_dropped, 1);
        assert!(!limit.serialization_failed);
        assert_eq!(limit.captures_dropped, 0);
        let error = execute_with(options.clone(), |_, _| Ok(worker_result(payload.clone())))
            .err()
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&error).unwrap(), payload);
    }
}

#[test]
fn bounded_micro_values_and_input_prefix_survive_an_oversized_keymap() {
    use crate::devices::probe_result::OmittedField;
    let options = parse(&args(&["--query", "--read-keymap"]), true).unwrap();
    let payload = callback_with(
        options.payload().unwrap(),
        true,
        || Ok(()),
        |options| {
            let mut outcome = success(options);
            let Outcome::Micro(result) = &mut outcome else {
                panic!("wrong kind")
            };
            result.report.device_status = Some(json!({"connected": true}));
            result.report.keymap = Some(json!({"data": "k".repeat(worker::MAX_FRAME_BODY_BYTES)}));
            result.report.inputs = vec![
                micro::ObservedInput {
                    message: json!({"AG": 1}),
                    ..Default::default()
                },
                micro::ObservedInput {
                    message: json!({"large": "i".repeat(worker::MAX_FRAME_BODY_BYTES)}),
                    ..Default::default()
                },
                micro::ObservedInput {
                    message: json!({"AG": 2}),
                    ..Default::default()
                },
            ];
            outcome
        },
    )
    .unwrap();
    assert_bounded_response(&payload);
    assert_eq!(
        payload["report"]["device_status"],
        json!({"connected": true})
    );
    assert_eq!(payload["report"]["inputs"].as_array().unwrap().len(), 1);
    assert_eq!(payload["report"]["inputs"][0]["message"], json!({"AG": 1}));
    assert_eq!(payload["response_limit"]["inputs_dropped"], 2);
    assert_eq!(
        payload["response_limit"]["omitted_fields"],
        json!([OmittedField::Keymap])
    );
}

#[test]
fn huge_diagnostic_strings_have_a_compact_failure_retaining_write_and_cleanup_flags() {
    let options = parse(&args(&["--query", "--test-pattern"]), true).unwrap();
    let payload = callback_with(
        options.payload().unwrap(),
        true,
        || Ok(()),
        |options| {
            let mut outcome = success(options);
            let Outcome::Micro(result) = &mut outcome else {
                panic!("wrong kind")
            };
            result.probe_succeeded = false;
            result.error = Some("e".repeat(worker::MAX_FRAME_BODY_BYTES));
            result.report.firmware = Some("f".repeat(worker::MAX_FRAME_BODY_BYTES));
            result.report.cleanup_error = Some("c".repeat(worker::MAX_FRAME_BODY_BYTES));
            result.report.led_test_attempted = true;
            result.report.led_clear_acknowledged = false;
            result.report.output_attempts = 1;
            result.report.output_reports = 1;
            result.report.output_bytes = 64;
            outcome
        },
    )
    .unwrap();
    assert_bounded_response(&payload);
    assert_eq!(payload["probe_succeeded"], false);
    assert_eq!(payload["report"]["phase"], "response_limit");
    assert_eq!(payload["report"]["output_bytes"], 64);
    assert_eq!(payload["report"]["led_test_attempted"], true);
    assert_eq!(payload["report"]["led_clear_acknowledged"], false);
    assert!(
        payload["report"]["cleanup_error"]
            .as_str()
            .unwrap()
            .contains("omitted")
    );
    assert!(payload["report"]["firmware"].is_null());
}

#[test]
fn compact_scalar_fallback_preserves_bounded_primary_and_cleanup_errors() {
    use crate::devices::probe_result::OmittedField;
    let options = parse(&args(&["--query", "--test-pattern"]), true).unwrap();
    let payload = callback_with(
        options.payload().unwrap(),
        true,
        || Ok(()),
        |options| {
            let mut outcome = success(options);
            let Outcome::Micro(result) = &mut outcome else {
                panic!("wrong kind")
            };
            result.probe_succeeded = false;
            result.error = Some("primary failure".into());
            result.report.cleanup_error = Some("cleanup failure".into());
            result.report.firmware = Some("f".repeat(worker::MAX_FRAME_BODY_BYTES));
            result.report.led_test_attempted = true;
            outcome
        },
    )
    .unwrap();
    assert_bounded_response(&payload);
    assert!(
        payload["error"]
            .as_str()
            .unwrap()
            .starts_with("primary failure; ")
    );
    assert_eq!(payload["report"]["cleanup_error"], "cleanup failure");
    let limit: crate::devices::probe_result::ResponseLimit =
        serde_json::from_value(payload["response_limit"].clone()).unwrap();
    assert!(limit.omitted_fields.contains(&OmittedField::Firmware));
    assert!(!limit.omitted_fields.contains(&OmittedField::Error));
    assert!(!limit.omitted_fields.contains(&OmittedField::CleanupError));
    assert!(!limit.serialization_failed);
    let error = execute_with(options, |_, _| Ok(worker_result(payload.clone())))
        .err()
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&error).unwrap(), payload);
}
