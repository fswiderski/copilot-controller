use std::sync::Arc;

use super::*;

static FIXTURE_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

mod reliability;

fn options(micro: bool, health: bool) -> Value {
    if micro {
        json!({
            "capture_ms": 0, "query": false, "read_keymap": false,
            "test_pattern": false, "health_check": health,
        })
    } else {
        json!({
            "capture_ms": 0, "initialize": false,
            "test_pattern": false, "health_check": health,
        })
    }
}

fn request(micro: bool, health: bool) -> DiagnosticRequest {
    DiagnosticRequest {
        kind: if micro {
            "micro_hid_probe"
        } else {
            "akp03_hid_probe"
        }
        .into(),
        payload: options(micro, health),
    }
}

fn frame(request: &DiagnosticRequest, nonce: Uuid) -> Vec<u8> {
    protocol::encode_frame(&protocol::RequestEnvelopeRef {
        version: protocol::VERSION,
        request_nonce: nonce,
        request,
    })
    .unwrap()
}

fn helper(mode: &str, micro: bool) -> Command {
    let mut command =
        Command::new(std::env::var_os("MACOS_WORKER_TEST_HELPER").expect("fixture path"));
    command
        .arg(if micro {
            "__probe-worker"
        } else {
            "__akp03-worker"
        })
        .env("MACOS_WORKER_FIXTURE_MODE", mode);
    command
}

