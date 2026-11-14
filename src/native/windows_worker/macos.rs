use std::{
    cell::Cell,
    ffi::OsStr,
    fs::File,
    io::{self, Cursor, Read, Write},
    marker::PhantomData,
    os::unix::{
        ffi::OsStrExt,
        io::{FromRawFd, IntoRawFd},
        process::CommandExt,
    },
    path::Path,
    process::{ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(any(feature = "micro-hid", feature = "akp03-hid", test))]
use std::rc::Rc;

use serde_json::{Value, json};
use uuid::Uuid;

use crate::native::portable::child::{GroupGuard, make_nonblocking, peek_exited};

use super::{
    DiagnosticAllowlist, DiagnosticCallbackError, DiagnosticRequest, DiagnosticResult,
    DiagnosticWorkerRoute, MAX_DIAGNOSTIC_BYTES, MAX_FRAME_BODY_BYTES, WorkerError,
    WorkerErrorKind as Kind, WorkerIdentity,
    protocol::{self, RequestEnvelope, ResponseEnvelope},
    supervision::SupervisorGate,
};

const POLL: Duration = Duration::from_millis(5);
const WATCHDOG_POLL: Duration = Duration::from_millis(50);
const WATCHDOG_START: Duration = Duration::from_secs(1);
static SUPERVISOR: SupervisorGate = SupervisorGate::new();

thread_local! {
    // Set only while the fully validated hidden request's callback is running.
    static WORKER_SCOPE: Cell<Option<(ScopeAuthority, Uuid)>> = const { Cell::new(None) };
    static WATCHDOG_PARENT: Cell<Option<WorkerIdentity>> = const { Cell::new(None) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScopeAuthority {
    Health,
    HidHealth,
    HidLive,
    CtSerialProbe,
}

struct WorkerScope;

impl WorkerScope {
    fn enter(authority: ScopeAuthority) -> Result<Self, Kind> {
        verify_containment()?;
        let parent = WATCHDOG_PARENT.with(Cell::get).ok_or(Kind::Containment)?;
        if !parent_alive(parent) {
            return Err(Kind::Containment);
        }
        WORKER_SCOPE.with(|scope| {
            if scope.get().is_some() {
                return Err(Kind::Containment);
            }
            scope.set(Some((authority, Uuid::new_v4())));
            Ok(Self)
        })
    }
}

impl Drop for WorkerScope {
    fn drop(&mut self) {
        WORKER_SCOPE.with(|scope| scope.set(None));
    }
}

/// No public constructor; neither health nor ordinary in-process callers get it.
#[derive(Clone)]
#[cfg(any(feature = "micro-hid", feature = "akp03-hid", test))]
pub(crate) struct HidWorkerCapability {
    pid: libc::pid_t,
    scope_nonce: Uuid,
    parent: WorkerIdentity,
    _thread_bound: PhantomData<Rc<()>>,
}

#[cfg(any(feature = "micro-hid", feature = "akp03-hid", test))]
impl HidWorkerCapability {
    pub(crate) fn valid(&self) -> bool {
        WORKER_SCOPE.with(|scope| scope.get() == Some((ScopeAuthority::HidLive, self.scope_nonce)))
            && unsafe { libc::getpid() } == self.pid
            && verify_containment().is_ok()
            && WATCHDOG_PARENT.with(Cell::get) == Some(self.parent)
            && parent_alive(self.parent)
            && verify_supervisor_parent().is_ok()
    }
}

#[cfg(any(feature = "micro-hid", feature = "akp03-hid", test))]
pub(crate) fn hid_worker_capability() -> Option<HidWorkerCapability> {
    let (authority, scope_nonce) = WORKER_SCOPE.with(Cell::get)?;
    let parent = WATCHDOG_PARENT.with(Cell::get)?;
    if authority != ScopeAuthority::HidLive
        || verify_containment().is_err()
        || verify_supervisor_parent().is_err()
    {
        return None;
    }
    if !parent_alive(parent) {
        return None;
    }
    Some(HidWorkerCapability {
        pid: unsafe { libc::getpid() },
        scope_nonce,
        parent,
        _thread_bound: PhantomData,
    })
}

#[derive(Clone)]
pub(crate) struct CtWorkerCapability {
    pid: libc::pid_t,
    scope_nonce: Uuid,
    parent: WorkerIdentity,
    _thread_bound: PhantomData<*const ()>,
}

impl CtWorkerCapability {
    pub(crate) fn valid(&self) -> bool {
        WORKER_SCOPE
            .with(|scope| scope.get() == Some((ScopeAuthority::CtSerialProbe, self.scope_nonce)))
            && unsafe { libc::getpid() } == self.pid
            && verify_containment().is_ok()
            && WATCHDOG_PARENT.with(Cell::get) == Some(self.parent)
            && parent_alive(self.parent)
            && verify_supervisor_parent().is_ok()
    }
}

pub(crate) fn ct_worker_capability() -> Option<CtWorkerCapability> {
    let (authority, scope_nonce) = WORKER_SCOPE.with(Cell::get)?;
    let parent = WATCHDOG_PARENT.with(Cell::get)?;
    if authority != ScopeAuthority::CtSerialProbe
        || verify_containment().is_err()
        || verify_supervisor_parent().is_err()
    {
        return None;
    }
    if !parent_alive(parent) {
        return None;
    }
    Some(CtWorkerCapability {
        pid: unsafe { libc::getpid() },
        scope_nonce,
        parent,
        _thread_bound: PhantomData,
    })
}

pub(crate) fn ct_probe_containment() -> Result<(), DiagnosticCallbackError> {
    let capability = ct_worker_capability().ok_or(DiagnosticCallbackError)?;
    if capability.valid() {
        Ok(())
    } else {
        Err(DiagnosticCallbackError)
    }
}

pub(crate) fn worker_health() -> Result<Value, DiagnosticCallbackError> {
    if !WORKER_SCOPE.with(|scope| scope.get().is_some())
        || WATCHDOG_PARENT
            .with(Cell::get)
            .is_none_or(|parent| !parent_alive(parent))
        || verify_containment().is_err()
    {
        return Err(DiagnosticCallbackError);
    }
    Ok(json!({
        "process_group": true, "pid": std::process::id(),
        "pgid": unsafe { libc::getpgrp() },
        "parent_death_guarantee": false, "escape_guarantee": false,
        "parent_death_watchdog": true, "parent_death_poll_ms": WATCHDOG_POLL.as_millis(),
        "read_only": true, "command_ready": false, "w2_qualified": false,
    }))
}

pub(super) fn validate_platform_request(request: &DiagnosticRequest) -> Result<Duration, Kind> {
    match request.kind.as_str() {
        "health" if request.payload.is_null() => Ok(Duration::from_secs(3)),
        "health" => Err(Kind::InvalidRequest),
        "micro_hid_probe" | "akp03_hid_probe" => crate::cli::validate_worker_options(
            request.payload.clone(),
            request.kind == "micro_hid_probe",
        )
        .map_err(|_| Kind::InvalidRequest),
        "ct_serial_probe" => {
            let options: crate::devices::ct::probe::Options =
                serde_json::from_value(request.payload.clone())
                    .map_err(|_| Kind::InvalidRequest)?;
            options.validate().map_err(|_| Kind::InvalidRequest)?;
            Ok(Duration::from_millis(
                u64::from(options.total_timeout_ms)
                    + 2_000
                    + u64::from(options.io_timeout_ms.min(250)),
            ))
        }
        _ => Err(Kind::UnsupportedPlatform),
    }
}

fn verify_containment() -> Result<(), Kind> {
    let pid = unsafe { libc::getpid() };
    if pid <= 0 || unsafe { libc::getpgrp() } != pid {
        return Err(Kind::Containment);
    }

    for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0
            || unsafe { stat.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFIFO
            || unsafe { libc::isatty(fd) } != 0
        {
            return Err(Kind::Containment);
        }
    }
    Ok(())
}

fn verify_supervisor_parent() -> Result<(), Kind> {
    let parent = unsafe { libc::getppid() };
    if parent <= 1 {
        return Err(Kind::Containment);
    }
    let mut path = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let count = unsafe { libc::proc_pidpath(parent, path.as_mut_ptr().cast(), path.len() as u32) };
    if count <= 0 {
        return Err(Kind::Containment);
    }
    let length = path
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(Kind::Containment)?;
    let executable = std::env::current_exe().map_err(|_| Kind::Containment)?;
    same_executable_paths(Path::new(OsStr::from_bytes(&path[..length])), &executable)
}

fn same_executable_paths(parent: &Path, current: &Path) -> Result<(), Kind> {
    let parent = parent.canonicalize().map_err(|_| Kind::Containment)?;
    let current = current.canonicalize().map_err(|_| Kind::Containment)?;
    if parent != current {
        return Err(Kind::Containment);
    }
    Ok(())
}

fn process_info(pid: libc::pid_t) -> Result<libc::proc_bsdinfo, Kind> {
    if pid <= 0 {
        return Err(Kind::Containment);
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of_val(&info) as libc::c_int;
    if unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    } != size
    {
        return Err(Kind::Containment);
    }
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid as u32 {
        return Err(Kind::Containment);
    }
    Ok(info)
}

fn process_lifetime(info: &libc::proc_bsdinfo) -> Result<WorkerIdentity, Kind> {
    let started = info
        .pbi_start_tvsec
        .checked_mul(10_000_000)
        .and_then(|seconds| {
            info.pbi_start_tvusec
                .checked_mul(10)
                .and_then(|fraction| seconds.checked_add(fraction))
        })
        .filter(|value| *value != 0)
        .ok_or(Kind::Containment)?;
    Ok(WorkerIdentity {
        pid: info.pbi_pid,
        creation_time_100ns: started,
    })
}

fn identity(pid: libc::pid_t) -> Result<WorkerIdentity, Kind> {
    let info = process_info(pid)?;
    if info.pbi_pgid != pid as u32 {
        return Err(Kind::Containment);
    }
    process_lifetime(&info)
}

fn parent_alive(parent: WorkerIdentity) -> bool {
    if unsafe { libc::getppid() } as u32 != parent.pid {
        return false;
    }
    process_info(parent.pid as libc::pid_t)
        .is_ok_and(|info| info.pbi_status != libc::SZOMB && process_lifetime(&info) == Ok(parent))
}

fn terminate_worker_group(pid: libc::pid_t) -> ! {
    // This live process reserves its original leader PID. No destructors, HID
    // locks or the possibly blocked main thread are needed to terminate.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
        libc::_exit(1);
    }
}

struct ParentWatchdog {
    stop: Arc<AtomicBool>,
    limit_ms: Arc<AtomicU64>,
    task: Option<thread::JoinHandle<()>>,
    started: Instant,
    parent: WorkerIdentity,
}

impl ParentWatchdog {
    fn start() -> Result<Self, Kind> {
        verify_containment()?;
        if WATCHDOG_PARENT.with(Cell::get).is_some() {
            return Err(Kind::Containment);
        }
        let pid = unsafe { libc::getpid() };
        let parent_pid = unsafe { libc::getppid() };
        if parent_pid <= 1 {
            return Err(Kind::Containment);
        }
        let parent = process_lifetime(&process_info(parent_pid)?)?;
        if !parent_alive(parent) {
            return Err(Kind::Containment);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let limit_ms = Arc::new(AtomicU64::new(super::MAX_TIMEOUT.as_millis() as u64));
        let (ready, receiver) = mpsc::sync_channel(1);
        let signal = Arc::clone(&stop);
        let limit = Arc::clone(&limit_ms);
        let started = Instant::now();
        let task = thread::Builder::new()
            .name("hid-parent-watchdog".into())
            .spawn(move || {
                if !parent_alive(parent) {
                    terminate_worker_group(pid);
                }
                let _ = ready.try_send(());
                while !signal.load(Ordering::Acquire) {
                    if !parent_alive(parent)
                        || started.elapsed().as_millis()
                            >= u128::from(limit.load(Ordering::Acquire))
                    {
                        terminate_worker_group(pid);
                    }
                    thread::park_timeout(WATCHDOG_POLL);
                }
            })
            .map_err(|_| Kind::Containment)?;
        let watchdog = Self {
            stop,
            limit_ms,
            task: Some(task),
            started,
            parent,
        };
        if receiver.recv_timeout(WATCHDOG_START).is_err() {
            terminate_worker_group(pid);
        }
        WATCHDOG_PARENT.with(|slot| slot.set(Some(parent)));
        Ok(watchdog)
    }

    fn set_budget(&self, budget: Duration) -> Result<(), Kind> {
        self.limit_ms
            .store(budget.as_millis() as u64, Ordering::Release);
        if self.started.elapsed() >= budget {
            return Err(Kind::TimedOut);
        }
        if let Some(task) = &self.task {
            task.thread().unpark();
        }
        Ok(())
    }
}

impl Drop for ParentWatchdog {
    fn drop(&mut self) {
        WATCHDOG_PARENT.with(|slot| slot.set(None));
        self.stop.store(true, Ordering::Release);
        let Some(task) = self.task.take() else {
            return;
        };
        task.thread().unpark();
        // Shutdown consumes the existing child budget, not a separate short
        // grace that could kill a successfully completed probe under load.
        // The supervisor still enforces any shorter caller deadline.
        while !task.is_finished() {
            if !parent_alive(self.parent)
                || self.started.elapsed().as_millis()
                    >= u128::from(self.limit_ms.load(Ordering::Acquire))
            {
                terminate_worker_group(unsafe { libc::getpid() });
            }
            thread::sleep(POLL);
        }
        if task.join().is_err() {
            terminate_worker_group(unsafe { libc::getpid() });
        }
    }
}

fn duplicate_pipe(fd: libc::c_int) -> Result<File, Kind> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(Kind::Containment);
    }
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err(Kind::Containment);
    }
    Ok(unsafe { File::from_raw_fd(duplicate) })
}

