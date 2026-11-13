#[cfg(windows)]
mod windows_fixture {
    use std::io::Write;
    use std::mem::{size_of, size_of_val};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::ptr::{null, null_mut};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::thread;
    use std::time::{Duration, Instant};

    use copilot_controller::native::windows_worker::{
        DiagnosticAllowlist, DiagnosticCallback, DiagnosticCallbackError, DiagnosticRequest,
        DiagnosticResult, WorkerError, WorkerErrorKind, WorkerIdentity, maybe_run_hidden_worker,
        probe,
    };
    use serde_json::{Value, json};
    use uuid::Uuid;
    use windows_sys::Win32::Foundation::{
        ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, FILETIME, GetLastError, HANDLE,
        WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
    use windows_sys::Win32::System::JobObjects::{
        IsProcessInJob, JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        QueryInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, EVENT_MODIFY_STATE, GetCurrentProcess, GetProcessHandleCount,
        GetProcessTimes, OpenEventW, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, SetEvent, WaitForSingleObject,
    };

    const KIND: &str = "fixture_diagnostic";
    static CALLBACKS: [DiagnosticCallback; 1] = [DiagnosticCallback::new(KIND, callback)];

    fn allowlist() -> DiagnosticAllowlist {
        DiagnosticAllowlist::new(&CALLBACKS).unwrap()
    }

    fn callback(payload: Value) -> Result<Value, DiagnosticCallbackError> {
        match payload["mode"].as_str().unwrap_or("") {
            "health" => {
                let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                let mut in_job = 0;
                let process = unsafe { GetCurrentProcess() };
                if unsafe { IsProcessInJob(process, null_mut(), &mut in_job) } == 0
                    || unsafe {
                        QueryInformationJobObject(
                            null_mut(),
                            JobObjectExtendedLimitInformation,
                            (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                            size_of_val(&limits) as u32,
                            null_mut(),
                        )
                    } == 0
                {
                    return Err(DiagnosticCallbackError);
                }
                let mut console_process = 0;
                let count = unsafe { GetConsoleProcessList(&mut console_process, 1) };
                let detached = count == 0
                    && unsafe { GetLastError() } == ERROR_INVALID_HANDLE
                    && unsafe { GetConsoleWindow() }.is_null();
                let flags = limits.BasicLimitInformation.LimitFlags;
                Ok(json!({
                    "detached": detached,
                    "in_job": in_job != 0,
                    "active_process_limit": limits.BasicLimitInformation.ActiveProcessLimit,
                    "active_limit_set": flags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS != 0,
                    "kill_on_close": flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0,
                    "breakaway": flags & (JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK) != 0,
                    "read_only": true,
                    "command_ready": false,
                    "w2_qualified": false
                }))
            }
            "echo" => Ok(payload["data"].clone()),
            "stall" => {
                if let Some(name) = payload["ready_event"].as_str() {
                    let name = wide(name);
                    let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
                    if event.is_null() {
                        return Err(DiagnosticCallbackError);
                    }
                    let event = unsafe { OwnedHandle::from_raw_handle(event) };
                    if unsafe { SetEvent(event.as_raw_handle()) } == 0 {
                        return Err(DiagnosticCallbackError);
                    }
                }
                thread::sleep(Duration::from_secs(60));
                Ok(json!(null))
            }
            "error" => Err(DiagnosticCallbackError),
            "panic" => panic!("PRIVATE_FIXTURE_PANIC"),
            "large" => Ok(Value::String("x".repeat(128 * 1024))),
            "stdout" => {
                let mut stdout = std::io::stdout();
                stdout.write_all(b"not-a-frame").unwrap();
                stdout.flush().unwrap();
                Ok(json!({}))
            }
            "stderr" => {
                let _ = std::io::stderr().write_all(&vec![b'x'; 32 * 1024]);
                Ok(json!({}))
            }
            "event" => {
                let raw = payload["handle"].as_u64().unwrap_or(0) as usize as HANDLE;
                let signalled = unsafe { SetEvent(raw) } != 0;
                Ok(json!({"attempt_succeeded": signalled}))
            }
            _ => Err(DiagnosticCallbackError),
        }
    }

    fn run(
        payload: Value,
        timeout: Duration,
        cancellation: &AtomicBool,
    ) -> Result<DiagnosticResult, WorkerError> {
        probe(
            &DiagnosticRequest {
                kind: KIND.to_owned(),
                payload,
            },
            timeout,
            cancellation,
            &allowlist(),
        )
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    fn named_event(name: &str) -> OwnedHandle {
        let name = wide(name);
        let event = unsafe { CreateEventW(null(), 1, 0, name.as_ptr()) };
        assert!(!event.is_null());
        unsafe { OwnedHandle::from_raw_handle(event) }
    }

    fn wait_ready(event: &OwnedHandle, timeout: Duration) -> bool {
        match unsafe { WaitForSingleObject(event.as_raw_handle(), timeout.as_millis() as u32) } {
            WAIT_OBJECT_0 => true,
            WAIT_TIMEOUT => false,
            _ => panic!("readiness wait failed with GetLastError={}", unsafe {
                GetLastError()
            }),
        }
    }

    fn lifetime_exited(identity: WorkerIdentity) -> bool {
        let process = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                identity.pid,
            )
        };
        if process.is_null() {
            return unsafe { GetLastError() } == ERROR_INVALID_PARAMETER;
        }
        let process = unsafe { OwnedHandle::from_raw_handle(process) };
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if unsafe {
            GetProcessTimes(
                process.as_raw_handle(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        } == 0
        {
            return false;
        }
        let creation =
            (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        creation != identity.creation_time_100ns
            || unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } == WAIT_OBJECT_0
    }

    fn health() {
        let result = run(
            json!({"mode": "health"}),
            Duration::from_secs(10),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(lifetime_exited(result.worker));
        assert_eq!(result.payload["detached"], true);
        assert_eq!(result.payload["in_job"], true);
        assert_eq!(result.payload["active_process_limit"], 1);
        assert_eq!(result.payload["active_limit_set"], true);
        assert_eq!(result.payload["kill_on_close"], true);
        assert_eq!(result.payload["breakaway"], false);
        assert_eq!(result.payload["read_only"], true);
        assert_eq!(result.payload["command_ready"], false);
        assert_eq!(result.payload["w2_qualified"], false);
    }

    fn assert_clean_failure_after_launch(error: WorkerError, expected: WorkerErrorKind) {
        assert_eq!(error.kind, expected);
        assert!(error.replacement_allowed);
        assert!(lifetime_exited(
            error
                .worker
                .expect("post-launch failure must include worker identity")
        ));
        assert!(!format!("{error} {error:?}").contains("PRIVATE_FIXTURE"));
        health();
    }

    fn assert_clean_failure_allow_prelaunch(error: WorkerError, expected: WorkerErrorKind) {
        assert_eq!(error.kind, expected);
        assert!(error.replacement_allowed);
        if let Some(worker) = error.worker {
            assert!(lifetime_exited(worker));
        } else {
            assert!(matches!(
                expected,
                WorkerErrorKind::TimedOut | WorkerErrorKind::Cancelled
            ));
        }
        assert!(!format!("{error} {error:?}").contains("PRIVATE_FIXTURE"));
        health();
    }

    fn handle_count() -> u32 {
        let mut count = 0;
        assert_ne!(
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) },
            0
        );
        count
    }

    fn suite() {
        health();

        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 1,
        };
        let event = unsafe { CreateEventW(&attributes, 1, 0, null()) };
        assert!(!event.is_null());
        let event = unsafe { OwnedHandle::from_raw_handle(event) };
        let result = run(
            json!({"mode": "event", "handle": event.as_raw_handle() as usize as u64}),
            Duration::from_secs(10),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(lifetime_exited(result.worker));
        assert_eq!(
            unsafe { WaitForSingleObject(event.as_raw_handle(), 0) },
            WAIT_TIMEOUT
        );
        drop(event);

        let echo = "s".repeat(48 * 1024);
        let result = run(
            json!({"mode": "echo", "data": echo}),
            Duration::from_secs(10),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(result.payload, echo);
        assert!(lifetime_exited(result.worker));

        let started = Instant::now();
        let error = run(
            json!({"mode": "stall"}),
            Duration::from_millis(750),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_clean_failure_allow_prelaunch(error, WorkerErrorKind::TimedOut);

        let cancellation = Arc::new(AtomicBool::new(false));
        let ready_event_name = format!("Local\\copilot-controller-worker-ready-{}", Uuid::new_v4());
        let ready_event = named_event(&ready_event_name);
        let cancellation_for_probe = Arc::clone(&cancellation);
        let probe_worker = thread::spawn(move || {
            run(
                json!({"mode": "stall", "ready_event": ready_event_name}),
                Duration::from_secs(10),
                &cancellation_for_probe,
            )
        });
        if !wait_ready(&ready_event, Duration::from_secs(5)) {
            cancellation.store(true, Ordering::Release);
            let result = probe_worker.join().unwrap();
            panic!("cancellation readiness handshake timed out: {result:?}");
        }
        cancellation.store(true, Ordering::Release);
        let error = probe_worker.join().unwrap().unwrap_err();
        assert_clean_failure_after_launch(error, WorkerErrorKind::Cancelled);

        let prelaunch_cancelled = AtomicBool::new(true);
        let error = run(
            json!({"mode": "health"}),
            Duration::from_secs(10),
            &prelaunch_cancelled,
        )
        .unwrap_err();
        assert_eq!(error.kind, WorkerErrorKind::Cancelled);
        assert!(error.replacement_allowed);
        assert!(error.worker.is_none());

        for (mode, expected) in [
            ("error", WorkerErrorKind::Callback),
            ("panic", WorkerErrorKind::Callback),
            ("large", WorkerErrorKind::FrameLimit),
            ("stdout", WorkerErrorKind::FrameLimit),
            ("stderr", WorkerErrorKind::DiagnosticLimit),
        ] {
            let error = run(
                json!({"mode": mode}),
                Duration::from_secs(10),
                &AtomicBool::new(false),
            )
            .unwrap_err();
            assert_clean_failure_after_launch(error, expected);
        }

        let busy_ready_name = format!("Local\\copilot-controller-worker-busy-{}", Uuid::new_v4());
        let busy_ready_event = named_event(&busy_ready_name);
        let cancellation = Arc::new(AtomicBool::new(false));
        let other_cancel = Arc::clone(&cancellation);
        let stalled = thread::spawn(move || {
            loop {
                let result = run(
                    json!({"mode": "stall", "ready_event": busy_ready_name}),
                    Duration::from_secs(10),
                    &other_cancel,
                );
                match result {
                    Err(error)
                        if error.kind == WorkerErrorKind::Busy
                            && !other_cancel.load(Ordering::Acquire) =>
                    {
                        thread::sleep(Duration::from_millis(10));
                    }
                    result => return result,
                }
            }
        });
        if !wait_ready(&busy_ready_event, Duration::from_secs(5)) {
            cancellation.store(true, Ordering::Release);
            let result = stalled.join().unwrap();
            panic!("busy readiness handshake timed out: {result:?}");
        }
        let started = Instant::now();
        loop {
            match run(
                json!({"mode": "health"}),
                Duration::from_secs(10),
                &AtomicBool::new(false),
            ) {
                Err(error) if error.kind == WorkerErrorKind::Busy => {
                    assert!(!error.replacement_allowed);
                    break;
                }
                Ok(_) if started.elapsed() < Duration::from_secs(3) => {
                    thread::sleep(Duration::from_millis(10))
                }
                other => panic!("concurrent fence failed: {other:?}"),
            }
        }
        cancellation.store(true, Ordering::Release);
        let error = stalled.join().unwrap().unwrap_err();
        assert_clean_failure_after_launch(error, WorkerErrorKind::Cancelled);

        health();
        let before = handle_count();
        for _ in 0..5 {
            health();
        }
        let after = handle_count();
        assert!(
            after <= before + 1,
            "handle count grew: {before} -> {after}"
        );
    }

    pub fn main() {
        if let Some(result) = maybe_run_hidden_worker(std::env::args_os().skip(1), &allowlist()) {
            std::process::exit(if result.is_ok() { 0 } else { 1 });
        }
        assert_eq!(std::env::args().nth(1).as_deref(), Some("suite"));
        suite();
        println!("synthetic Windows worker suite passed");
    }
}

#[cfg(windows)]
fn main() {
    windows_fixture::main();
}

#[cfg(not(windows))]
fn main() {}
