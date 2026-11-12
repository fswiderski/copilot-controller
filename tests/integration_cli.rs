use std::{
    io::{self, Read, Write},
    process::{Command, Output, Stdio},
};

use copilot_controller::{
    cli,
    devices::micro::wire::encode_json,
    domain::{ClockSample, ClockSource, MonotonicStamp, RuntimeEpoch},
    presentation::input::{PhysicalEvent, PhysicalEventKind},
};
use serde_json::{Value, json};

const BINARY: &str = env!("CARGO_BIN_EXE_copilot-controller");

fn invoke(args: &[&str]) -> Output {
    Command::new(BINARY)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn json_result(args: &[&str]) -> Value {
    let output = invoke(args);
    assert!(output.status.success(), "{:?}: {:?}", args, output.stderr);
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.ends_with('\n'));
    assert!(!text.ends_with("\n\n"));
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        text,
        format!("{}\n", serde_json::to_string_pretty(&value).unwrap())
    );
    assert_eq!(value["command_ready"], false);
    value
}

#[test]
fn inventory_and_doctor_are_truthful_and_never_action_ready() {
    let capabilities = json_result(&["capabilities"]);
    assert_eq!(
        capabilities["hardware_backend_available"],
        cfg!(windows)
            || cfg!(all(
                target_os = "macos",
                target_arch = "aarch64",
                any(feature = "micro-hid", feature = "akp03-hid")
            ))
    );
    assert_eq!(
        capabilities["micro_hid_probe_available"],
        cfg!(all(
            target_os = "macos",
            target_arch = "aarch64",
            feature = "micro-hid"
        ))
    );
    assert_eq!(
        capabilities["akp03_hid_probe_available"],
        cfg!(all(
            target_os = "macos",
            target_arch = "aarch64",
            feature = "akp03-hid"
        ))
    );
    assert_eq!(capabilities["ct_serial_probe_available"], cfg!(windows));
    assert_eq!(capabilities["firmware_execution_available"], false);
    assert_eq!(capabilities["runtime_owner_acquired"], false);
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    assert_eq!(
        capabilities["inventory"]["platform"]["cooperative_runtime_ownership"]["status"],
        "compiled"
    );
    assert_eq!(
        capabilities["inventory"]["hosts"]["integrated"]["authentication"]["status"],
        "unavailable"
    );
    let profiles = json_result(&["device-profiles"]);
    let profiles = profiles["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 3);
    for profile in profiles {
        assert_eq!(profile["runtime_qualified"], false);
        assert_eq!(profile["command_ready"], false);
        assert!(!profile["prioritized_roles"].as_array().unwrap().is_empty());
    }
    let doctor = json_result(&["doctor"]);
    assert_eq!(doctor["schema"], 1);
    assert_eq!(doctor["read_only"], true);
    assert_eq!(doctor["qualification"], "foundation_only");
    assert_eq!(doctor["platform_probes"]["serial"]["status"], "unavailable");
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    assert_eq!(doctor["platform_probes"]["clock"]["status"], "observed");
    #[cfg(not(windows))]
    for probe in ["current_process", "processes", "foreground", "desktop"] {
        assert_eq!(doctor["platform_probes"][probe]["status"], "unavailable");
    }
    for command in ["micro-doctor", "akp03-doctor"] {
        let report = json_result(&[command]);
        assert_eq!(report["discovery_performed"], false);
    }
}

#[test]
fn preview_routes_are_synthetic_and_apply_preferences() {
    for device in ["micro", "ajazz", "ct"] {
        let report = json_result(&[
            "device-preview",
            device,
            "--reduced-motion",
            "--high-contrast",
            "--brightness",
            "0.1",
        ]);
        assert_eq!(report["synthetic"], true);
        assert_eq!(report["device_io_performed"], false);
        assert_eq!(report["slots"].as_array().unwrap().len(), 6);
        assert!(
            report["slots"]
                .as_array()
                .unwrap()
                .iter()
                .all(|slot| slot["led_entry"].is_null())
        );
    }
    let micro = json_result(&["micro-preview"]);
    assert_eq!(micro["led_frame"].as_array().unwrap().len(), 6);
    for args in [
        vec!["akp03-preview"],
        vec!["device-preview", "ajazz", "--svg"],
    ] {
        let output = invoke(&args);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let svg = String::from_utf8(output.stdout).unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.ends_with('\n'));
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

#[test]
fn offline_decode_reports_complete_and_incomplete_frames_without_io() {
    let reports = encode_json(&json!({"result": {"status": "offline"}})).unwrap();
    let hexes: Vec<_> = reports.iter().map(|report| hex(report)).collect();
    let mut args = vec!["micro-decode"];
    args.extend(hexes.iter().map(String::as_str));
    let decoded = json_result(&args);
    assert_eq!(
        decoded["messages"],
        json!([{"result": {"status": "offline"}}])
    );
    assert_eq!(decoded["incomplete_bytes"], 0);

    let mut partial = [0u8; 64];
    partial[..4].copy_from_slice(&[6, 2, 1, b'{']);
    let incomplete = json_result(&["micro-decode", &hex(&partial)]);
    assert_eq!(incomplete["messages"], json!([]));
    assert_eq!(incomplete["incomplete_bytes"], 1);

    let decoded = json_result(&["akp03-decode", "41434b00000000000000"]);
    assert_eq!(decoded["profile"], "akp03-original");
    let decoded = json_result(&["akp03-decode", "41434b0000000000000000", "--rev2"]);
    assert_eq!(decoded["profile"], "akp03e-rev2");
}

#[test]
fn validation_errors_and_live_routes_have_no_success_output() {
    let invalid = [
        vec!["run"],
        vec!["send"],
        vec!["accept"],
        vec!["stop"],
        vec!["fork"],
        vec!["_control"],
        vec!["device-preview", "micro", "--svg"],
        vec!["device-preview", "ct", "--brightness", "NaN"],
        vec!["device-preview", "ajazz", "--brightness", "1.1"],
        vec![
            "device-preview",
            "micro",
            "--reduced-motion",
            "--reduced-motion",
        ],
        vec![
            "device-assign",
            "--export-root",
            "relative",
            "--confirm",
            "ASSIGN",
        ],
        vec!["device-assign", "--confirm", "ASSIGN"],
        vec!["micro-decode"],
        vec!["micro-decode", "0x00"],
        vec!["akp03-decode", "41434b00000000000000", "--rev2"],
        vec!["akp03-decode", "41434b00000000000000", "--unknown"],
        vec!["micro-doctor", "--discover", "--discover"],
        vec!["probe-health", "--command", "anything"],
        vec!["__probe-worker", "extra"],
        vec![
            "micro-firmware-plan",
            "--image",
            "relative",
            "--esptool",
            "relative",
        ],
    ];
    for args in invalid {
        let output = invoke(&args);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }
    let output = invoke(&["micro-firmware-update"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("unavailable")
    );
    for command in ["micro-doctor", "akp03-doctor"] {
        let output = invoke(&[command, "--discover"]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("unavailable")
        );
    }
}

#[test]
fn hid_cli_validation_and_hidden_routes_never_emit_success_output() {
    for args in [
        vec!["micro-probe", "--capture-ms", "30001"],
        vec!["micro-probe", "--capture-ms", "-1"],
        vec!["micro-probe", "--capture-ms", "0", "--capture-ms", "1"],
        vec!["micro-probe", "--query", "--query"],
        vec!["micro-probe", "--read-keymap"],
        vec!["micro-probe", "--test-pattern"],
        vec!["micro-probe", "--query", "--health-check"],
        vec!["micro-probe", "--query", "--command", "anything"],
        vec!["micro-probe", "--initialize"],
        vec!["akp03-probe", "--capture-ms"],
        vec!["akp03-probe", "--capture-ms", "1.0"],
        vec!["akp03-probe", "--capture-ms", "30001"],
        vec!["akp03-probe", "--initialize", "--initialize"],
        vec!["akp03-probe", "--test-pattern"],
        vec!["akp03-probe", "--initialize", "--health-check"],
        vec!["akp03-probe", "--query"],
        vec!["akp03-probe", "--path", "anything"],
        vec!["__akp03-worker"],
        vec!["__akp03-worker", "extra"],
        vec!["__akp03-worker", "--capture-ms", "0"],
        vec!["__micro-worker", "--query"],
    ] {
        let output = invoke(&args);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }
    let help = invoke(&["--help"]);
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("micro-probe"));
    assert!(text.contains("akp03-probe"));
    assert!(!text.contains("__probe-worker"));
    assert!(!text.contains("__akp03-worker"));
    #[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
    for args in [
        vec!["micro-probe", "--capture-ms", "0"],
        vec!["micro-probe", "--query", "--read-keymap", "--test-pattern"],
        vec!["akp03-probe", "--capture-ms", "0"],
        vec!["akp03-probe", "--initialize", "--test-pattern"],
    ] {
        let output = invoke(&args);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["status"], "worker_error");
        assert_eq!(error["error"]["kind"], "unsupported_platform");
        assert!(error["error"]["worker"].is_null());
    }
}

#[test]
fn hid_health_is_feature_independent_and_reports_zero_device_io() {
    for command in ["micro-probe", "akp03-probe"] {
        let output = invoke(&[command, "--health-check", "--capture-ms", "30000"]);
        #[cfg(windows)]
        if !output.status.success() {
            // Windows hosts can refuse containment; never replace it with live I/O.
            assert!(output.stdout.is_empty());
            assert!(!output.stderr.is_empty());
            continue;
        }
        assert!(output.status.success(), "{command}: {:?}", output.stderr);
        assert!(output.stderr.is_empty());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["probe_succeeded"], true);
        assert_eq!(value["command_ready"], false);
        assert_eq!(value["hardware_qualified"], false);
        assert_eq!(value["read_only"], true);
        assert!(value["error"].is_null());
        assert!(value["report"]["candidate"].is_null());
        assert_eq!(value["report"]["opened"], false);
        assert_eq!(value["report"]["descriptor_verified"], false);
        assert_eq!(value["report"]["phase"], "health_check");
        assert_eq!(value["report"]["output_reports"], 0);
        assert_eq!(value["report"]["output_bytes"], 0);
        assert_eq!(value["report"]["captures"], json!([]));
        #[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
        {
            let args: Vec<_> = [command, "--health-check"]
                .into_iter()
                .map(str::to_owned)
                .collect();
            cli::execute(cli::parse(&args).unwrap(), RefuseInput).unwrap();
        }
    }
}