pub(super) fn worker_main(allowlist: &DiagnosticAllowlist) -> Result<(), WorkerError> {
    let run = || -> Result<(), Kind> {
        verify_containment()?;
        let watchdog = ParentWatchdog::start()?;
        let mut stdin = duplicate_pipe(libc::STDIN_FILENO)?;
        let mut stdout = duplicate_pipe(libc::STDOUT_FILENO)?;
        let _stderr = duplicate_pipe(libc::STDERR_FILENO)?;
        let envelope: RequestEnvelope = protocol::read_frame(&mut stdin)?;
        protocol::validate_request(&envelope, allowlist)?;
        watchdog.set_budget(validate_platform_request(&envelope.request)?)?;
        let worker = identity(unsafe { libc::getpid() })?;
        let callback = allowlist
            .find(&envelope.request.kind)
            .ok_or(Kind::InvalidRequest)?;
        let authority = match envelope.request.kind.as_str() {
            "health" => ScopeAuthority::Health,
            "ct_serial_probe" => ScopeAuthority::CtSerialProbe,
            "micro_hid_probe" | "akp03_hid_probe" => {
                if envelope.request.payload["health_check"] == false {
                    ScopeAuthority::HidLive
                } else {
                    ScopeAuthority::HidHealth
                }
            }
            _ => return Err(Kind::InvalidRequest),
        };
        let _scope = WorkerScope::enter(authority)?;
        let outcome = protocol::invoke_callback(callback, envelope.request.payload);
        let frame = protocol::encode_response(&outcome, envelope.request_nonce, worker)?;
        stdout.write_all(&frame).map_err(|_| Kind::Transport)
    };
    run().map_err(WorkerError::before_launch)
}

