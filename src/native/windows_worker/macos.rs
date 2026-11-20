use std::{
    cell::Cell,
    ffi::OsStr,
    fs::File,
    io::{self, Cursor, Read, Write},
    marker::PhantomData,
    os::unix::{
        ffi::OsStrExt,
        io::{AsRawFd, FromRawFd, IntoRawFd},
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

#[cfg(any(
    feature = "micro-hid",
    feature = "akp03-hid",
    feature = "mx-creative-hid",
    test
))]
use std::rc::Rc;

use serde::{Deserialize, Serialize};
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
    MxCreativeDisplayInfo,
    MxCreativeDisplayRoute,
    MxCreativeFirmwareInfo,
    MxCreativeSingleKeyPaint,
    MxCreativeReview,
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
#[cfg(any(
    feature = "micro-hid",
    feature = "akp03-hid",
    feature = "mx-creative-hid",
    test
))]
pub(crate) struct HidWorkerCapability {
    pid: libc::pid_t,
    scope_nonce: Uuid,
    parent: WorkerIdentity,
    authority: ScopeAuthority,
    _thread_bound: PhantomData<Rc<()>>,
}

#[cfg(any(
    feature = "micro-hid",
    feature = "akp03-hid",
    feature = "mx-creative-hid",
    test
))]
impl HidWorkerCapability {
    pub(crate) fn valid(&self) -> bool {
        WORKER_SCOPE.with(|scope| scope.get() == Some((self.authority, self.scope_nonce)))
            && unsafe { libc::getpid() } == self.pid
            && verify_containment().is_ok()
            && WATCHDOG_PARENT.with(Cell::get) == Some(self.parent)
            && parent_alive(self.parent)
            && verify_supervisor_parent().is_ok()
    }
}

#[cfg(any(
    feature = "micro-hid",
    feature = "akp03-hid",
    feature = "mx-creative-hid",
    test
))]
pub(crate) fn hid_worker_capability() -> Option<HidWorkerCapability> {
    hid_capability_for(ScopeAuthority::HidLive)
}

#[cfg(any(feature = "mx-creative-hid", test))]
pub(crate) fn review_hid_worker_capability() -> Option<HidWorkerCapability> {
    hid_capability_for(ScopeAuthority::MxCreativeReview)
}

#[cfg(any(feature = "mx-creative-hid", test))]
pub(crate) fn display_info_hid_worker_capability() -> Option<HidWorkerCapability> {
    hid_capability_for(ScopeAuthority::MxCreativeDisplayInfo)
}

#[cfg(any(feature = "mx-creative-hid", test))]
pub(crate) fn display_route_hid_worker_capability() -> Option<HidWorkerCapability> {
    hid_capability_for(ScopeAuthority::MxCreativeDisplayRoute)
}

#[cfg(any(feature = "mx-creative-hid", test))]
pub(crate) fn firmware_info_hid_worker_capability() -> Option<HidWorkerCapability> {
    hid_capability_for(ScopeAuthority::MxCreativeFirmwareInfo)
}

#[cfg(any(feature = "mx-creative-hid", test))]
pub(crate) fn single_key_paint_hid_worker_capability() -> Option<HidWorkerCapability> {
    hid_capability_for(ScopeAuthority::MxCreativeSingleKeyPaint)
}

