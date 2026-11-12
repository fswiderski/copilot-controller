use std::{
    fs,
    os::unix::{
        fs::{DirBuilderExt, symlink},
        process::ExitStatusExt,
    },
    path::PathBuf,
};

use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::var_os("PORTABLE_TEST_FIXTURES")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target"));
        let path = root.join(format!("macos-worker-reliability-{}", Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn record_identity(value: &Value) -> WorkerIdentity {
    WorkerIdentity {
        pid: u32::try_from(value["pid"].as_u64().unwrap()).unwrap(),
        creation_time_100ns: value["creation_time_100ns"].as_u64().unwrap(),
    }
}

fn wait_for_record(path: &Path, deadline: Instant) -> Value {
    loop {
        if let Ok(bytes) = fs::read(path)
            && let Ok(record) = serde_json::from_slice::<Value>(&bytes)
        {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "fixture did not publish identity"
        );
        thread::sleep(POLL);
    }
}

fn lifetime_gone(worker: WorkerIdentity) -> bool {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of_val(&info) as i32;
    let count = unsafe {
        libc::proc_pidinfo(
            worker.pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if count == size {
        process_lifetime(&unsafe { info.assume_init() }).is_ok_and(|current| current != worker)
    } else {
        count == 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
}

struct RetainedProcess {
    worker: WorkerIdentity,
    pgid: libc::pid_t,
}

impl RetainedProcess {
    fn assert_gone(&self, deadline: Instant) {
        while !lifetime_gone(self.worker) && Instant::now() < deadline {
            thread::sleep(POLL);
        }
        assert!(
            lifetime_gone(self.worker),
            "process exit/reap remains unconfirmed"
        );
    }
}

impl Drop for RetainedProcess {
    fn drop(&mut self) {
        if process_info(self.worker.pid as i32).is_ok_and(|info| {
            process_lifetime(&info) == Ok(self.worker) && info.pbi_pgid == self.pgid as u32
        }) {
            // Rescue only the retained fixture lifetime, never a reused PID.
            unsafe { libc::kill(self.worker.pid as i32, libc::SIGKILL) };
        }
    }
}

#[test]
#[ignore = "requires self-authored MACOS_WORKER_TEST_HELPER"]
fn symlink_launched_same_executable_retains_native_capability() {
    let _serial = FIXTURE_TESTS.lock().unwrap();
    let fixture = Fixture::new();
    let executable = PathBuf::from(std::env::var_os("MACOS_WORKER_TEST_HELPER").unwrap())
        .canonicalize()
        .unwrap();
    let alias = fixture.path("worker-link");
    symlink(&executable, &alias).unwrap();
    assert_eq!(same_executable_paths(&executable, &alias), Ok(()));
    assert_eq!(same_executable_paths(&alias, &executable), Ok(()));
    assert_eq!(
        same_executable_paths(&alias, &fixture.path("missing")),
        Err(Kind::Containment)
    );
    assert_eq!(
        same_executable_paths(&fixture.path("missing"), &alias),
        Err(Kind::Containment)
    );
    let mut coordinator = Command::new(&alias);
    coordinator
        .arg("round-trip")
        .env("MACOS_WORKER_FIXTURE_MODE", "parent-gate");
    let bytes =
        crate::native::portable::child::capture(&mut coordinator, Duration::from_secs(5)).unwrap();
    let result: DiagnosticResult = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        result.payload["native_access_denied"],
        !cfg!(any(feature = "micro-hid", feature = "akp03-hid"))
    );
}

#[test]
#[ignore = "requires self-authored MACOS_WORKER_TEST_HELPER"]
fn parent_termination_exits_and_reaps_separate_group_blocked_worker() {
    let _serial = FIXTURE_TESTS.lock().unwrap();
    for signal in [libc::SIGINT, libc::SIGKILL] {
        let fixture = Fixture::new();
        let worker_path = fixture.path("worker.json");
        let parent_result = fixture.path("parent-result.json");
        let mut command = Command::new(std::env::var_os("MACOS_WORKER_TEST_HELPER").unwrap());
        command
            .arg("orphan-parent")
            .env("MACOS_WORKER_FIXTURE_MODE", "blocked-main")
            .env("MACOS_WORKER_RECORD", &worker_path)
            .env("MACOS_PARENT_RESULT", &parent_result)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().unwrap();
        let parent_pid = child.id() as i32;
        let mut parent = GroupGuard::new(child, parent_pid);
        let record = wait_for_record(&worker_path, Instant::now() + Duration::from_secs(2));
        let retained = RetainedProcess {
            worker: record_identity(&record),
            pgid: record["pgid"].as_u64().unwrap() as i32,
        };
        assert_eq!(retained.pgid as u32, retained.worker.pid);
        assert_ne!(retained.pgid, parent_pid);
        assert!(
            process_info(retained.worker.pid as i32)
                .is_ok_and(|info| info.pbi_status != libc::SZOMB)
        );
        let started = Instant::now();
        assert_eq!(unsafe { libc::kill(parent_pid, signal) }, 0);
        let deadline = started + Duration::from_secs(2);
        while !peek_exited(parent_pid, deadline).unwrap() && Instant::now() < deadline {
            thread::sleep(POLL);
        }
        assert!(peek_exited(parent_pid, deadline).unwrap());
        assert!(!parent.finish().unwrap().success());
        retained.assert_gone(deadline);
        assert!(
            !parent_result.exists(),
            "dead supervisor must not publish replacement eligibility"
        );
    }
}

#[test]
#[ignore = "requires self-authored MACOS_WORKER_TEST_HELPER"]
fn exited_worker_with_pipe_holding_descendant_is_killed_before_replacement() {
    let _serial = FIXTURE_TESTS.lock().unwrap();
    let fixture = Fixture::new();
    let path = fixture.path("descendant.json");
    let mut command = helper("hold-pipes", true);
    command.env("MACOS_DESCENDANT_RECORD", &path);
    let nonce = Uuid::new_v4();
    let started = Instant::now();
    let error = supervise_command(
        &mut command,
        frame(&request(true, false), nonce),
        nonce,
        Duration::from_millis(300),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    let record = wait_for_record(&path, Instant::now() + Duration::from_secs(1));
    let descendant = RetainedProcess {
        worker: record_identity(&record),
        pgid: record["pgid"].as_u64().unwrap() as i32,
    };
    assert_eq!(descendant.pgid as u32, error.worker.unwrap().pid);
    assert_eq!(error.kind, Kind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(error.replacement_allowed);
    assert_reaped(error.worker.unwrap());
    descendant.assert_gone(Instant::now() + Duration::from_secs(2));
    assert!(run("echo", true).is_ok());
}

#[test]
fn watchdog_parent_identity_does_not_accept_reused_or_unavailable_lifetimes() {
    let parent = process_lifetime(&process_info(unsafe { libc::getppid() }).unwrap()).unwrap();
    assert!(parent_alive(parent));
    assert!(!parent_alive(WorkerIdentity {
        creation_time_100ns: parent.creation_time_100ns + 1,
        ..parent
    }));
    assert!(!parent_alive(WorkerIdentity {
        pid: std::process::id(),
        ..parent
    }));
}

#[test]
fn watchdog_bounds_a_blocked_main_thread_with_a_live_parent() {
    const TEST: &str = "reliability::watchdog_bounds_a_blocked_main_thread_with_a_live_parent";
    if std::env::var("MACOS_WORKER_ISOLATED").as_deref() == Ok(TEST) {
        let watchdog = ParentWatchdog::start().unwrap();
        watchdog.set_budget(Duration::from_millis(500)).unwrap();
        loop {
            thread::park();
        }
    }
    let name = format!(
        "{}::watchdog_bounds_a_blocked_main_thread_with_a_live_parent",
        module_path!().split_once("::").unwrap().1
    );
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &name, "--nocapture"])
        .env("MACOS_WORKER_ISOLATED", TEST)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    let pid = child.id() as i32;
    let mut child = GroupGuard::new(child, pid);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !peek_exited(pid, deadline).unwrap() && Instant::now() < deadline {
        thread::sleep(POLL);
    }
    assert!(
        peek_exited(pid, deadline).unwrap(),
        "watchdog did not independently terminate blocked main"
    );
    let status = child.finish().unwrap();
    assert!(status.signal() == Some(libc::SIGKILL) || status.code() == Some(1));
}

#[test]
fn delayed_watchdog_shutdown_allows_normal_exit_but_keeps_child_deadline() {
    const TEST: &str =
        "reliability::delayed_watchdog_shutdown_allows_normal_exit_but_keeps_child_deadline";
    if std::env::var("MACOS_WORKER_ISOLATED").as_deref() == Ok(TEST) {
        verify_containment().unwrap();
        let blocked = std::env::var("MACOS_WATCHDOG_SHUTDOWN_BLOCKED").unwrap() == "true";
        let parent = process_lifetime(&process_info(unsafe { libc::getppid() }).unwrap()).unwrap();
        let worker = identity(unsafe { libc::getpid() }).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&stop);
        let started = Instant::now();
        let task = thread::spawn(move || {
            while !signal.load(Ordering::Acquire) {
                thread::park_timeout(POLL);
            }
            if blocked {
                loop {
                    thread::park();
                }
            }
            // Deterministically defer completion beyond the former 250 ms
            // shutdown grace, without requiring scheduler contention.
            thread::sleep(Duration::from_millis(500));
        });
        let watchdog = ParentWatchdog {
            stop,
            limit_ms: Arc::new(AtomicU64::new(if blocked { 300 } else { 3000 })),
            task: Some(task),
            started,
            parent,
        };
        let frame = protocol::encode_response(
            &protocol::Outcome::Ok { payload: json!({}) },
            Uuid::new_v4(),
            worker,
        )
        .unwrap();
        duplicate_pipe(libc::STDOUT_FILENO)
            .unwrap()
            .write_all(&frame)
            .unwrap();
        fs::write(
            std::env::var_os("MACOS_WATCHDOG_RESPONSE_RECORD").unwrap(),
            serde_json::to_vec(&json!({
                "pid": worker.pid, "pgid": worker.pid,
                "creation_time_100ns": worker.creation_time_100ns,
                "response_written": true,
            }))
            .unwrap(),
        )
        .unwrap();
        let shutdown_started = Instant::now();
        drop(watchdog);
        assert!(
            !blocked,
            "blocked shutdown must not escape its child budget"
        );
        assert!(shutdown_started.elapsed() >= Duration::from_millis(500));
        return;
    }
    let name = format!(
        "{}::delayed_watchdog_shutdown_allows_normal_exit_but_keeps_child_deadline",
        module_path!().split_once("::").unwrap().1
    );
    for blocked in [false, true] {
        let fixture = Fixture::new();
        let record_path = fixture.path("response.json");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", &name, "--nocapture"])
            .env("MACOS_WORKER_ISOLATED", TEST)
            .env("MACOS_WATCHDOG_SHUTDOWN_BLOCKED", blocked.to_string())
            .env("MACOS_WATCHDOG_RESPONSE_RECORD", &record_path)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().unwrap();
        let pid = child.id() as i32;
        let mut child = GroupGuard::new(child, pid);
        let deadline = Instant::now() + Duration::from_secs(2);
        let record = wait_for_record(&record_path, deadline);
        assert_eq!(record["response_written"], true);
        assert_eq!(record["pid"], pid);
        while !peek_exited(pid, deadline).unwrap() && Instant::now() < deadline {
            thread::sleep(POLL);
        }
        assert!(
            peek_exited(pid, deadline).unwrap(),
            "shutdown did not complete within its existing budget"
        );
        let status = child.finish().unwrap();
        if blocked {
            assert!(status.signal() == Some(libc::SIGKILL) || status.code() == Some(1));
        } else {
            assert!(
                status.success(),
                "delayed watchdog changed a successful exit"
            );
        }
        assert_reaped(record_identity(&record));
    }
}