#[cfg(all(
    any(windows, all(target_os = "macos", target_arch = "aarch64")),
    any(not(feature = "micro-hid"), not(feature = "akp03-hid"))
))]
#[test]
fn disabled_hid_cli_reports_fail_nonzero_without_native_backend_access() {
    let mut commands: Vec<&str> = Vec::new();
    #[cfg(not(feature = "micro-hid"))]
    commands.push("micro-probe");
    #[cfg(not(feature = "akp03-hid"))]
    commands.push("akp03-probe");
    for command in commands {
        let output = invoke(&[command, "--capture-ms", "0"]);
        assert!(!output.status.success(), "{command}");
        assert!(output.stdout.is_empty());
        let failure: Value = serde_json::from_slice(&output.stderr).unwrap();
        if failure.get("report").is_some() {
            assert_eq!(failure["probe_succeeded"], false);
            assert_eq!(failure["command_ready"], false);
            assert_eq!(failure["hardware_qualified"], false);
            assert_eq!(failure["report"]["opened"], false);
            assert!(failure["report"]["candidate"].is_null());
        } else {
            assert_eq!(failure["status"], "worker_error");
        }
    }
}

#[test]
fn ct_validation_and_hidden_worker_never_emit_success_output() {
    for args in [
        vec!["ct-probe", "--port", "COM1"],
        vec!["ct-probe", "--capture-ms", "0", "--capture-ms", "1"],
        vec!["ct-probe", "--test-pattern", "--command", "anything"],
        vec!["ct-probe", "--schema-version", "2"],
        vec!["__probe-worker"],
        vec!["__probe-worker", "extra"],
    ] {
        let output = invoke(&args);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }
    #[cfg(not(windows))]
    {
        let output = invoke(&["ct-probe", "--capture-ms", "0"]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["status"], "worker_error");
        assert_eq!(error["error"]["kind"], "unsupported_platform");
        assert!(error["error"]["worker"].is_null());
    }
}

