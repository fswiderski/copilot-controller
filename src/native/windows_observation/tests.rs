use super::*;
use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};
use uuid::Uuid;

mod probes;

fn epoch() -> Uuid {
    Uuid::from_u128(1)
}
fn stamp(ticks: u64, frequency: u64) -> MonotonicStamp {
    MonotonicStamp {
        epoch: epoch(),
        ticks,
        frequency,
    }
}
fn identity() -> ProcessIdentity {
    ProcessIdentity {
        pid: 123,
        creation_time_100ns: 456,
        session_id: 3,
    }
}
fn names() -> ExecutableNames<'static> {
    ExecutableNames::new("agent.exe", "terminal.exe").unwrap()
}
fn evidence(id: ProcessIdentity, class: ExecutableClass) -> ProcessEvidence {
    ProcessEvidence {
        identity: id,
        class,
        stamp: stamp(100, 10),
    }
}
fn error(kind: ErrorKind) -> PlatformError {
    PlatformError::new(kind, "synthetic")
}
fn little_endian(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|u| u.to_le_bytes()).collect()
}

#[test]
fn native_errors_remain_typed_and_privacy_safe() {
    for code in [5, 0x8007_0005] {
        let error = windows_native_error("native_operation", code);
        assert_eq!(error.kind, ErrorKind::AccessDenied);
        assert_eq!(error.native_code, Some(code));
    }
    assert_eq!(
        windows_native_error("native_operation", 2).kind,
        ErrorKind::NativeFailure
    );
    assert_eq!(
        configuration_manager_error("configuration_manager", 51).kind,
        ErrorKind::AccessDenied
    );
    assert_eq!(
        configuration_manager_error("configuration_manager", 26).kind,
        ErrorKind::LimitExceeded
    );
    let error = windows_native_error("native_operation", 0x1234);
    assert_eq!(
        error.to_string(),
        "native_operation: NativeFailure (0x1234)"
    );
    assert_eq!(
        serde_json::to_value(error).unwrap(),
        serde_json::json!({"kind": "native_failure", "operation": "native_operation", "native_code": 4660})
    );
}

#[test]
fn executable_classifier_is_exact_and_ascii_only() {
    for value in [
        "agent.exe",
        "AGENT.EXE",
        "agent.exe.old-123-456",
        "AgEnT.ExE.OLD-0-09",
    ] {
        assert_eq!(names().classify(value), ExecutableClass::Cli, "{value}");
    }
    assert_eq!(
        names().classify("TERMINAL.EXE"),
        ExecutableClass::NativeHost
    );
    for value in [
        "agent.exe2",
        "agent.exe.old--2",
        "agent.exe.old-1-",
        "agent.exe.old-1-2-extra",
        "agent.exe.old-1-a",
        "agent.exe.old-١-2",
        "agent.exe ",
        "C:\\agent.exe",
        "terminal.exe.old-1-2",
        "é",
    ] {
        assert_eq!(names().classify(value), ExecutableClass::Other, "{value}");
    }
    assert!(ExecutableNames::new("C:\\agent.exe", "terminal.exe").is_err());
    assert!(ExecutableNames::new("same", "SAME").is_err());
}