fn assert_reaped(identity: WorkerIdentity) {
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(identity.pid as i32, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

fn run(mode: &str, micro: bool) -> Result<DiagnosticResult, WorkerError> {
    let nonce = Uuid::new_v4();
    supervise_command(
        &mut helper(mode, micro),
        frame(&request(micro, false), nonce),
        nonce,
        Duration::from_secs(3),
        &AtomicBool::new(false),
    )
}

#[test]
fn macos_parent_admission_rejects_ct_unknown_and_invalid_hid_options_without_spawn() {
    for (kind, payload, expected) in [
        ("ct_serial_probe", Value::Null, Kind::UnsupportedPlatform),
        ("command", Value::Null, Kind::UnsupportedPlatform),
        ("micro_hid_probe", json!({}), Kind::InvalidRequest),
        (
            "akp03_hid_probe",
            json!({"initialize": true}),
            Kind::InvalidRequest,
        ),
        ("health", json!({}), Kind::InvalidRequest),
    ] {
        assert_eq!(
            validate_platform_request(&DiagnosticRequest {
                kind: kind.into(),
                payload
            }),
            Err(expected)
        );
    }
    let registry = crate::cli::diagnostic_allowlist().unwrap();
    for micro in [true, false] {
        let mut invalid = request(micro, false);
        invalid.payload["capture_ms"] = json!(30001);
        let error = super::super::probe(
            &invalid,
            Duration::from_secs(3),
            &AtomicBool::new(false),
            &registry,
        )
        .unwrap_err();
        assert_eq!(error.kind, Kind::InvalidRequest);
        assert!(error.worker.is_none());
    }
    let error = super::super::probe(
        &DiagnosticRequest {
            kind: "ct_serial_probe".into(),
            payload: Value::Null,
        },
        Duration::from_secs(3),
        &AtomicBool::new(false),
        &registry,
    )
    .unwrap_err();
    assert_eq!(error.kind, Kind::UnsupportedPlatform);
    assert!(error.worker.is_none());
}

#[test]
#[ignore = "requires self-authored MACOS_WORKER_TEST_HELPER"]
fn macos_success_identity_correlation_and_bounded_output() {
    let _serial = FIXTURE_TESTS.lock().unwrap();
    let mut coordinator = Command::new(std::env::var_os("MACOS_WORKER_TEST_HELPER").unwrap());
    coordinator
        .arg("round-trip")
        .env("MACOS_WORKER_FIXTURE_MODE", "echo");
    let output =
        crate::native::portable::child::capture(&mut coordinator, Duration::from_secs(5)).unwrap();
    let result: DiagnosticResult = serde_json::from_slice(&output).unwrap();
    assert_eq!(result.payload, options(true, false));
    for micro in [true, false] {
        let result = run("echo", micro).unwrap();
        assert_eq!(result.payload, options(micro, false));
        assert_ne!(result.worker.pid, std::process::id());
        assert!(result.worker.creation_time_100ns > 0);
        assert_reaped(result.worker);
    }
    assert!(run("stderr-exact", true).is_ok());
    let direct = run("parent-gate", true).unwrap();
    assert_eq!(direct.payload["native_access_denied"], true);
    assert_reaped(direct.worker);
    for (mode, expected) in [
        ("forge-nonce", Kind::Protocol),
        ("forge-pid", Kind::Protocol),
        ("forge-start", Kind::Protocol),
        ("forge-version", Kind::Protocol),
        ("trailing", Kind::Protocol),
        ("truncated-header", Kind::Transport),
        ("truncated-body", Kind::Transport),
        ("oversize", Kind::FrameLimit),
        ("stdout-over", Kind::FrameLimit),
        ("stderr-over", Kind::DiagnosticLimit),
    ] {
        let error = run(mode, true).unwrap_err();
        assert_eq!(error.kind, expected, "{mode}");
        assert!(error.replacement_allowed, "{mode}");
        assert_reaped(error.worker.unwrap());
        assert!(run("echo", true).is_ok(), "{mode}: replacement");
    }
}

#[test]
#[ignore = "requires self-authored MACOS_WORKER_TEST_HELPER"]
fn macos_timeout_and_cancel_kill_reap_and_allow_only_clean_replacement() {
    let _serial = FIXTURE_TESTS.lock().unwrap();
    for cancel in [false, true] {
        let signal = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&signal);
        let canceller = thread::spawn(move || {
            if cancel {
                thread::sleep(Duration::from_millis(100));
                flag.store(true, Ordering::Release);
            }
        });
        let nonce = Uuid::new_v4();
        let started = Instant::now();
        let error = supervise_command(
            &mut helper("stall", true),
            frame(&request(true, false), nonce),
            nonce,
            if cancel {
                Duration::from_secs(3)
            } else {
                Duration::from_millis(100)
            },
            &signal,
        )
        .unwrap_err();
        canceller.join().unwrap();
        assert_eq!(
            error.kind,
            if cancel {
                Kind::Cancelled
            } else {
                Kind::TimedOut
            }
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(error.replacement_allowed);
        assert_reaped(error.worker.unwrap());
        assert!(run("echo", true).is_ok());
    }
    let nonce = Uuid::new_v4();
    let error = supervise_command(
        &mut helper("no-read", true),
        vec![b'x'; MAX_FRAME_BODY_BYTES + 4],
        nonce,
        Duration::from_millis(100),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert_eq!(error.kind, Kind::TimedOut);
    assert!(error.replacement_allowed);
    assert_reaped(error.worker.unwrap());
    assert!(run("echo", true).is_ok());
}

#[test]
#[ignore = "requires self-authored MACOS_WORKER_TEST_HELPER"]
fn macos_child_revalidates_malformed_frames_and_health_never_needs_native_backend() {
    let _serial = FIXTURE_TESTS.lock().unwrap();
    let nonce = Uuid::new_v4();
    let valid = frame(&request(true, false), nonce);
    let mut trailing = valid.clone();
    trailing.push(b'x');
    let body = br#"{"version":1,"request_nonce":"00000000-0000-0000-0000-000000000001","request":{"kind":"micro_hid_probe","payload":{"capture_ms":0,"capture_ms":1}}}"#;
    let mut duplicate = (body.len() as u32).to_le_bytes().to_vec();
    duplicate.extend_from_slice(body);
    for invalid in [
        vec![0; 4],
        (MAX_FRAME_BODY_BYTES as u32 + 1).to_le_bytes().to_vec(),
        valid[..valid.len() - 1].to_vec(),
        trailing,
        duplicate,
        frame(
            &DiagnosticRequest {
                kind: "ct_serial_probe".into(),
                payload: Value::Null,
            },
            nonce,
        ),
        frame(
            &DiagnosticRequest {
                kind: "micro_hid_probe".into(),
                payload: json!({}),
            },
            nonce,
        ),
    ] {
        let error = supervise_command(
            &mut helper("echo", true),
            invalid,
            nonce,
            Duration::from_secs(3),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.replacement_allowed);
        assert_reaped(error.worker.unwrap());
        assert!(run("echo", true).is_ok());
    }
    for micro in [true, false] {
        let mut command = helper("production", true);
        let result = supervise_command(
            &mut command,
            frame(&request(micro, true), nonce),
            nonce,
            Duration::from_secs(3),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(result.payload["report"]["phase"], "health_check");
        assert_eq!(result.payload["report"]["opened"], false);
        assert!(result.payload["report"]["candidate"].is_null());
        assert_eq!(result.payload["report"]["output_reports"], 0);
        assert_reaped(result.worker);
    }
}

fn isolated(test: &str) {
    let name = format!("{}::{test}", module_path!().split_once("::").unwrap().1);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &name, "--nocapture"])
        .env("MACOS_WORKER_ISOLATED", test)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    let pid = child.id() as i32;
    let mut guard = GroupGuard::new(child, pid);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !peek_exited(pid, deadline).unwrap() && Instant::now() < deadline {
        thread::sleep(POLL);
    }
    assert!(
        guard.finish().unwrap().success(),
        "isolated test failed: {test}"
    );
}

#[test]
fn macos_direct_routes_and_uncontained_capabilities_are_rejected() {
    assert!(hid_worker_capability().is_none());
    assert!(worker_health().is_err());
    for (route, allowlist) in [
        (
            "__probe-worker",
            crate::cli::diagnostic_allowlist().unwrap(),
        ),
        (
            "__akp03-worker",
            crate::cli::akp03_probe_allowlist().unwrap(),
        ),
    ] {
        let error = super::super::maybe_run_hidden_worker([route], &allowlist)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind, Kind::Containment);
    }
    #[cfg(any(feature = "micro-hid", feature = "akp03-hid"))]
    assert!(matches!(
        crate::native::hid::native_backend(),
        Err(crate::native::hid::HidError::Unavailable(
            crate::native::hid::UnavailableReason::UnsupervisedAccess
        ))
    ));
}

#[test]
fn macos_write_capability_is_scoped_health_denied_and_native_output_uses_the_gate() {
    const TEST: &str =
        "macos_write_capability_is_scoped_health_denied_and_native_output_uses_the_gate";
    if std::env::var("MACOS_WORKER_ISOLATED").as_deref() != Ok(TEST) {
        isolated(TEST);
        return;
    }
    assert!(hid_worker_capability().is_none());
    {
        let _watchdog = ParentWatchdog::start().unwrap();
        let _health = WorkerScope::enter(false).unwrap();
        assert!(worker_health().is_ok());
        assert!(hid_worker_capability().is_none());
        #[cfg(any(feature = "micro-hid", feature = "akp03-hid"))]
        assert!(matches!(
            crate::native::hid::native_backend(),
            Err(crate::native::hid::HidError::Unavailable(
                crate::native::hid::UnavailableReason::UnsupervisedAccess
            ))
        ));
    }
    let capability;
    {
        let _watchdog = ParentWatchdog::start().unwrap();
        let _live = WorkerScope::enter(true).unwrap();
        capability = hid_worker_capability().unwrap();
        assert!(capability.valid());
        #[cfg(any(feature = "micro-hid", feature = "akp03-hid"))]
        {
            // Constructor and fake SetReport only; never enumerate/open IOKit.
            assert!(crate::native::hid::native_backend().is_ok());
            for report in [vec![0; 1025], vec![6; 64]] {
                let written =
                    crate::native::hid::fixture_output(&capability, &report, |id, bytes| {
                        assert_eq!(id, isize::from(report[0]));
                        assert_eq!(bytes, if id == 0 { &report[1..] } else { &report });
                        0
                    })
                    .unwrap();
                assert_eq!(written, report.len());
            }
        }
    }
    assert!(!capability.valid());
    {
        let _watchdog = ParentWatchdog::start().unwrap();
        let _next = WorkerScope::enter(true).unwrap();
        assert!(
            !capability.valid(),
            "a later request must not revive the old capability"
        );
    }
    #[cfg(any(feature = "micro-hid", feature = "akp03-hid"))]
    assert_eq!(
        crate::native::hid::fixture_output(&capability, &[6; 64], |_, _| panic!(
            "unscoped native write"
        )),
        Err(crate::native::hid::HidError::Unavailable(
            crate::native::hid::UnavailableReason::UnsupervisedWrite
        )),
    );
}

#[test]
fn macos_lost_leader_stickily_fences_replacement() {
    const TEST: &str = "macos_lost_leader_stickily_fences_replacement";
    if std::env::var("MACOS_WORKER_ISOLATED").as_deref() != Ok(TEST) {
        isolated(TEST);
        return;
    }
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--list")
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().unwrap();
    let pid = child.id() as i32;
    child.wait().unwrap(); // Simulate a conflicting, external reaper.
    let mut guard = GroupGuard::new(child, pid);
    assert!(guard.finish().is_err());
    drop(guard);
    let nonce = Uuid::new_v4();
    let error = supervise_command(
        &mut command,
        frame(&request(true, false), nonce),
        nonce,
        Duration::from_secs(1),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert_eq!(error.kind, Kind::SupervisionBlocked);
    assert!(!error.replacement_allowed);
    assert!(error.worker.is_none());
}