#[test]
fn micro_decode_bounds_and_late_errors_never_emit_partial_json() {
    let report = hex(&encode_json(&json!({})).unwrap()[0]);
    let mut args = vec!["micro-decode"];
    args.extend(std::iter::repeat_n(report.as_str(), 129));
    let too_many = invoke(&args);
    assert!(!too_many.status.success());
    assert!(too_many.stdout.is_empty());

    let malformed = "00".repeat(64);
    let late_error = invoke(&["micro-decode", &report, &malformed]);
    assert!(!late_error.status.success());
    assert!(late_error.stdout.is_empty());
}

#[test]
fn micro_replay_preserves_surrogates_and_handles_deep_json_without_recursive_clone() {
    let raw = r#"{"s":"\ud800","literal":"\ufdd0sd800"}"#;
    let mut report = [0u8; 64];
    report[..3].copy_from_slice(&[6, 2, raw.len() as u8]);
    report[3..3 + raw.len()].copy_from_slice(raw.as_bytes());
    let output = invoke(&["micro-decode", &hex(&report)]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains(r#""s": "\ud800""#));
    assert!(!text.contains("\u{fdd0}sd800\u{fdd0}"));
    assert!(text.contains("\u{fdd0}sd800"));

    let raw = format!("{{\"deep\":{}0{}}}", "[".repeat(100), "]".repeat(100));
    let reports: Vec<_> = raw
        .as_bytes()
        .chunks(61)
        .map(|chunk| {
            let mut report = [0u8; 64];
            report[..3].copy_from_slice(&[6, 2, chunk.len() as u8]);
            report[3..3 + chunk.len()].copy_from_slice(chunk);
            hex(&report)
        })
        .collect();
    let mut args = vec!["micro-decode"];
    args.extend(reports.iter().map(String::as_str));
    let output = invoke(&args);
    assert!(output.status.success(), "{:?}", output.stderr);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("\"incomplete_bytes\": 0"));
    assert!(text.contains("\"deep\": ["));

    let raw = format!("{{\"deep\":{}0{}}}", "[".repeat(256), "]".repeat(256));
    let reports: Vec<_> = raw
        .as_bytes()
        .chunks(61)
        .map(|chunk| {
            let mut report = [0u8; 64];
            report[..3].copy_from_slice(&[6, 2, chunk.len() as u8]);
            report[3..3 + chunk.len()].copy_from_slice(chunk);
            hex(&report)
        })
        .collect();
    let mut args = vec!["micro-decode"];
    args.extend(reports.iter().map(String::as_str));
    let output = invoke(&args);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("depth limit")
    );
}
fn assignment() -> Value {
    let epoch = RuntimeEpoch::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
    let event = |kind, ticks| PhysicalEvent {
        control: "button_0".into(),
        kind,
        connection_generation: 1,
        at: ClockSample {
            source: ClockSource::LinuxMonotonic,
            stamp: MonotonicStamp {
                epoch,
                ticks,
                frequency: 1000,
            },
        },
    };
    json!({
        "device": "ajazz", "intent": {"select_slot": 1}, "adjacency_confirmed": false,
        "events": [event(PhysicalEventKind::Press, 0), event(PhysicalEventKind::Release, 50)],
    })
}