#[test]
fn lifetime_foreground_and_desktop_matching_fail_closed() {
    let id = identity();
    let process = Observation::Observed(evidence(id, ExecutableClass::NativeHost));
    assert_eq!(match_process(id, &process), TargetMatch::Match);
    for expected in [
        ProcessIdentity { pid: 124, ..id },
        ProcessIdentity {
            creation_time_100ns: 457,
            ..id
        },
        ProcessIdentity {
            session_id: 4,
            ..id
        },
        ProcessIdentity { pid: 0, ..id },
        ProcessIdentity {
            creation_time_100ns: 0,
            ..id
        },
    ] {
        assert_eq!(match_process(expected, &process), TargetMatch::WrongTarget);
    }
    for kind in [
        ErrorKind::AccessDenied,
        ErrorKind::ProcessExited,
        ErrorKind::ObservationChanged,
        ErrorKind::NativeFailure,
    ] {
        assert_eq!(
            match_process(id, &Observation::Unavailable(error(kind))),
            TargetMatch::Unavailable
        );
    }
    let foreground = ForegroundEvidence {
        window: 42,
        owner_thread_id: 50,
        owner: evidence(id, ExecutableClass::NativeHost),
    };
    assert_eq!(
        match_foreground(42, id, &Observation::Observed(foreground.clone())),
        TargetMatch::Match
    );
    assert_eq!(
        match_foreground(99, id, &Observation::Observed(foreground.clone())),
        TargetMatch::WrongTarget
    );
    for changed in [
        ForegroundEvidence {
            owner_thread_id: 0,
            ..foreground.clone()
        },
        ForegroundEvidence {
            owner: evidence(
                ProcessIdentity {
                    session_id: 4,
                    ..id
                },
                ExecutableClass::NativeHost,
            ),
            ..foreground.clone()
        },
        ForegroundEvidence {
            owner: evidence(
                ProcessIdentity {
                    creation_time_100ns: 999,
                    ..id
                },
                ExecutableClass::NativeHost,
            ),
            ..foreground.clone()
        },
        ForegroundEvidence {
            owner: evidence(id, ExecutableClass::Cli),
            ..foreground.clone()
        },
    ] {
        assert_eq!(
            match_foreground(42, id, &Observation::Observed(changed)),
            TargetMatch::WrongTarget
        );
    }
    assert_eq!(
        match_foreground(
            42,
            id,
            &Observation::Unavailable(error(ErrorKind::NoForeground))
        ),
        TargetMatch::Unavailable
    );
    let desktop = DesktopContext {
        process_session_id: 3,
        active_console_session_id: Some(3),
        active: true,
        local: true,
        interactive_station: true,
        thread_default_desktop: true,
        input_default_desktop: true,
        stamp: stamp(100, 10),
    };
    assert_eq!(
        match_desktop(3, &Observation::Observed(desktop.clone())),
        ContextMatch::Match
    );
    for changed in [
        DesktopContext {
            process_session_id: 4,
            ..desktop.clone()
        },
        DesktopContext {
            active_console_session_id: None,
            ..desktop.clone()
        },
        DesktopContext {
            active_console_session_id: Some(4),
            ..desktop.clone()
        },
        DesktopContext {
            active: false,
            ..desktop.clone()
        },
        DesktopContext {
            local: false,
            ..desktop.clone()
        },
        DesktopContext {
            interactive_station: false,
            ..desktop.clone()
        },
        DesktopContext {
            thread_default_desktop: false,
            ..desktop.clone()
        },
        DesktopContext {
            input_default_desktop: false,
            ..desktop.clone()
        },
    ] {
        assert_eq!(
            match_desktop(3, &Observation::Observed(changed)),
            ContextMatch::UnsafeContext
        );
    }
    assert_eq!(
        match_desktop(0, &Observation::Observed(desktop)),
        ContextMatch::UnsafeContext
    );
    assert_eq!(
        match_desktop(3, &Observation::Unavailable(error(ErrorKind::AccessDenied))),
        ContextMatch::Unavailable
    );
}

#[test]
fn conservative_token_context_checks_both_tokens() {
    let token = TokenContext {
        session_id: 3,
        integrity_rid: 0x2000,
        elevated: false,
        ui_access: false,
        app_container: false,
    };
    let security = SecurityEvidence {
        same_user: true,
        same_logon: true,
        observer: token,
        target: token,
    };
    assert_eq!(
        match_security(3, &Observation::Observed(security.clone())),
        ContextMatch::Match
    );
    for changed in [
        TokenContext {
            session_id: 0,
            ..token
        },
        TokenContext {
            session_id: 4,
            ..token
        },
        TokenContext {
            integrity_rid: 0,
            ..token
        },
        TokenContext {
            integrity_rid: 0x1000,
            ..token
        },
        TokenContext {
            integrity_rid: 0x2100,
            ..token
        },
        TokenContext {
            integrity_rid: 0x3000,
            ..token
        },
        TokenContext {
            elevated: true,
            ..token
        },
        TokenContext {
            ui_access: true,
            ..token
        },
        TokenContext {
            app_container: true,
            ..token
        },
    ] {
        assert_eq!(
            match_security(
                3,
                &Observation::Observed(SecurityEvidence {
                    observer: changed,
                    ..security.clone()
                })
            ),
            ContextMatch::UnsafeContext
        );
        assert_eq!(
            match_security(
                3,
                &Observation::Observed(SecurityEvidence {
                    target: changed,
                    ..security.clone()
                })
            ),
            ContextMatch::UnsafeContext
        );
    }
    assert_eq!(
        match_security(
            3,
            &Observation::Observed(SecurityEvidence {
                same_user: false,
                ..security.clone()
            })
        ),
        ContextMatch::UnsafeContext
    );
    assert_eq!(
        match_security(
            3,
            &Observation::Observed(SecurityEvidence {
                same_logon: false,
                ..security.clone()
            })
        ),
        ContextMatch::UnsafeContext
    );
    assert_eq!(
        match_security(0, &Observation::Observed(security)),
        ContextMatch::UnsafeContext
    );
}