#[derive(Default)]
struct Pipes {
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
}

fn close_pipe<T: IntoRawFd>(pipe: &mut Option<T>) -> bool {
    pipe.take().is_none_or(|pipe| {
        // No I/O tasks exist. An ambiguous close fences replacement, without
        // retrying a descriptor number the OS may already have released.
        (unsafe { libc::close(pipe.into_raw_fd()) }) == 0
    })
}

impl Pipes {
    fn close(&mut self) -> bool {
        let stdin = close_pipe(&mut self.stdin);
        let stdout = close_pipe(&mut self.stdout);
        let stderr = close_pipe(&mut self.stderr);
        stdin && stdout && stderr
    }
}

impl Drop for Pipes {
    fn drop(&mut self) {
        if !self.close() {
            SUPERVISOR.block();
        }
    }
}

fn checkpoint(cancelled: &AtomicBool, deadline: Instant) -> Result<(), Kind> {
    if cancelled.load(Ordering::Acquire) {
        Err(Kind::Cancelled)
    } else if Instant::now() >= deadline {
        Err(Kind::TimedOut)
    } else {
        Ok(())
    }
}

pub(super) fn supervise(
    frame: Vec<u8>,
    nonce: Uuid,
    timeout: Duration,
    cancelled: &AtomicBool,
    route: DiagnosticWorkerRoute,
) -> Result<DiagnosticResult, WorkerError> {
    let executable =
        std::env::current_exe().map_err(|_| WorkerError::before_launch(Kind::Spawn))?;
    let mut command = Command::new(executable);
    command.arg(route.argument());
    supervise_command(&mut command, frame, nonce, timeout, cancelled)
}