fn invoke_assignment_with_args(args: &[&str], bytes: &[u8]) -> Output {
    let mut child = Command::new(BINARY)
        .arg("device-assign")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    child.wait_with_output().unwrap()
}

fn invoke_assignment(bytes: &[u8]) -> Output {
    invoke_assignment_with_args(&[], bytes)
}

#[test]
fn confirmed_export_is_exclusive_or_fails_closed_without_creating_a_file() {
    use copilot_controller::persistence::root::PrivateRoot;
    let base = std::path::PathBuf::from(std::env::var_os("COPILOT_PRIVATE_ROOT_FIXTURES").unwrap());
    let root = base.join(format!("cli-export-{}", uuid::Uuid::new_v4()));
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let mut builder = builder;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&root).unwrap();
    let bytes = serde_json::to_vec(&assignment()).unwrap();
    let args = [
        "--export-root",
        root.to_str().unwrap(),
        "--confirm",
        "ASSIGN",
    ];
    let native = PrivateRoot::open(&root).and_then(|root| root.check_mutation_support());
    let output = invoke_assignment_with_args(&args, &bytes);
    let file = root.join("device-binding.json");
    if native.is_ok() {
        assert!(output.status.success(), "{:?}", output.stderr);
        let original = std::fs::read(&file).unwrap();
        let candidate: Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(candidate["activation"], "candidate_only");
        assert_eq!(candidate["command_ready"], false);
        let stdout_candidate: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(candidate, stdout_candidate);
        let retry = invoke_assignment_with_args(&args, &bytes);
        assert!(!retry.status.success());
        assert!(retry.stdout.is_empty());
        assert_eq!(std::fs::read(&file).unwrap(), original);
        std::fs::remove_file(file).unwrap();
    } else {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!file.exists());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn bounded_assignment_is_candidate_only_and_strict() {
    let output = invoke_assignment(&serde_json::to_vec(&assignment()).unwrap());
    assert!(output.status.success(), "{:?}", output.stderr);
    let candidate: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(candidate["activation"], "candidate_only");
    assert_eq!(candidate["command_ready"], false);
    assert_eq!(candidate["binding"]["control"], "button_0");
    assert!(candidate.get("events").is_none());

    let mut invalid = assignment();
    invalid["unexpected"] = json!(true);
    let output = invoke_assignment(&serde_json::to_vec(&invalid).unwrap());
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());

    let command = cli::parse(&["device-assign".into()]).unwrap();
    assert!(cli::execute(command, vec![b' '; 32_769].as_slice()).is_err());
}