#[cfg(any(
    feature = "micro-hid",
    feature = "akp03-hid",
    feature = "mx-creative-hid",
    test
))]
fn hid_capability_for(requested_authority: ScopeAuthority) -> Option<HidWorkerCapability> {
    let (authority, scope_nonce) = WORKER_SCOPE.with(Cell::get)?;
    let parent = WATCHDOG_PARENT.with(Cell::get)?;
    if authority != requested_authority
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
        authority,
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
        "mx_creative_evidence" => {
            let options: crate::devices::mx_creative::probe::Options =
                serde_json::from_value(request.payload.clone())
                    .map_err(|_| Kind::InvalidRequest)?;
            options.validate().map_err(|_| Kind::InvalidRequest)?;
            Ok(options.budget())
        }
        "mx_creative_display_info" => {
            let options: crate::devices::mx_creative::display_info::Options =
                serde_json::from_value(request.payload.clone())
                    .map_err(|_| Kind::InvalidRequest)?;
            options.validate().map_err(|_| Kind::InvalidRequest)?;
            Ok(options.budget())
        }
        "mx_creative_display_route" if request.payload.is_null() => Ok(Duration::from_secs(8)),
        "mx_creative_firmware_info" => {
            let options: crate::devices::mx_creative::firmware_info::Options =
                serde_json::from_value(request.payload.clone())
                    .map_err(|_| Kind::InvalidRequest)?;
            options.validate().map_err(|_| Kind::InvalidRequest)?;
            Ok(options.budget())
        }
        "mx_creative_single_key_paint" if request.payload.is_null() => Ok(Duration::from_secs(14)),
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

fn request_authority(request: &DiagnosticRequest) -> Result<ScopeAuthority, Kind> {
    match request.kind.as_str() {
        "health" => Ok(ScopeAuthority::Health),
        "ct_serial_probe" => Ok(ScopeAuthority::CtSerialProbe),
        "mx_creative_evidence" => Ok(ScopeAuthority::HidLive),
        "mx_creative_display_info" => Ok(ScopeAuthority::MxCreativeDisplayInfo),
        "mx_creative_display_route" => Ok(ScopeAuthority::MxCreativeDisplayRoute),
        "mx_creative_firmware_info" => Ok(ScopeAuthority::MxCreativeFirmwareInfo),
        "mx_creative_single_key_paint" => Ok(ScopeAuthority::MxCreativeSingleKeyPaint),
        "micro_hid_probe" | "akp03_hid_probe" => {
            if request.payload["health_check"] == false {
                Ok(ScopeAuthority::HidLive)
            } else {
                Ok(ScopeAuthority::HidHealth)
            }
        }
        _ => Err(Kind::InvalidRequest),
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
        Self::start_with_limit(Some(super::MAX_TIMEOUT))
    }

    fn start_live() -> Result<Self, Kind> {
        Self::start_with_limit(None)
    }

    fn start_with_limit(limit: Option<Duration>) -> Result<Self, Kind> {
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
        let limit_ms = Arc::new(AtomicU64::new(
            limit
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0),
        ));
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
                    let budget = limit.load(Ordering::Acquire);
                    if !parent_alive(parent)
                        || (budget != 0 && started.elapsed().as_millis() >= u128::from(budget))
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
        let milliseconds = budget.as_millis() as u64;
        if milliseconds == 0 {
            return Err(Kind::InvalidTimeout);
        }
        self.limit_ms.store(milliseconds, Ordering::Release);
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
            let budget = self.limit_ms.load(Ordering::Acquire);
            if !parent_alive(self.parent)
                || (budget != 0 && self.started.elapsed().as_millis() >= u128::from(budget))
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
        let authority = request_authority(&envelope.request)?;
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

const LIVE_VERSION: u32 = 1;
const MAX_LIVE_FRAME: usize = 256;
const LIVE_SETUP: Duration = Duration::from_secs(2);
const LIVE_READY: Duration = Duration::from_secs(10);
const LIVE_WRITE: Duration = Duration::from_millis(250);
const LIVE_PARENT_POLL: Duration = Duration::from_millis(20);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LiveRequest {
    version: u32,
    nonce: Uuid,
    device: String,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LiveMessage {
    version: u32,
    nonce: Uuid,
    kind: LiveMessageKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action: Option<crate::devices::mx_creative::review_input::Action>,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LiveMessageKind {
    Ready,
    Action,
}

fn encode_live<T: Serialize>(value: &T) -> Result<Vec<u8>, Kind> {
    let frame = protocol::encode_frame(value)?;
    if frame.len() > MAX_LIVE_FRAME + 4 {
        return Err(Kind::FrameLimit);
    }
    Ok(frame)
}

fn wait_fd(fd: libc::c_int, events: i16, timeout: Duration) -> Result<i16, Kind> {
    let mut descriptor = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    let milliseconds = i32::try_from(timeout.as_millis().max(1)).unwrap_or(i32::MAX);
    let result = unsafe { libc::poll(&mut descriptor, 1, milliseconds) };
    if result < 0 {
        return if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            Ok(0)
        } else {
            Err(Kind::Transport)
        };
    }
    Ok(descriptor.revents)
}

fn read_exact_live<T: AsRawFd + Read>(
    reader: &mut T,
    bytes: usize,
    deadline: Instant,
) -> Result<Vec<u8>, Kind> {
    let mut result = vec![0; bytes];
    let mut offset = 0;
    while offset != bytes {
        match reader.read(&mut result[offset..]) {
            Ok(0) => return Err(Kind::Transport),
            Ok(count) => offset += count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err(Kind::TimedOut);
                }
                let events = wait_fd(
                    reader.as_raw_fd(),
                    libc::POLLIN,
                    deadline.saturating_duration_since(Instant::now()),
                )?;
                if events & (libc::POLLERR | libc::POLLNVAL) != 0 {
                    return Err(Kind::Transport);
                }
            }
            Err(_) => return Err(Kind::Transport),
        }
    }
    Ok(result)
}

fn control_open(reader: &impl AsRawFd) -> Result<bool, Kind> {
    let mut descriptor = libc::pollfd {
        fd: reader.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&mut descriptor, 1, 0) } < 0 {
        return Err(Kind::Transport);
    }
    if descriptor.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(Kind::Transport);
    }
    if descriptor.revents & (libc::POLLIN | libc::POLLHUP) == 0 {
        return Ok(true);
    }
    let mut byte = [0; 1];
    loop {
        match unsafe {
            libc::read(
                reader.as_raw_fd(),
                byte.as_mut_ptr().cast(),
                byte.len() as libc::size_t,
            )
        } {
            0 => return Ok(false),
            1 => return Err(Kind::Protocol),
            -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => continue,
            -1 if io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock => {
                return Ok(true);
            }
            _ => return Err(Kind::Transport),
        }
    }
}

