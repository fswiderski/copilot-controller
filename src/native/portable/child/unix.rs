use std::{
    io::{self, Read},
    os::unix::{io::AsRawFd, process::CommandExt},
    process::{Child, ChildStdout, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use super::{ChildErrorKind, MAX_STDOUT_BYTES, block_after_unconfirmed_cleanup};

const POLL_INTERVAL: Duration = Duration::from_millis(5);
const CLEANUP_WAIT: Duration = Duration::from_secs(2);
const CHUNK_SIZE: usize = 8_192;
const MAX_CHUNKS_PER_LOOP: usize = 16;

pub(super) fn capture(command: &mut Command, timeout: Duration) -> Result<Vec<u8>, ChildErrorKind> {
    let deadline = Instant::now() + timeout;
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|_| ChildErrorKind::Spawn)?;
    let pid = match libc::pid_t::try_from(child.id()) {
        Ok(pid) if pid > 0 => pid,
        _ => {
            let _ = child.kill();
            block_after_unconfirmed_cleanup();
            return Err(ChildErrorKind::CleanupUnconfirmed);
        }
    };
    let mut guard = GroupGuard {
        child,
        pid,
        group_signaled: false,
        status: None,
    };
    let result = match guard.child.stdout.take() {
        Some(mut stdout) => match make_nonblocking(&stdout, deadline) {
            Ok(()) => capture_loop(&mut guard, &mut stdout, deadline),
            Err(error)
                if error.kind() == io::ErrorKind::Interrupted && Instant::now() >= deadline =>
            {
                Err(ChildErrorKind::TimedOut)
            }
            Err(_) => Err(ChildErrorKind::Transport),
        },
        None => Err(ChildErrorKind::Transport),
    };
    match confirm_cleanup(&mut guard) {
        Ok(_) => result,
        Err(error) => Err(error),
    }
}

fn capture_loop(
    guard: &mut GroupGuard,
    stdout: &mut ChildStdout,
    deadline: Instant,
) -> Result<Vec<u8>, ChildErrorKind> {
    let mut output = Vec::new();
    let mut eof = false;
    let mut status = None;
    loop {
        if Instant::now() >= deadline {
            return Err(ChildErrorKind::TimedOut);
        }
        if !eof {
            eof = drain(stdout, &mut output)?;
        }
        if status.is_none() {
            match peek_exited(guard.pid, deadline) {
                Ok(true) => {
                    status = Some(confirm_cleanup(guard)?);
                }
                Ok(false) => {}
                Err(error)
                    if error.kind() == io::ErrorKind::Interrupted && Instant::now() >= deadline =>
                {
                    return Err(ChildErrorKind::TimedOut);
                }
                Err(_) => return Err(ChildErrorKind::Transport),
            }
        }
        if Instant::now() >= deadline {
            return Err(ChildErrorKind::TimedOut);
        }
        if let Some(exit) = status
            && eof
        {
            return if exit.success() {
                Ok(output)
            } else {
                Err(ChildErrorKind::Failed)
            };
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn confirm_cleanup(guard: &mut GroupGuard) -> Result<ExitStatus, ChildErrorKind> {
    guard.finish().map_err(|()| {
        block_after_unconfirmed_cleanup();
        ChildErrorKind::CleanupUnconfirmed
    })
}

pub(crate) fn make_nonblocking(stdout: &impl AsRawFd, deadline: Instant) -> io::Result<()> {
    let fd = stdout.as_raw_fd();
    let flags = fcntl_retry(fd, libc::F_GETFL, 0, deadline)?;
    fcntl_retry(fd, libc::F_SETFL, flags | libc::O_NONBLOCK, deadline)?;
    Ok(())
}

fn fcntl_retry(
    fd: libc::c_int,
    command: libc::c_int,
    value: libc::c_int,
    deadline: Instant,
) -> io::Result<libc::c_int> {
    loop {
        // The descriptor is the live pipe owned by stdout.
        let result = unsafe { libc::fcntl(fd, command, value) };
        if result >= 0 {
            return Ok(result);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted || Instant::now() >= deadline {
            return Err(error);
        }
    }
}

fn drain(stdout: &mut ChildStdout, output: &mut Vec<u8>) -> Result<bool, ChildErrorKind> {
    let mut buffer = [0_u8; CHUNK_SIZE];
    for _ in 0..MAX_CHUNKS_PER_LOOP {
        match stdout.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                if count > MAX_STDOUT_BYTES - output.len() {
                    return Err(ChildErrorKind::OutputLimit);
                }
                output.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(ChildErrorKind::Transport),
        }
    }
    Ok(false)
}

/// WNOWAIT keeps the group leader's PID reserved until after group signaling.
pub(crate) fn peek_exited(pid: libc::pid_t, deadline: Instant) -> io::Result<bool> {
    loop {
        // A zeroed siginfo also makes the WNOHANG/no-event case unambiguous.
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            #[cfg(target_os = "macos")]
            let observed_pid = info.si_pid;
            #[cfg(target_os = "linux")]
            let observed_pid = unsafe { info.si_pid() };
            return match observed_pid {
                0 => Ok(false),
                observed if observed == pid => Ok(true),
                _ => Err(io::Error::other("unexpected child status")),
            };
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted || Instant::now() >= deadline {
            return Err(error);
        }
    }
}

pub(crate) struct GroupGuard {
    pub(crate) child: Child,
    pid: libc::pid_t,
    group_signaled: bool,
    status: Option<ExitStatus>,
}

impl GroupGuard {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub(crate) fn new(child: Child, pid: libc::pid_t) -> Self {
        Self {
            child,
            pid,
            group_signaled: false,
            status: None,
        }
    }

    pub(crate) fn finish(&mut self) -> Result<ExitStatus, ()> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let until = Instant::now() + CLEANUP_WAIT;
        if !self.group_signaled {
            signal_group(self.pid, until)?;
            self.group_signaled = true;
        }
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.status = Some(status);
                    return Ok(status);
                }
                Ok(None) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(()),
            }
            if Instant::now() >= until {
                return Err(());
            }
            thread::sleep(POLL_INTERVAL.min(until.saturating_duration_since(Instant::now())));
        }
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if self.status.is_none() && self.finish().is_err() {
            block_after_unconfirmed_cleanup();
        }
    }
}