#[test]
fn synthetic_token_snapshots_detect_mutation_and_hide_identifiers() {
    use super::qualification::{TokenIds, TokenSnapshot, stable_security};
    let token = TokenContext {
        session_id: 3,
        integrity_rid: 0x2000,
        elevated: false,
        ui_access: false,
        app_container: false,
    };
    let snapshot = TokenSnapshot {
        ids: TokenIds {
            token: (1, 0),
            modified: (2, 0),
            authentication: (3, 0),
        },
        sid: vec![1, 2, 3],
        context: token,
    };
    let evidence = stable_security(
        snapshot.clone(),
        snapshot.clone(),
        snapshot.clone(),
        snapshot.clone(),
    )
    .unwrap();
    assert!(evidence.same_user && evidence.same_logon);
    let json = serde_json::to_string(&evidence).unwrap();
    for private in ["sid", "authentication", "modified", "token_id"] {
        assert!(!json.contains(private));
    }

    for change in 0..5 {
        let mut modified = snapshot.clone();
        match change {
            0 => modified.ids.token.0 += 1,
            1 => modified.ids.modified.0 += 1,
            2 => modified.ids.authentication.0 += 1,
            3 => modified.sid.push(4),
            _ => modified.context.ui_access = true,
        }
        assert_eq!(
            stable_security(
                snapshot.clone(),
                snapshot.clone(),
                snapshot.clone(),
                modified
            )
            .unwrap_err()
            .kind,
            ErrorKind::ObservationChanged
        );
    }
    let mut different = snapshot.clone();
    different.sid.push(4);
    different.ids.authentication.0 += 1;
    let evidence =
        stable_security(snapshot.clone(), different.clone(), snapshot, different).unwrap();
    assert!(!evidence.same_user && !evidence.same_logon);
}

#[test]
fn exact_usb_ids_and_com_ports() {
    for value in [
        "USB\\VID_2EC2&PID_0003",
        "usb\\vid_2ec2&pid_0003",
        "USB\\VID_2EC2&PID_0003&REV_0001",
        "USB\\VID_2EC2&PID_0003&MI_00",
        "USB\\VID_2EC2&PID_0003&REV_00aF&MI_12",
        "USB\\VID_2EC2&PID_0003&MI_FF&REV_FFFF",
    ] {
        assert!(matches_serial_hardware_id(value), "{value}");
    }
    for value in [
        "USB\\VID_2EC2&PID_00030",
        "USB\\VID_2EC20&PID_0003",
        "USB\\VID_2EC2&PID_0004",
        "HID\\VID_2EC2&PID_0003",
        "USB\\VID_2EC2&PID_0003\\serial",
        "USB\\VID_2EC2&PID_0003&PID_0003",
        "USB\\VID_2EC2&PID_0003&MI_ZZ",
        "USB\\VID_2EC2&PID_0003&MI_00&MI_01",
        "USB\\VID_2EC2&PID_0003&REV_1",
        "\\\\?\\USB#VID_2EC2&PID_0003",
        " USB\\VID_2EC2&PID_0003",
    ] {
        assert!(!matches_serial_hardware_id(value), "{value}");
    }
    for (input, output) in [
        ("COM3", "COM3"),
        ("com17", "COM17"),
        ("COM65535", "COM65535"),
    ] {
        assert_eq!(normalize_com_port(input).as_deref(), Some(output));
    }
    for value in [
        "COM0",
        "COM03",
        "COM65536",
        "COM+3",
        "COM3extra",
        " COM3",
        "COM3 ",
        "COM",
        "\\\\.\\COM3",
        "COM３",
    ] {
        assert!(normalize_com_port(value).is_none(), "{value}");
    }
}

fn fact(present: bool, active: bool, hardware_associated: bool, port: Option<&str>) -> SerialFact {
    SerialFact {
        present,
        active,
        hardware_associated,
        port: port.map(str::to_owned),
    }
}