fn read_live_frame<R: Read + AsRawFd>(reader: &mut R, deadline: Instant) -> Result<Vec<u8>, Kind> {
    let header = read_exact_live(reader, 4, deadline)?;
    let length = u32::from_le_bytes(header.try_into().expect("exact header")) as usize;
    if length == 0 || length > MAX_LIVE_FRAME {
        return Err(Kind::FrameLimit);
    }
    read_exact_live(reader, length, deadline)
}

fn write_live<T: Write + AsRawFd>(
    writer: &mut T,
    bytes: &[u8],
    deadline: Instant,
) -> Result<(), Kind> {
    let mut offset = 0;
    while offset < bytes.len() {
        match writer.write(&bytes[offset..]) {
            Ok(0) => return Err(Kind::Transport),
            Ok(count) => offset += count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                if Instant::now() >= deadline
                    || wait_fd(
                        writer.as_raw_fd(),
                        libc::POLLOUT,
                        deadline.saturating_duration_since(Instant::now()),
                    )
                    .is_err()
                {
                    return Err(Kind::Transport);
                }
            }
            Err(_) => return Err(Kind::Transport),
        }
    }
    Ok(())
}

pub(super) fn mx_creative_review_worker_main() -> Result<(), WorkerError> {
    let run = || -> Result<(), Kind> {
        verify_containment()?;
        let watchdog = ParentWatchdog::start_live()?;
        let mut stdin = duplicate_pipe(libc::STDIN_FILENO)?;
        let mut stdout = duplicate_pipe(libc::STDOUT_FILENO)?;
        let _stderr = duplicate_pipe(libc::STDERR_FILENO)?;
        make_nonblocking(&stdin, Instant::now() + LIVE_SETUP).map_err(|_| Kind::Transport)?;
        make_nonblocking(&stdout, Instant::now() + LIVE_SETUP).map_err(|_| Kind::Transport)?;
        let body = read_live_frame(&mut stdin, Instant::now() + LIVE_SETUP)?;
        let request: LiveRequest = protocol::decode_frame_body(&body)?;
        if request.version != LIVE_VERSION
            || request.nonce.is_nil()
            || request.device != "mx-creative"
        {
            return Err(Kind::InvalidRequest);
        }
        let _scope = WorkerScope::enter(ScopeAuthority::MxCreativeReview)?;
        let backend =
            crate::native::hid::mx_creative_review_backend().map_err(|_| Kind::Callback)?;
        let mut devices = crate::devices::mx_creative::probe::LiveDevices::open(backend.as_ref())
            .map_err(|_| Kind::Callback)?;
        let mut reducer = crate::devices::mx_creative::review_input::Reducer::default();
        let arm = Instant::now();
        while arm.elapsed() < crate::devices::mx_creative::review_input::ARM_DRAIN {
            if !control_open(&stdin)? || !parent_alive(watchdog.parent) {
                return Err(Kind::Cancelled);
            }
            for role in [
                crate::devices::mx_creative::probe::Role::Keypad,
                crate::devices::mx_creative::probe::Role::Dialpad,
            ] {
                match devices.read(role, crate::devices::mx_creative::review_input::READ_SLICE) {
                    Ok(report) => reducer.arm_baseline(role, &report),
                    Err(crate::native::hid::HidError::Timeout) => {}
                    Err(_) => return Err(Kind::Callback),
                }
            }
        }
        let ready = encode_live(&LiveMessage {
            version: LIVE_VERSION,
            nonce: request.nonce,
            kind: LiveMessageKind::Ready,
            action: None,
        })?;
        write_live(&mut stdout, &ready, Instant::now() + LIVE_WRITE)?;
        let started = Instant::now();
        loop {
            if !control_open(&stdin)? || !parent_alive(watchdog.parent) {
                return Ok(());
            }
            for role in [
                crate::devices::mx_creative::probe::Role::Keypad,
                crate::devices::mx_creative::probe::Role::Dialpad,
            ] {
                let report = match devices
                    .read(role, crate::devices::mx_creative::review_input::READ_SLICE)
                {
                    Ok(report) => report,
                    Err(crate::native::hid::HidError::Timeout) => continue,
                    Err(_) => return Err(Kind::Callback),
                };
                let Some(action) = reducer.accept(role, &report, started.elapsed()) else {
                    continue;
                };
                let frame = encode_live(&LiveMessage {
                    version: LIVE_VERSION,
                    nonce: request.nonce,
                    kind: LiveMessageKind::Action,
                    action: Some(action),
                })?;
                write_live(&mut stdout, &frame, Instant::now() + LIVE_WRITE)?;
            }
        }
    };
    run().map_err(WorkerError::before_launch)
}