fn supervise_command(
    command: &mut Command,
    frame: Vec<u8>,
    nonce: Uuid,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<DiagnosticResult, WorkerError> {
    let _active = SUPERVISOR.acquire().map_err(WorkerError::before_launch)?;
    if crate::native::portable::child::cleanup_blocked() {
        SUPERVISOR.block();
        return Err(WorkerError::before_launch(Kind::SupervisionBlocked));
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| WorkerError::before_launch(Kind::InvalidTimeout))?;
    checkpoint(cancelled, deadline).map_err(WorkerError::before_launch)?;
    command
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .map_err(|_| WorkerError::before_launch(Kind::Spawn))?;
    let pid = child.id() as libc::pid_t;
    let mut child = GroupGuard::new(child, pid);
    let mut pipes = Pipes {
        stdin: child.child.stdin.take(),
        stdout: child.child.stdout.take(),
        stderr: child.child.stderr.take(),
    };
    let mut worker = None;
    let run = (|| {
        if pid <= 0 || unsafe { libc::getpgid(pid) } != pid {
            return Err(Kind::Containment);
        }
        let retained = identity(pid)?;
        worker = Some(retained);
        checkpoint(cancelled, deadline)?;
        make_nonblocking(pipes.stdin.as_ref().ok_or(Kind::Transport)?, deadline)
            .map_err(|_| Kind::Transport)?;
        make_nonblocking(pipes.stdout.as_ref().ok_or(Kind::Transport)?, deadline)
            .map_err(|_| Kind::Transport)?;
        make_nonblocking(pipes.stderr.as_ref().ok_or(Kind::Transport)?, deadline)
            .map_err(|_| Kind::Transport)?;
        pump(&mut pipes, pid, &frame, deadline, cancelled).and_then(|output| {
            let response: ResponseEnvelope = protocol::read_frame(&mut Cursor::new(output))?;
            let outcome = protocol::validate_response(response, nonce, retained)?;
            checkpoint(cancelled, deadline)?;
            match outcome {
                protocol::Outcome::Ok { payload } => Ok(DiagnosticResult {
                    worker: retained,
                    payload,
                }),
                protocol::Outcome::CallbackFailed => Err(Kind::Callback),
                protocol::Outcome::ResponseEncodingFailed => Err(Kind::FrameLimit),
            }
        })
    })();
    // Group signaling precedes reaping even on success, while WNOWAIT keeps the
    // leader PID reserved. Never leave a replacement claim to Drop.
    let exited = child.finish();
    let io_closed = pipes.close() && !matches!(run, Err(Kind::CleanupUnconfirmed));
    if exited.is_err() || !io_closed {
        SUPERVISOR.block();
        return Err(WorkerError::after_launch(
            Kind::CleanupUnconfirmed,
            worker,
            exited.is_ok(),
            io_closed,
        ));
    }
    match run {
        Ok(result) if exited.is_ok_and(|status| status.success()) => {
            checkpoint(cancelled, deadline)
                .map_err(|kind| WorkerError::after_launch(kind, worker, true, true))?;
            Ok(result)
        }
        Ok(_) => Err(WorkerError::after_launch(
            Kind::WorkerFailed,
            worker,
            true,
            true,
        )),
        Err(kind) => Err(WorkerError::after_launch(kind, worker, true, true)),
    }
}

fn pump(
    pipes: &mut Pipes,
    pid: libc::pid_t,
    frame: &[u8],
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, Kind> {
    let mut sent = 0;
    let mut output = Vec::new();
    let mut diagnostics = 0;
    loop {
        checkpoint(cancelled, deadline)?;
        if let Some(stdin) = &mut pipes.stdin {
            match stdin.write(&frame[sent..]) {
                Ok(0) => return Err(Kind::Transport),
                Ok(count) => sent += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(Kind::Transport),
            }
            if sent == frame.len() && !close_pipe(&mut pipes.stdin) {
                SUPERVISOR.block();
                return Err(Kind::CleanupUnconfirmed);
            }
        }
        drain(&mut pipes.stdout, |bytes| {
            if bytes.len() > MAX_FRAME_BODY_BYTES + 4 - output.len() {
                return Err(Kind::FrameLimit);
            }
            output.extend_from_slice(bytes);
            if output.len() >= 4 {
                let length = u32::from_le_bytes(output[..4].try_into().unwrap()) as usize;
                if length == 0 || length > MAX_FRAME_BODY_BYTES {
                    return Err(Kind::FrameLimit);
                }
                if output.len() > length + 4 {
                    return Err(Kind::Protocol);
                }
            }
            Ok(())
        })?;
        drain(&mut pipes.stderr, |bytes| {
            if bytes.len() > MAX_DIAGNOSTIC_BYTES - diagnostics {
                return Err(Kind::DiagnosticLimit);
            }
            diagnostics += bytes.len();
            Ok(())
        })?;
        checkpoint(cancelled, deadline)?;
        let exited = peek_exited(pid, deadline).map_err(|_| Kind::Containment)?;
        if exited && pipes.stdin.is_none() && pipes.stdout.is_none() && pipes.stderr.is_none() {
            return Ok(output);
        }
        thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn drain<T: Read + IntoRawFd>(
    pipe: &mut Option<T>,
    mut accept: impl FnMut(&[u8]) -> Result<(), Kind>,
) -> Result<(), Kind> {
    let Some(reader) = pipe.as_mut() else {
        return Ok(());
    };
    let mut buffer = [0; 4096];
    for _ in 0..16 {
        match reader.read(&mut buffer) {
            Ok(0) => {
                if !close_pipe(pipe) {
                    SUPERVISOR.block();
                    return Err(Kind::CleanupUnconfirmed);
                }
                break;
            }
            Ok(count) => accept(&buffer[..count])?,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(Kind::Transport),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