#[test]
fn serial_selection_preserves_ambiguity_and_discards_invalid_raw_values() {
    let valid = fact(true, true, true, Some("com17"));
    let excluded = [
        fact(false, true, true, Some("COM1")),
        fact(true, false, true, Some("COM2")),
        fact(true, true, false, Some("COM3")),
    ];
    let selected = select_serial(
        &[
            valid,
            fact(false, true, true, Some("COM1")),
            fact(true, false, true, Some("COM2")),
            fact(true, true, false, Some("COM3")),
        ],
        true,
    );
    assert_eq!(selected.state, SerialSelectionState::Unique);
    assert_eq!(selected.present_count, 3);
    assert_eq!(selected.matching_count, 1);
    assert_eq!(selected.selected_port.as_deref(), Some("COM17"));
    assert_eq!(
        select_serial(&excluded, true).state,
        SerialSelectionState::NotFound
    );
    let duplicate = select_serial(
        &[
            fact(true, true, true, Some("COM3")),
            fact(true, true, true, Some("com3")),
        ],
        true,
    );
    assert_eq!(duplicate.state, SerialSelectionState::Ambiguous);
    assert_eq!(duplicate.ports, ["COM3", "COM3"]);
    assert!(duplicate.selected_port.is_none());
    let incomplete = select_serial(
        &[fact(true, true, true, Some("\\\\secret-device-path"))],
        true,
    );
    assert_eq!(incomplete.state, SerialSelectionState::Incomplete);
    assert!(
        !serde_json::to_string(&incomplete)
            .unwrap()
            .contains("secret")
    );
    assert_eq!(
        select_serial(&[], false).state,
        SerialSelectionState::Incomplete
    );
    assert_eq!(
        select_serial(&[fact(true, true, true, None)], true).state,
        SerialSelectionState::Incomplete
    );
}

#[test]
fn strict_registry_decoders_reject_hidden_or_malformed_data() {
    assert_eq!(
        decode_registry_sz(1, &little_endian(&[67, 79, 77, 51, 0])).unwrap(),
        "COM3"
    );
    for units in [
        &[67, 79, 77, 51][..],
        &[0xd800, 0][..],
        &[67, 0, 80, 0][..],
        &[][..],
    ] {
        assert!(decode_registry_sz(1, &little_endian(units)).is_err());
    }
    assert!(decode_registry_sz(7, &little_endian(&[0])).is_err());
    assert!(decode_registry_sz(1, &[1]).is_err());
    assert_eq!(
        decode_registry_sz(1, &[0; 130]).unwrap_err().kind,
        ErrorKind::LimitExceeded
    );
    assert_eq!(
        decode_registry_multi_sz(7, &little_endian(&[65, 0, 66, 0, 0])).unwrap(),
        ["A", "B"]
    );
    assert!(
        decode_registry_multi_sz(7, &little_endian(&[0, 0]))
            .unwrap()
            .is_empty()
    );
    for units in [
        &[65, 0][..],
        &[65, 0, 0, 66, 0, 0][..],
        &[0xd800, 0, 0][..],
        &[0][..],
        &[][..],
    ] {
        assert!(decode_registry_multi_sz(7, &little_endian(units)).is_err());
    }
    assert!(decode_registry_multi_sz(1, &little_endian(&[0, 0])).is_err());
}