struct RefuseInput;
impl Read for RefuseInput {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("offline non-assignment route must not read stdin");
    }
}

#[test]
fn offline_api_never_reads_stdin_and_health_is_explicitly_platform_scoped() {
    for args in [
        vec!["capabilities"],
        vec!["doctor"],
        vec!["device-profiles"],
        vec!["device-preview", "micro"],
        vec!["micro-preview"],
        vec!["akp03-preview"],
        vec!["micro-doctor"],
        vec!["akp03-doctor"],
    ] {
        let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
        let command = cli::parse(&args).unwrap();
        cli::execute(command, RefuseInput).unwrap();
    }
    #[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
    {
        let output = invoke(&["probe-health"]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("UnsupportedPlatform")
        );
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        let output = invoke(&["probe-health"]);
        assert!(output.status.success(), "{:?}", output.stderr);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["command_ready"], false);
        assert_eq!(value["health"]["process_group"], true);
        assert_eq!(value["health"]["pid"], value["health"]["pgid"]);
        assert_eq!(value["health"]["parent_death_guarantee"], false);
        assert_eq!(value["health"]["parent_death_watchdog"], true);
        assert_eq!(value["health"]["parent_death_poll_ms"], 50);
        assert_eq!(value["health"]["escape_guarantee"], false);
    }
    #[cfg(windows)]
    {
        let output = invoke(&["probe-health"]);
        if output.status.success() {
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["command_ready"], false);
            assert_eq!(value["health"]["in_job"], true);
            assert_eq!(value["health"]["kill_on_close"], true);
            assert_eq!(value["health"]["breakaway"], false);
        } else {
            assert!(output.stdout.is_empty());
            assert!(!output.stderr.is_empty());
        }
    }
}

#[test]
fn firmware_plan_checks_local_pins_without_executing_or_mutating_artifacts() {
    let base = std::path::PathBuf::from(std::env::var_os("COPILOT_FIRMWARE_FIXTURE_ROOT").unwrap());
    let root = base.join(format!("cli-plan-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let image = root.join("image");
    let tool = root.join("tool");
    std::fs::write(&image, b"not the reviewed image").unwrap();
    std::fs::write(&tool, b"not executable and not the reviewed tool").unwrap();
    let output = invoke(&[
        "micro-firmware-plan",
        "--image",
        image.to_str().unwrap(),
        "--esptool",
        tool.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("offline firmware validation failed")
    );
    assert_eq!(std::fs::read(&image).unwrap(), b"not the reviewed image");
    assert_eq!(
        std::fs::read(&tool).unwrap(),
        b"not executable and not the reviewed tool"
    );
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
    std::fs::remove_file(image).unwrap();
    std::fs::remove_file(tool).unwrap();
    std::fs::remove_dir(root).unwrap();
}