#[derive(Default)]
struct LiveFrameBuffer {
    bytes: Vec<u8>,
    started: Option<Instant>,
}

impl LiveFrameBuffer {
    fn push(&mut self, bytes: &[u8]) -> Result<(), Kind> {
        if bytes.len() > MAX_DIAGNOSTIC_BYTES - self.bytes.len() {
            return Err(Kind::FrameLimit);
        }
        if self.bytes.is_empty() && !bytes.is_empty() {
            self.started = Some(Instant::now());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn pop(&mut self) -> Result<Option<LiveMessage>, Kind> {
        if self
            .started
            .is_some_and(|started| started.elapsed() > LIVE_WRITE)
        {
            return Err(Kind::TimedOut);
        }
        if self.bytes.len() < 4 {
            return Ok(None);
        }
        let length = u32::from_le_bytes(self.bytes[..4].try_into().unwrap()) as usize;
        if length == 0 || length > MAX_LIVE_FRAME {
            return Err(Kind::FrameLimit);
        }
        if self.bytes.len() < length + 4 {
            return Ok(None);
        }
        let message = protocol::decode_frame_body(&self.bytes[4..length + 4])?;
        self.bytes.drain(..length + 4);
        if self.bytes.is_empty() {
            self.started = None;
        } else {
            self.started = Some(Instant::now());
        }
        Ok(Some(message))
    }

    fn empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

fn accept_live_message(
    message: LiveMessage,
    nonce: Uuid,
    ready: &mut bool,
    output: &mut OutputGate,
) -> Result<(), Kind> {
    if message.version != LIVE_VERSION || message.nonce != nonce {
        return Err(Kind::Protocol);
    }
    match (message.kind, message.action) {
        (LiveMessageKind::Ready, None) if !*ready => {
            *ready = true;
            Ok(())
        }
        (LiveMessageKind::Action, Some(action)) if *ready => output.enqueue(action, Instant::now()),
        _ => Err(Kind::Protocol),
    }
}

struct OutputGate {
    pending: Vec<u8>,
    since: Option<Instant>,
}

impl OutputGate {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            since: None,
        }
    }

    fn enqueue(
        &mut self,
        action: crate::devices::mx_creative::review_input::Action,
        now: Instant,
    ) -> Result<(), Kind> {
        if !self.pending.is_empty() {
            return Err(Kind::Transport);
        }
        self.pending
            .extend_from_slice(action.json_line().as_bytes());
        self.since = Some(now);
        Ok(())
    }

    fn flush<W: Write>(&mut self, writer: &mut W, now: Instant) -> Result<(), Kind> {
        if self.pending.is_empty() {
            return Ok(());
        }
        if self
            .since
            .is_some_and(|since| now.saturating_duration_since(since) > LIVE_WRITE)
        {
            return Err(Kind::Transport);
        }
        match writer.write(&self.pending) {
            Ok(0) => Err(Kind::Transport),
            Ok(count) => {
                self.pending.drain(..count);
                if self.pending.is_empty() {
                    self.since = None;
                }
                Ok(())
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(())
            }
            Err(_) => Err(Kind::Transport),
        }
    }

    fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

fn accept_parent_input(read: io::Result<usize>) -> Result<bool, Kind> {
    match read {
        Ok(0) => Ok(false),
        Ok(_) => Err(Kind::Protocol),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(true)
        }
        Err(_) => Err(Kind::Transport),
    }
}

pub(super) fn supervise_mx_creative_review() -> Result<(), WorkerError> {
    let _active = SUPERVISOR.acquire().map_err(WorkerError::before_launch)?;
    if crate::native::portable::child::cleanup_blocked() {
        SUPERVISOR.block();
        return Err(WorkerError::before_launch(Kind::SupervisionBlocked));
    }
    let executable =
        std::env::current_exe().map_err(|_| WorkerError::before_launch(Kind::Spawn))?;
    let mut command = Command::new(executable);
    command
        .arg(super::MX_CREATIVE_REVIEW_HIDDEN_WORKER_ARGUMENT)
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
        let deadline = Instant::now() + LIVE_SETUP;
        let public_in = std::io::stdin();
        let public_out = std::io::stdout();
        let mut public_in = public_in.lock();
        let mut public_out = public_out.lock();
        for handle in [
            pipes.stdin.as_ref().map(|pipe| pipe.as_raw_fd()),
            pipes.stdout.as_ref().map(|pipe| pipe.as_raw_fd()),
            pipes.stderr.as_ref().map(|pipe| pipe.as_raw_fd()),
        ]
        .into_iter()
        .flatten()
        {
            let flags = unsafe { libc::fcntl(handle, libc::F_GETFL) };
            if flags < 0
                || unsafe { libc::fcntl(handle, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(Kind::Transport);
            }
        }
        for handle in [public_in.as_raw_fd(), public_out.as_raw_fd()] {
            let flags = unsafe { libc::fcntl(handle, libc::F_GETFL) };
            if flags < 0
                || unsafe { libc::fcntl(handle, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(Kind::Transport);
            }
        }
        let nonce = Uuid::new_v4();
        let request = encode_live(&LiveRequest {
            version: LIVE_VERSION,
            nonce,
            device: "mx-creative".into(),
        })?;
        let mut sent = 0;
        let ready_deadline = Instant::now() + LIVE_READY;
        let mut frames = LiveFrameBuffer::default();
        let mut ready = false;
        let mut output = OutputGate::new();
        let mut diagnostics = 0;
        loop {
            let mut external = libc::pollfd {
                fd: public_in.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&raw mut external, 1, 0) } < 0 {
                return Err(Kind::Transport);
            }
            if external.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                let mut one = [0; 1];
                if !accept_parent_input(public_in.read(&mut one))? {
                    return Ok(());
                }
            }
            if !ready && Instant::now() >= ready_deadline {
                return Err(Kind::TimedOut);
            }
            if sent < request.len() {
                match pipes
                    .stdin
                    .as_mut()
                    .ok_or(Kind::Transport)?
                    .write(&request[sent..])
                {
                    Ok(0) => return Err(Kind::Transport),
                    Ok(count) => sent += count,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return Err(Kind::Transport),
                }
                if sent < request.len() && Instant::now() >= deadline {
                    return Err(Kind::TimedOut);
                }
            }
            if output.is_pending() {
                output.flush(&mut public_out, Instant::now())?;
            }
            if !output.is_pending() {
                if !frames.empty()
                    && let Some(message) = frames.pop()?
                {
                    accept_live_message(message, nonce, &mut ready, &mut output)?;
                    continue;
                }
                let mut buffer = [0; 512];
                if let Some(stdout) = &mut pipes.stdout {
                    loop {
                        match stdout.read(&mut buffer) {
                            Ok(0) => return Err(Kind::Transport),
                            Ok(count) => {
                                frames.push(&buffer[..count])?;
                                if let Some(message) = frames.pop()? {
                                    accept_live_message(message, nonce, &mut ready, &mut output)?;
                                }
                                break;
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                            Err(_) => return Err(Kind::Transport),
                        }
                    }
                }
            }
            let mut buffer = [0; 512];
            if let Some(stderr) = &mut pipes.stderr {
                loop {
                    match stderr.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            if count > MAX_DIAGNOSTIC_BYTES - diagnostics {
                                return Err(Kind::DiagnosticLimit);
                            }
                            diagnostics += count;
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => return Err(Kind::Transport),
                    }
                }
            }
            if peek_exited(pid, Instant::now() + LIVE_PARENT_POLL).map_err(|_| Kind::Containment)? {
                return Err(Kind::WorkerFailed);
            }
            thread::sleep(LIVE_PARENT_POLL);
        }
    })();
    let exited = child.finish();
    let io_closed = pipes.close();
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
        Ok(()) => Ok(()),
        Err(kind) => Err(WorkerError::after_launch(kind, worker, true, true)),
    }
}

#[cfg(test)]
mod tests;