#[test]
fn exact_age_and_request_schema_boundaries() {
    assert!(
        check_age(
            stamp(100, 10_000_000),
            stamp(200, 10_000_000),
            Duration::from_secs(3)
        )
        .is_ok()
    );
    for now in [99, 100, 101] {
        assert_eq!(
            check_age(stamp(100, 10), stamp(now, 10), Duration::from_secs(3)),
            Err(AgeError::ClockMismatch)
        );
    }
    for now in [
        MonotonicStamp {
            epoch: Uuid::from_u128(2),
            ..stamp(200, 10)
        },
        stamp(200, 0),
        stamp(200, 11),
    ] {
        assert_eq!(
            check_age(stamp(100, 10), now, Duration::from_secs(3)),
            Err(AgeError::ClockMismatch)
        );
    }
    assert!(
        check_age(
            stamp(0, 2_000_000_000),
            stamp(6_000_000_000, 2_000_000_000),
            Duration::from_secs(3)
        )
        .is_ok()
    );
    assert_eq!(
        check_age(
            stamp(0, 2_000_000_000),
            stamp(6_000_000_001, 2_000_000_000),
            Duration::from_secs(3)
        ),
        Err(AgeError::StaleState)
    );
    assert!(
        check_age(
            stamp(0, u64::MAX),
            stamp(u64::MAX, u64::MAX),
            Duration::from_secs(1)
        )
        .is_ok()
    );
    assert!(
        check_probe_interval(
            stamp(100, 10),
            stamp(110, 10),
            stamp(120, 10),
            stamp(130, 10)
        )
        .is_ok()
    );
    assert!(
        check_probe_interval(
            stamp(100, 10),
            stamp(110, 10),
            stamp(200, 10),
            stamp(220, 10)
        )
        .is_ok()
    );
    assert_eq!(
        check_probe_interval(
            stamp(100, 10),
            stamp(110, 10),
            stamp(200, 10),
            stamp(221, 10)
        ),
        Err(AgeError::StaleState)
    );
    assert_eq!(
        check_probe_interval(
            stamp(100, 10),
            stamp(101, 10),
            stamp(120, 10),
            stamp(130, 10)
        ),
        Err(AgeError::ClockMismatch)
    );
    assert!(stamp_from_timespec(epoch(), 1, 20).is_ok_and(|s| s.ticks == 1_000_000_020));
    for (s, n) in [(-1, 0), (0, -1), (0, 1_000_000_000), (i64::MAX, 0)] {
        assert!(stamp_from_timespec(epoch(), s, n).is_err());
    }
    assert!(observe_clock(Uuid::nil()).is_err());
    let request = ProcessRequest {
        target: identity(),
        requested_at: stamp(100, 10),
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<ProcessRequest>(value.clone()).unwrap(),
        request
    );
    assert!(
        validate_process_request(
            &ProcessRequest {
                target: ProcessIdentity {
                    session_id: 0,
                    ..identity()
                },
                ..request.clone()
            },
            stamp(102, 10)
        )
        .is_ok()
    );
    assert_eq!(
        validate_process_request(
            &ProcessRequest {
                target: ProcessIdentity {
                    pid: 0,
                    ..identity()
                },
                ..request.clone()
            },
            stamp(102, 10)
        )
        .unwrap_err()
        .operation,
        "process_request_identity"
    );
    for field in ["title", "text", "action", "command", "port", "read_text"] {
        let mut altered = value.clone();
        altered[field] = serde_json::json!("secret");
        assert!(
            serde_json::from_value::<ProcessRequest>(altered).is_err(),
            "{field}"
        );
    }
    let mut altered = value.clone();
    altered["target"]["extra"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ProcessRequest>(altered).is_err());
    for invalid_pid in [
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!("123"),
    ] {
        let mut altered = value.clone();
        altered["target"]["pid"] = invalid_pid;
        assert!(serde_json::from_value::<ProcessRequest>(altered).is_err());
    }
    let mut altered = value.clone();
    altered["requested_at"]["epoch"] = serde_json::json!("00000000000000000000000000000001");
    assert!(serde_json::from_value::<ProcessRequest>(altered).is_err());
    let mut ui = serde_json::json!({"process": value, "window": 42});
    ui["title"] = serde_json::json!("never accepted");
    assert!(serde_json::from_value::<UiRequest>(ui).is_err());
}

#[test]
fn image_paths_and_sid_bounds_are_checked_without_exposing_values() {
    use super::qualification::bounded_sid_bytes;
    let path: Vec<u16> = "C:\\x".encode_utf16().collect();
    assert!(is_drive_absolute_image_path(&path));
    for path in ["C:x", "\\\\server\\x", "\\\\?\\C:\\x", "1:\\x", "C:\\"] {
        assert!(!is_drive_absolute_image_path(
            &path.encode_utf16().collect::<Vec<_>>()
        ));
    }
    let mut sid = vec![1, 1, 0, 0, 0, 0, 0, 16, 0, 32, 0, 0];
    assert_eq!(bounded_sid_bytes(&sid, 0).unwrap().len(), 12);
    assert!(bounded_sid_bytes(&sid, 1).is_err());
    sid[1] = 16;
    assert!(bounded_sid_bytes(&sid, 0).is_err());
    sid[1] = 2;
    assert!(bounded_sid_bytes(&sid, 0).is_err());
}

struct FakeImage {
    paths: VecDeque<Vec<u16>>,
    records: VecDeque<super::qualification::ImageFileRecord>,
    opened: usize,
}

impl super::qualification::ImageFileBackend for FakeImage {
    type Process = u32;
    type File = usize;
    fn check_running(&mut self, _: &u32) -> PlatformResult<()> {
        Ok(())
    }
    fn image_path(&mut self, _: &u32) -> PlatformResult<Vec<u16>> {
        Ok(self.paths.pop_front().unwrap())
    }
    fn open_attributes(&mut self, _: &[u16]) -> PlatformResult<usize> {
        self.opened += 1;
        Ok(self.opened)
    }
    fn information(&mut self, _: &usize) -> PlatformResult<super::qualification::ImageFileRecord> {
        Ok(self.records.pop_front().unwrap())
    }
}