fn signal_group(pid: libc::pid_t, deadline: Instant) -> Result<(), ()> {
    // Refuse to signal if another reaper has already released the leader PID.
    peek_exited(pid, deadline).map_err(|_| ())?;
    loop {
        // A negative PID targets only the newly created process group.
        if unsafe { libc::kill(-pid, libc::SIGKILL) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => return Ok(()),
            Some(libc::EINTR) if Instant::now() < deadline => continue,
            #[cfg(target_os = "macos")]
            Some(libc::EPERM) if sole_zombie_group_leader(pid, deadline) => return Ok(()),
            _ => return Err(()),
        }
    }
}

#[cfg(target_os = "macos")]
fn sole_zombie_group_leader(pid: libc::pid_t, deadline: Instant) -> bool {
    if !matches!(peek_exited(pid, deadline), Ok(true)) {
        return false;
    }
    // Two entries distinguish a sole leader from any group with more members.
    let mut members = [0 as libc::pid_t; 2];
    let member_bytes = std::mem::size_of_val(&members) as libc::c_int;
    let count = unsafe { libc::proc_listpgrppids(pid, members.as_mut_ptr().cast(), member_bytes) };
    if count != 1 || members[0] != pid {
        return false;
    }
    // WNOWAIT proves this sole listed member is exited but still unreaped.
    matches!(peek_exited(pid, deadline), Ok(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn externally_reaped_leader_causes_sticky_cleanup_block() {
        const CHILD_TEST: &str = "PORTABLE_TEST_LOST_LEADER";
        if std::env::var_os(CHILD_TEST).is_some() {
            let mut command =
                Command::new(std::env::var_os("PORTABLE_TEST_HELPER").expect("helper path"));
            command
                .arg("stall")
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let mut child = command.spawn().unwrap();
            let pid = libc::pid_t::try_from(child.id()).unwrap();
            child.kill().unwrap();
            child.wait().unwrap();
            let mut guard = GroupGuard {
                child,
                pid,
                group_signaled: false,
                status: None,
            };
            assert!(guard.finish().is_err());
            drop(guard);
            assert!(super::super::cleanup_blocked());
            let mut later =
                Command::new(std::env::var_os("PORTABLE_TEST_HELPER").expect("helper path"));
            later.arg("success");
            assert_eq!(
                super::super::capture(&mut later, Duration::from_secs(1))
                    .unwrap_err()
                    .kind,
                ChildErrorKind::CleanupUnconfirmed
            );
            return;
        }

        let module = module_path!().split_once("::").unwrap().1;
        let test_name = format!("{module}::externally_reaped_leader_causes_sticky_cleanup_block");
        let mut subprocess = Command::new(std::env::current_exe().unwrap());
        subprocess
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .env(CHILD_TEST, "1");
        let output = super::super::capture(&mut subprocess, Duration::from_secs(5)).unwrap();
        assert!(
            String::from_utf8_lossy(&output).contains("1 passed; 0 failed"),
            "isolated cleanup test did not run"
        );
    }
}
