use std::{
    process::Command,
    time::{Duration, Instant},
};

use super::super::child::{self, ChildErrorKind, MAX_STDOUT_BYTES};

fn helper(mode: &str) -> Command {
    let mut command = Command::new(std::env::var_os("PORTABLE_TEST_HELPER").expect("helper path"));
    command.arg(mode);
    command
}

#[test]
fn success_failure_output_boundaries_and_timeout() {
    assert!(!child::cleanup_blocked());
    assert_eq!(
        child::capture(&mut helper("success"), Duration::from_secs(5)).unwrap(),
        b"marker"
    );
    assert_eq!(
        child::capture(&mut helper("failure"), Duration::from_secs(5))
            .unwrap_err()
            .kind,
        ChildErrorKind::Failed
    );
    let mut exact = helper("output");
    exact.arg(MAX_STDOUT_BYTES.to_string());
    let output = child::capture(&mut exact, Duration::from_secs(5)).unwrap();
    assert_eq!(output.len(), MAX_STDOUT_BYTES);
    assert!(output.iter().all(|byte| *byte == b'x'));
    let mut excessive = helper("output");
    excessive.arg((MAX_STDOUT_BYTES + 1).to_string());
    assert_eq!(
        child::capture(&mut excessive, Duration::from_secs(5))
            .unwrap_err()
            .kind,
        ChildErrorKind::OutputLimit
    );
    let started = Instant::now();
    let error = child::capture(&mut helper("stall"), Duration::from_millis(100)).unwrap_err();
    assert_eq!(error.kind, ChildErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(4));
    assert_eq!(error.to_string(), "diagnostic child: TimedOut");
    assert!(!child::cleanup_blocked());
    assert_eq!(
        child::capture(&mut helper("success"), Duration::from_secs(5)).unwrap(),
        b"marker"
    );
}

#[test]
fn timeout_validation_accepts_positive_submillisecond_budgets() {
    for timeout in [
        Duration::ZERO,
        Duration::from_secs(120) + Duration::from_nanos(1),
    ] {
        assert_eq!(
            child::capture(&mut helper("success"), timeout)
                .unwrap_err()
                .kind,
            ChildErrorKind::InvalidTimeout
        );
    }
    let result = child::capture(&mut helper("stall"), Duration::from_nanos(1));
    assert_eq!(result.unwrap_err().kind, ChildErrorKind::TimedOut);
    assert_eq!(
        child::capture(&mut helper("success"), Duration::from_secs(120)).unwrap(),
        b"marker"
    );
    assert!(!child::cleanup_blocked());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod group {
    use std::{fs, path::PathBuf, thread};

    use uuid::Uuid;

    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root =
                PathBuf::from(std::env::var_os("PORTABLE_TEST_FIXTURES").expect("fixture root"));
            let path = root.join(Uuid::new_v4().to_string());
            fs::create_dir(&path).expect("unique fixture directory");
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove fixture");
        }
    }

    fn is_running(pid: libc::pid_t) -> bool {
        #[cfg(target_os = "macos")]
        {
            let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
            let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
            let written = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    info.as_mut_ptr().cast(),
                    size,
                )
            };
            if written == size {
                return unsafe { info.assume_init() }.pbi_status != libc::SZOMB;
            }
        }
        #[cfg(target_os = "linux")]
        {
            if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat"))
                && let Some((_, rest)) = stat.rsplit_once(") ")
                && rest.starts_with("Z ")
            {
                return false;
            }
        }
        // Only the known synthetic descendant PID is inspected.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    #[test]
    fn exited_leader_does_not_leave_pipe_holding_descendant() {
        let fixture = Fixture::new();
        let pid_file = fixture.0.join("descendant-pid");
        let mut command = helper("descendant-leader");
        command.arg(&pid_file);
        let started = Instant::now();
        let output = child::capture(&mut command, Duration::from_secs(3)).unwrap();
        assert_eq!(output, b"done");
        assert!(started.elapsed() < Duration::from_secs(4));
        let pid: libc::pid_t = fs::read_to_string(pid_file).unwrap().parse().unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while is_running(pid) && Instant::now() < until {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!is_running(pid), "synthetic descendant is still running");
        assert!(!child::cleanup_blocked());
    }
}