fn image_record() -> super::qualification::ImageFileRecord {
    super::qualification::ImageFileRecord {
        attributes: 0,
        creation_time: 1,
        last_access_time: 2,
        last_write_time: 3,
        volume_serial: 4,
        size_high: 0,
        size_low: 8,
        link_count: 1,
        index_high: 0,
        index_low: 9,
    }
}

fn fake_image() -> FakeImage {
    let path: Vec<u16> = "C:\\private-image.exe".encode_utf16().collect();
    FakeImage {
        paths: [path.clone(), path.clone(), path.clone(), path].into(),
        records: [image_record(); 4].into(),
        opened: 0,
    }
}

#[test]
fn synthetic_image_comparison_checks_paths_before_open_and_detects_changes() {
    use super::qualification::collect_image_file;
    let mut image = fake_image();
    let evidence = collect_image_file(&mut image, &1, &2).unwrap();
    assert!(evidence.same_file_as_observer && evidence.path_file_stable_during_probe);
    assert_eq!(evidence.provenance, Provenance::Unverified);
    assert_eq!(image.opened, 2);
    assert!(
        !serde_json::to_string(&evidence)
            .unwrap()
            .contains("private-image")
    );

    let mut different_identity = fake_image();
    different_identity.records[1].index_low = 10;
    different_identity.records[3].index_low = 10;
    assert!(
        !collect_image_file(&mut different_identity, &1, &2)
            .unwrap()
            .same_file_as_observer
    );

    let mut different = fake_image();
    different.paths[1] = "C:\\other.exe".encode_utf16().collect();
    assert_eq!(
        collect_image_file(&mut different, &1, &2)
            .unwrap_err()
            .operation,
        "external_image_provenance_unqualified"
    );
    assert_eq!(different.opened, 0);

    let mut changed_path = fake_image();
    changed_path.paths[2] = "C:\\changed.exe".encode_utf16().collect();
    assert_eq!(
        collect_image_file(&mut changed_path, &1, &2)
            .unwrap_err()
            .kind,
        ErrorKind::ObservationChanged
    );

    let mut changed_record = fake_image();
    changed_record.records[2].last_write_time += 1;
    assert_eq!(
        collect_image_file(&mut changed_record, &1, &2)
            .unwrap_err()
            .operation,
        "image_file_identity"
    );

    let mut invalid = fake_image();
    invalid.records[0].link_count = 0;
    assert_eq!(
        collect_image_file(&mut invalid, &1, &2).unwrap_err().kind,
        ErrorKind::InvalidData
    );
}

struct FakeInventory {
    entries: VecDeque<(u32, String)>,
}
impl ProcessInventoryBackend for FakeInventory {
    fn next_process(&mut self) -> PlatformResult<Option<(u32, String)>> {
        Ok(self.entries.pop_front())
    }
    fn inspect(&mut self, pid: u32) -> PlatformResult<ProcessEvidence> {
        match pid {
            2 => Err(error(ErrorKind::AccessDenied)),
            3 => Ok(evidence(
                ProcessIdentity { pid, ..identity() },
                ExecutableClass::Other,
            )),
            _ => Ok(evidence(
                ProcessIdentity { pid, ..identity() },
                ExecutableClass::Cli,
            )),
        }
    }
}

#[test]
fn synthetic_process_snapshot_keeps_independent_failures_and_bound() {
    let mut source = FakeInventory {
        entries: [(1, "other"), (2, "agent.exe"), (3, "agent.exe")]
            .into_iter()
            .map(|(p, n)| (p, n.to_owned()))
            .collect(),
    };
    let result = collect_process_inventory(&mut source, names()).unwrap();
    assert_eq!(result.examined_count, 3);
    assert_eq!(result.candidates.len(), 2);
    assert!(
        matches!(&result.candidates[0].inspection, Observation::Unavailable(e) if e.kind == ErrorKind::AccessDenied)
    );
    assert!(
        matches!(&result.candidates[1].inspection, Observation::Unavailable(e) if e.operation == "process_snapshot_identity")
    );
    let mut bounded = FakeInventory {
        entries: (0..16_384).map(|p| (p, "other".into())).collect(),
    };
    assert_eq!(
        collect_process_inventory(&mut bounded, names())
            .unwrap()
            .examined_count,
        16_384
    );
    let mut excessive = FakeInventory {
        entries: (0..16_385).map(|p| (p, "other".into())).collect(),
    };
    assert_eq!(
        collect_process_inventory(&mut excessive, names())
            .unwrap_err()
            .kind,
        ErrorKind::LimitExceeded
    );
}

struct FakeForeground {
    windows: VecDeque<Option<u64>>,
    owners: VecDeque<(u32, u32)>,
    running: bool,
}
impl ForegroundBackend for FakeForeground {
    type Held = u32;
    fn foreground_window(&mut self) -> PlatformResult<Option<u64>> {
        Ok(self.windows.pop_front().unwrap())
    }
    fn window_owner(&mut self, _: u64) -> PlatformResult<(u32, u32)> {
        Ok(self.owners.pop_front().unwrap())
    }
    fn hold_process(&mut self, pid: u32) -> PlatformResult<u32> {
        Ok(pid)
    }
    fn inspect_held(&mut self, pid: &u32) -> PlatformResult<ProcessEvidence> {
        Ok(evidence(
            ProcessIdentity {
                pid: *pid,
                ..identity()
            },
            ExecutableClass::NativeHost,
        ))
    }
    fn check_running(&mut self, _: &u32) -> PlatformResult<()> {
        if self.running {
            Ok(())
        } else {
            Err(error(ErrorKind::ProcessExited))
        }
    }
}

#[test]
fn synthetic_foreground_detects_races_and_process_exit() {
    let make = || FakeForeground {
        windows: [Some(42), Some(42)].into(),
        owners: [(50, 123), (50, 123)].into(),
        running: true,
    };
    assert_eq!(
        collect_foreground(&mut make()).unwrap().owner.identity,
        identity()
    );
    let mut changed = make();
    changed.windows[1] = Some(99);
    assert_eq!(
        collect_foreground(&mut changed).unwrap_err().kind,
        ErrorKind::ObservationChanged
    );
    let mut changed = make();
    changed.owners[1] = (51, 123);
    assert_eq!(
        collect_foreground(&mut changed).unwrap_err().kind,
        ErrorKind::ObservationChanged
    );
    let mut exited = make();
    exited.running = false;
    assert_eq!(
        collect_foreground(&mut exited).unwrap_err().kind,
        ErrorKind::ProcessExited
    );
    let mut missing = make();
    missing.windows[0] = None;
    assert_eq!(
        collect_foreground(&mut missing).unwrap_err().kind,
        ErrorKind::NoForeground
    );
}

struct FakeDesktop(VecDeque<DesktopSnapshot>);
impl DesktopBackend for FakeDesktop {
    fn stamp(&mut self) -> PlatformResult<MonotonicStamp> {
        Ok(stamp(100, 10))
    }
    fn process_session(&mut self) -> PlatformResult<u32> {
        Ok(3)
    }
    fn snapshot(&mut self, _: u32) -> PlatformResult<DesktopSnapshot> {
        Ok(self.0.pop_front().unwrap())
    }
}

#[test]
fn synthetic_desktop_compares_complete_raw_snapshots() {
    let snapshot = DesktopSnapshot {
        active_console_session_id: Some(3),
        connection_state: 1,
        client_protocol: 1,
        interactive_station: true,
        thread_default_desktop: true,
        input_default_desktop: true,
    };
    let mut changed = FakeDesktop(
        [
            snapshot,
            DesktopSnapshot {
                connection_state: 2,
                ..snapshot
            },
        ]
        .into(),
    );
    assert_eq!(
        collect_desktop(&mut changed).unwrap_err().kind,
        ErrorKind::ObservationChanged
    );
    let context = collect_desktop(&mut FakeDesktop([snapshot, snapshot].into())).unwrap();
    assert_eq!(
        match_desktop(3, &Observation::Observed(context)),
        ContextMatch::UnsafeContext
    );
}

#[test]
fn desktop_object_names_require_bounded_strict_terminated_data() {
    use super::process::object_name_matches;
    let default: Vec<u16> = "DeFaUlT\0".encode_utf16().collect();
    assert!(object_name_matches(&default, default.len() * 2, "Default").unwrap());
    for (units, size) in [
        (vec![68, 0, 88, 0], 8),
        (vec![68, 69], 4),
        (vec![0xd800, 0], 4),
        (vec![0], 1),
        (vec![], 0),
    ] {
        assert!(object_name_matches(&units, size, "Default").is_err());
    }
    assert_eq!(
        object_name_matches(&[0; 257], 514, "Default")
            .unwrap_err()
            .kind,
        ErrorKind::LimitExceeded
    );
}

#[derive(Clone)]
struct FakeInterface {
    present: bool,
    active: bool,
    node: u32,
}
struct FakeSerial {
    interfaces: VecDeque<FakeInterface>,
    parents: HashMap<u32, Option<u32>>,
    ids: HashMap<u32, Vec<String>>,
    ports: HashMap<u32, String>,
    queried: Vec<u32>,
}
impl SerialBackend for FakeSerial {
    type Interface = FakeInterface;
    type Associated = u32;
    type Node = u32;
    fn next_interface(&mut self) -> PlatformResult<Option<Self::Interface>> {
        Ok(self.interfaces.pop_front())
    }
    fn flags(&self, interface: &Self::Interface) -> (bool, bool) {
        (interface.present, interface.active)
    }
    fn associated_device(&mut self, interface: &Self::Interface) -> PlatformResult<u32> {
        Ok(interface.node)
    }
    fn node(&self, device: &u32) -> u32 {
        *device
    }
    fn parent(&mut self, node: u32) -> PlatformResult<Option<u32>> {
        self.parents
            .get(&node)
            .copied()
            .ok_or_else(|| error(ErrorKind::NativeFailure))
    }
    fn hardware_ids(&mut self, node: u32) -> PlatformResult<Vec<String>> {
        self.queried.push(node);
        Ok(self.ids.get(&node).cloned().unwrap_or_default())
    }
    fn port_name(&mut self, device: &u32) -> PlatformResult<Option<String>> {
        Ok(self.ports.get(device).cloned())
    }
}

fn fake_serial() -> FakeSerial {
    FakeSerial {
        interfaces: [FakeInterface {
            present: true,
            active: true,
            node: 1,
        }]
        .into(),
        parents: [(1, Some(2)), (2, Some(3)), (3, None)].into(),
        ids: [(2, vec!["USB\\VID_2EC2&PID_0003&MI_00".into()])].into(),
        ports: [(1, "com17".into())].into(),
        queried: vec![],
    }
}

#[test]
fn synthetic_serial_walk_is_bounded_and_never_uses_root() {
    let mut source = fake_serial();
    let result = collect_serial(&mut source).unwrap();
    assert_eq!(result.selected_port.as_deref(), Some("COM17"));
    assert_eq!(source.queried, [1, 2]);
    let mut cycle = fake_serial();
    cycle.parents.insert(3, Some(1));
    assert_eq!(
        collect_serial(&mut cycle).unwrap_err().operation,
        "serial_ancestor_cycle"
    );
    let mut too_deep = fake_serial();
    too_deep.parents = (1..=17)
        .map(|n| (n, Some(n + 1)))
        .chain([(18, None)])
        .collect();
    assert_eq!(
        collect_serial(&mut too_deep).unwrap_err().kind,
        ErrorKind::LimitExceeded
    );
    let mut maximum_depth = fake_serial();
    maximum_depth.parents = (1..=16)
        .map(|n| (n, Some(n + 1)))
        .chain([(17, None)])
        .collect();
    assert_eq!(
        collect_serial(&mut maximum_depth).unwrap().state,
        SerialSelectionState::Unique
    );
    assert_eq!(maximum_depth.queried.len(), 16);
    let mut maximum_interfaces = fake_serial();
    maximum_interfaces.interfaces = (0..512)
        .map(|_| FakeInterface {
            present: false,
            active: false,
            node: 1,
        })
        .collect();
    assert_eq!(
        collect_serial(&mut maximum_interfaces).unwrap().state,
        SerialSelectionState::NotFound
    );
    let mut many = fake_serial();
    many.interfaces = (0..513)
        .map(|_| FakeInterface {
            present: false,
            active: false,
            node: 1,
        })
        .collect();
    assert_eq!(
        collect_serial(&mut many).unwrap_err().operation,
        "serial_interface_enumeration"
    );
    let mut failed = fake_serial();
    failed.parents.clear();
    assert_eq!(
        collect_serial(&mut failed).unwrap_err().kind,
        ErrorKind::NativeFailure
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn unsupported_platform_is_unavailable_not_an_empty_success() {
    let report = ordinary_diagnostic(epoch(), names());
    assert!(matches!(report.clock, Observation::Observed(_)));
    let json = serde_json::to_value(&report).unwrap();
    for field in [
        "current_process",
        "processes",
        "foreground",
        "desktop",
        "serial",
    ] {
        assert_eq!(json[field]["status"], "unavailable");
        assert_eq!(json[field]["evidence"]["kind"], "unsupported_platform");
        assert_eq!(json[field]["evidence"]["operation"], "windows_platform");
    }
    assert_eq!(json["read_only"], true);
    assert_eq!(json["command_ready"], false);
    assert_eq!(json["w2_qualified"], false);
}
