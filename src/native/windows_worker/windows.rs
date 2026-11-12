use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::{size_of, size_of_val};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;
use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, FILETIME, GetHandleInformation, HANDLE,
    HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess, GetProcessId,
    GetProcessTimes, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::protocol::{self, RequestEnvelope, ResponseEnvelope};
use super::supervision::{Progress, SupervisorGate};
use super::{
    DiagnosticAllowlist, DiagnosticResult, DiagnosticWorkerRoute, WorkerError,
    WorkerErrorKind as Kind, WorkerIdentity,
};

const POLL: Duration = Duration::from_millis(10);
const EXIT_CLEANUP_BUDGET: Duration = Duration::from_secs(5);
const IO_CLEANUP_BUDGET: Duration = Duration::from_secs(1);
static SUPERVISOR: SupervisorGate = SupervisorGate::new();

fn handle(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle()
}

pub(super) fn new_nonce() -> Result<Uuid, Kind> {
    let mut bytes = [0; 16];
    // The system RNG is used only for correlation, not same-user authentication.
    let status = unsafe {
        BCryptGenRandom(
            null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status < 0 {
        return Err(Kind::Containment);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Uuid::from_bytes(bytes))
}

fn set_inheritance(raw: HANDLE, inheritable: bool) -> Result<(), Kind> {
    let desired = if inheritable { HANDLE_FLAG_INHERIT } else { 0 };
    // `raw` is a live handle owned by this process.
    if unsafe { SetHandleInformation(raw, HANDLE_FLAG_INHERIT, desired) } == 0 {
        return Err(Kind::Containment);
    }
    let mut flags = 0;
    if unsafe { GetHandleInformation(raw, &mut flags) } == 0
        || (flags & HANDLE_FLAG_INHERIT != 0) != inheritable
    {
        return Err(Kind::Containment);
    }
    Ok(())
}

fn pipe() -> Result<(OwnedHandle, OwnedHandle), Kind> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 0,
    };
    let mut read = null_mut();
    let mut write = null_mut();
    // Successful CreatePipe transfers ownership of both new handles.
    if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
        return Err(Kind::Spawn);
    }
    let read = unsafe { OwnedHandle::from_raw_handle(read) };
    let write = unsafe { OwnedHandle::from_raw_handle(write) };
    Ok((read, write))
}

struct Pipes {
    parent_stdin: OwnedHandle,
    parent_stdout: OwnedHandle,
    parent_stderr: OwnedHandle,
    child_stdin: OwnedHandle,
    child_stdout: OwnedHandle,
    child_stderr: OwnedHandle,
}

impl Pipes {
    fn new() -> Result<Self, Kind> {
        let (child_stdin, parent_stdin) = pipe()?;
        let (parent_stdout, child_stdout) = pipe()?;
        let (parent_stderr, child_stderr) = pipe()?;
        for parent in [&parent_stdin, &parent_stdout, &parent_stderr] {
            set_inheritance(handle(parent), false)?;
        }
        for child in [&child_stdin, &child_stdout, &child_stderr] {
            set_inheritance(handle(child), false)?;
        }
        Ok(Self {
            parent_stdin,
            parent_stdout,
            parent_stderr,
            child_stdin,
            child_stdout,
            child_stderr,
        })
    }
}

fn job() -> Result<OwnedHandle, Kind> {
    // Null security attributes and name create a private, noninheritable Job.
    let raw = unsafe { CreateJobObjectW(null(), null()) };
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        return Err(Kind::Containment);
    }
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    set_inheritance(raw, false)?;

    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    if unsafe {
        SetInformationJobObject(
            raw,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(Kind::Containment);
    }
    let mut actual = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    if unsafe {
        QueryInformationJobObject(
            raw,
            JobObjectExtendedLimitInformation,
            (&mut actual as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&actual) as u32,
            null_mut(),
        )
    } == 0
    {
        return Err(Kind::Containment);
    }
    let flags = actual.BasicLimitInformation.LimitFlags;
    let required = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
    let breakaway = JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
    if flags & required != required
        || flags & breakaway != 0
        || actual.BasicLimitInformation.ActiveProcessLimit != 1
    {
        return Err(Kind::Containment);
    }
    Ok(job)
}

struct AttributeList {
    _storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}

impl AttributeList {
    fn new(handles: &[HANDLE; 3]) -> Result<Self, Kind> {
        let mut bytes = 0;
        // The documented sizing call intentionally passes a null list.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes) };
        if bytes == 0 || bytes > 1_048_576 {
            return Err(Kind::Containment);
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = storage.as_mut_ptr().cast::<c_void>();
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut bytes) } == 0 {
            return Err(Kind::Containment);
        }
        let attributes = Self {
            _storage: storage,
            list,
        };
        // The handle array stays alive and unchanged through CreateProcessW.
        if unsafe {
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                size_of_val(handles),
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(Kind::Containment);
        }
        Ok(attributes)
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

fn executable_command(route: DiagnosticWorkerRoute) -> Result<(Vec<u16>, Vec<u16>), Kind> {
    let path = std::env::current_exe().map_err(|_| Kind::Spawn)?;
    let mut application: Vec<u16> = path.as_os_str().encode_wide().collect();
    if application.is_empty() || application.contains(&0) || application.contains(&(b'"' as u16)) {
        return Err(Kind::Spawn);
    }
    let trailing_slashes = application
        .iter()
        .rev()
        .take_while(|character| **character == b'\\' as u16)
        .count();
    let mut command = Vec::with_capacity(application.len() + trailing_slashes + 32);
    command.push(b'"' as u16);
    command.extend_from_slice(&application);
    command.extend(std::iter::repeat_n(b'\\' as u16, trailing_slashes));
    command.push(b'"' as u16);
    command.push(b' ' as u16);
    command.extend(route.argument().encode_utf16());
    command.push(0);
    application.push(0);
    Ok((application, command))
}

fn identity(process: HANDLE) -> Result<WorkerIdentity, Kind> {
    let pid = unsafe { GetProcessId(process) };
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if pid == 0
        || unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) }
            == 0
    {
        return Err(Kind::Containment);
    }
    let creation_time_100ns =
        (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    if creation_time_100ns == 0 {
        return Err(Kind::Containment);
    }
    Ok(WorkerIdentity {
        pid,
        creation_time_100ns,
    })
}

type IoTask<T> = JoinHandle<Result<T, Kind>>;

#[derive(Default)]
struct IoTasks {
    send: Option<IoTask<()>>,
    response: Option<IoTask<ResponseEnvelope>>,
    stderr: Option<IoTask<()>>,
}

impl IoTasks {
    fn cancel(&self) {
        for raw in [
            self.send.as_ref().map(AsRawHandle::as_raw_handle),
            self.response.as_ref().map(AsRawHandle::as_raw_handle),
            self.stderr.as_ref().map(AsRawHandle::as_raw_handle),
        ]
        .into_iter()
        .flatten()
        {
            // Repeat during cleanup to cover a thread entering I/O after a cancel.
            unsafe { CancelSynchronousIo(raw) };
        }
    }

    fn is_empty(&self) -> bool {
        self.send.is_none() && self.response.is_none() && self.stderr.is_none()
    }

    fn discard_finished(&mut self) {
        let _ = join_finished(&mut self.send);
        let _ = join_finished(&mut self.response);
        let _ = join_finished(&mut self.stderr);
    }
}

fn join_finished<T>(task: &mut Option<IoTask<T>>) -> Option<Result<T, Kind>> {
    // Rust's is_finished can precede OS-thread termination (for example,
    // while TLS destructors run). Join only a signalled native thread.
    if !task.as_ref().is_some_and(|thread| {
        (unsafe { WaitForSingleObject(thread.as_raw_handle(), 0) }) == WAIT_OBJECT_0
    }) {
        return None;
    }
    let task = task.take().expect("finished task exists");
    Some(task.join().unwrap_or(Err(Kind::Transport)))
}

struct StoppableFile {
    file: File,
    stop: Arc<AtomicBool>,
}

impl StoppableFile {
    fn check(&self) -> io::Result<()> {
        if self.stop.load(Ordering::Acquire) {
            Err(io::Error::other("diagnostic I/O stopped"))
        } else {
            Ok(())
        }
    }
}

impl Read for StoppableFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        self.file.read(buffer)
    }
}

impl Write for StoppableFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.check()?;
        self.file.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        self.file.flush()
    }
}

struct Child {
    job: OwnedHandle,
    process: OwnedHandle,
    thread: OwnedHandle,
    process_id: u32,
    identity: Option<WorkerIdentity>,
    observed_exit: bool,
    stop: Arc<AtomicBool>,
    io: IoTasks,
    clean: bool,
}

impl Child {
    fn launch(
        job: OwnedHandle,
        stop: Arc<AtomicBool>,
        application: &[u16],
        command: &mut [u16],
        attributes: &AttributeList,
        inherited: &[HANDLE; 3],
    ) -> Result<Self, Kind> {
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = inherited[0];
        startup.StartupInfo.hStdOutput = inherited[1];
        startup.StartupInfo.hStdError = inherited[2];
        startup.lpAttributeList = attributes.list;
        let mut process = PROCESS_INFORMATION::default();
        // The explicit application and inherited-handle list preclude a shell or
        // accidental inheritance of unrelated handles.
        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                1,
                CREATE_SUSPENDED | DETACHED_PROCESS | EXTENDED_STARTUPINFO_PRESENT,
                null(),
                null(),
                &startup.StartupInfo,
                &mut process,
            )
        };
        if created == 0 {
            return Err(Kind::Spawn);
        }
        // Construct the guard before any fallible post-create operation.
        Ok(Self {
            job,
            process: unsafe { OwnedHandle::from_raw_handle(process.hProcess) },
            thread: unsafe { OwnedHandle::from_raw_handle(process.hThread) },
            process_id: process.dwProcessId,
            identity: None,
            observed_exit: false,
            stop,
            io: IoTasks::default(),
            clean: false,
        })
    }

    fn contain(&mut self) -> Result<WorkerIdentity, Kind> {
        let current = identity(handle(&self.process))?;
        if current.pid != self.process_id {
            return Err(Kind::Containment);
        }
        self.identity = Some(current);
        if unsafe { AssignProcessToJobObject(handle(&self.job), handle(&self.process)) } == 0 {
            return Err(Kind::Containment);
        }
        let mut in_job = 0;
        if unsafe { IsProcessInJob(handle(&self.process), handle(&self.job), &mut in_job) } == 0
            || in_job == 0
        {
            return Err(Kind::Containment);
        }
        Ok(current)
    }

    fn start_io(
        &mut self,
        stdin: OwnedHandle,
        stdout: OwnedHandle,
        stderr: OwnedHandle,
        frame: Vec<u8>,
    ) -> Result<(), Kind> {
        let stop = Arc::clone(&self.stop);
        self.io.send = Some(
            thread::Builder::new()
                .spawn(move || {
                    let mut output = StoppableFile {
                        file: File::from(stdin),
                        stop,
                    };
                    output.write_all(&frame).map_err(|_| Kind::Transport)
                })
                .map_err(|_| Kind::Containment)?,
        );
        let stop = Arc::clone(&self.stop);
        self.io.response = Some(
            thread::Builder::new()
                .spawn(move || {
                    let mut input = StoppableFile {
                        file: File::from(stdout),
                        stop,
                    };
                    protocol::read_frame(&mut input)
                })
                .map_err(|_| Kind::Containment)?,
        );
        let stop = Arc::clone(&self.stop);
        self.io.stderr = Some(
            thread::Builder::new()
                .spawn(move || {
                    let mut input = StoppableFile {
                        file: File::from(stderr),
                        stop,
                    };
                    protocol::drain_diagnostics(&mut input)
                })
                .map_err(|_| Kind::Containment)?,
        );
        Ok(())
    }

    fn resume(&self) -> Result<(), Kind> {
        if unsafe { ResumeThread(handle(&self.thread)) } != 1 {
            return Err(Kind::Containment);
        }
        Ok(())
    }

    fn poll_exit(&mut self) -> Option<Result<u32, Kind>> {
        match unsafe { WaitForSingleObject(handle(&self.process), 0) } {
            WAIT_OBJECT_0 => {
                self.observed_exit = true;
                let mut code = 0;
                if unsafe { GetExitCodeProcess(handle(&self.process), &mut code) } == 0 {
                    Some(Err(Kind::WorkerFailed))
                } else {
                    Some(Ok(code))
                }
            }
            WAIT_TIMEOUT => None,
            _ => Some(Err(Kind::Containment)),
        }
    }

    fn wait(
        &mut self,
        nonce: Uuid,
        worker: WorkerIdentity,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Value, Kind> {
        let mut progress = Progress::default();
        loop {
            if let Some(result) = join_finished(&mut self.io.send) {
                progress.send = Some(result);
            }
            if let Some(result) = join_finished(&mut self.io.response) {
                progress.response = Some(
                    result
                        .and_then(|response| protocol::validate_response(response, nonce, worker)),
                );
            }
            if let Some(result) = join_finished(&mut self.io.stderr) {
                progress.stderr = Some(result);
            }
            if progress.exit.is_none() {
                progress.exit = self.poll_exit();
            }
            if let Some(result) = progress.decide(
                cancelled.load(Ordering::Acquire),
                Instant::now() >= deadline,
            ) {
                return result;
            }
            thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    fn terminate(&self) {
        // Terminate both: assignment may have failed while the child was suspended.
        unsafe {
            TerminateJobObject(handle(&self.job), 1);
            TerminateProcess(handle(&self.process), 1);
        }
    }

    fn wait_for_exit(&mut self) -> bool {
        if self.observed_exit {
            return true;
        }
        let end = Instant::now() + EXIT_CLEANUP_BUDGET;
        loop {
            match unsafe { WaitForSingleObject(handle(&self.process), POLL.as_millis() as u32) } {
                WAIT_OBJECT_0 => {
                    self.observed_exit = true;
                    return true;
                }
                WAIT_TIMEOUT if Instant::now() < end => {}
                _ => return false,
            }
        }
    }

    fn stop_io(&mut self) -> bool {
        self.stop.store(true, Ordering::Release);
        let end = Instant::now() + IO_CLEANUP_BUDGET;
        loop {
            self.io.discard_finished();
            if self.io.is_empty() {
                return true;
            }
            self.io.cancel();
            if Instant::now() >= end {
                return false;
            }
            thread::sleep(POLL);
        }
    }

    fn fail(&mut self, kind: Kind) -> WorkerError {
        if !self.observed_exit {
            self.terminate();
        }
        let exited = self.wait_for_exit();
        let joined = self.stop_io();
        let error = WorkerError::after_launch(kind, self.identity, exited, joined);
        if error.replacement_allowed {
            self.clean = true;
        } else {
            SUPERVISOR.block();
        }
        error
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if !self.clean {
            SUPERVISOR.block();
            self.stop.store(true, Ordering::Release);
            self.io.cancel();
            self.terminate();
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
    let _active = SUPERVISOR.acquire().map_err(WorkerError::before_launch)?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| WorkerError::before_launch(Kind::Containment))?;
    let stop = Arc::new(AtomicBool::new(false));
    let Pipes {
        parent_stdin,
        parent_stdout,
        parent_stderr,
        child_stdin,
        child_stdout,
        child_stderr,
    } = Pipes::new().map_err(WorkerError::before_launch)?;
    let job = job().map_err(WorkerError::before_launch)?;
    let (application, mut command) =
        executable_command(route).map_err(WorkerError::before_launch)?;
    for child in [&child_stdin, &child_stdout, &child_stderr] {
        set_inheritance(handle(child), true).map_err(WorkerError::before_launch)?;
    }
    let inherited = [
        handle(&child_stdin),
        handle(&child_stdout),
        handle(&child_stderr),
    ];
    let attributes = AttributeList::new(&inherited).map_err(WorkerError::before_launch)?;
    checkpoint(cancelled, deadline).map_err(WorkerError::before_launch)?;
    let mut child = Child::launch(
        job,
        stop,
        &application,
        &mut command,
        &attributes,
        &inherited,
    )
    .map_err(WorkerError::before_launch)?;
    drop(attributes);
    drop((child_stdin, child_stdout, child_stderr));

    let run = (|| {
        let worker = child.contain()?;
        checkpoint(cancelled, deadline)?;
        child.start_io(parent_stdin, parent_stdout, parent_stderr, frame)?;
        checkpoint(cancelled, deadline)?;
        child.resume()?;
        let payload = child.wait(nonce, worker, deadline, cancelled)?;
        Ok(DiagnosticResult { worker, payload })
    })();

    match run {
        Ok(result) => {
            child.clean = true;
            Ok(result)
        }
        Err(kind) => Err(child.fail(kind)),
    }
}

fn duplicate_pipe(which: STD_HANDLE) -> Result<File, Kind> {
    let original = unsafe { GetStdHandle(which) };
    if original.is_null()
        || original == INVALID_HANDLE_VALUE
        || unsafe { GetFileType(original) } != FILE_TYPE_PIPE
    {
        return Err(Kind::Containment);
    }
    set_inheritance(original, false)?;
    let current = unsafe { GetCurrentProcess() };
    let mut duplicate = null_mut();
    if unsafe {
        DuplicateHandle(
            current,
            original,
            current,
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(Kind::Containment);
    }
    let duplicate = unsafe { OwnedHandle::from_raw_handle(duplicate) };
    set_inheritance(handle(&duplicate), false)?;
    Ok(File::from(duplicate))
}

#[cfg(any(feature = "micro-hid", feature = "akp03-hid"))]
pub(super) fn verify_hid_containment() -> bool {
    use windows_sys::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};

    let mut in_job = 0;
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    let mut process_ids = [0; 1];
    // SAFETY: fixed-size out-structures; null Job selects the current process's
    // Job. These queries do not touch devices.
    let valid = unsafe {
        IsProcessInJob(GetCurrentProcess(), null_mut(), &mut in_job) != 0
            && QueryInformationJobObject(
                null_mut(),
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of_val(&limits) as u32,
                null_mut(),
            ) != 0
            && GetConsoleWindow().is_null()
            && GetConsoleProcessList(process_ids.as_mut_ptr(), 1) == 0
    };
    let flags = limits.BasicLimitInformation.LimitFlags;
    let required = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
    valid
        && in_job != 0
        && limits.BasicLimitInformation.ActiveProcessLimit == 1
        && flags & required == required
        && flags & (JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK) == 0
}

pub(super) fn worker_main(allowlist: &DiagnosticAllowlist) -> Result<(), WorkerError> {
    let run = || -> Result<(), Kind> {
        let mut stdin = duplicate_pipe(STD_INPUT_HANDLE)?;
        let mut stdout = duplicate_pipe(STD_OUTPUT_HANDLE)?;
        let _stderr = duplicate_pipe(STD_ERROR_HANDLE)?;

        let envelope: RequestEnvelope = protocol::read_frame(&mut stdin)?;
        protocol::validate_request(&envelope, allowlist)?;
        let worker = identity(unsafe { GetCurrentProcess() })?;
        let callback = allowlist
            .find(&envelope.request.kind)
            .ok_or(Kind::InvalidRequest)?;
        #[cfg(any(feature = "micro-hid", feature = "akp03-hid"))]
        let _hid_scope = match envelope.request.kind.as_str() {
            "micro_hid_probe" | "akp03_hid_probe" => {
                crate::cli::validate_worker_options(
                    envelope.request.payload.clone(),
                    envelope.request.kind == "micro_hid_probe",
                )
                .map_err(|_| Kind::InvalidRequest)?;
                Some(super::hid_scope::HidWorkerScope::enter(
                    envelope.request.payload["health_check"] == false,
                    new_nonce()?,
                    verify_hid_containment,
                )?)
            }
            _ => None,
        };
        let outcome = protocol::invoke_callback(callback, envelope.request.payload);
        let frame = protocol::encode_response(&outcome, envelope.request_nonce, worker)?;
        stdout.write_all(&frame).map_err(|_| Kind::Transport)?;
        Ok(())
    };
    run().map_err(WorkerError::before_launch)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use serde_json::json;
    use windows_sys::Win32::System::Threading::CreateEventW;

    use super::super::{DiagnosticCallback, DiagnosticCallbackError, DiagnosticRequest, probe};
    use super::*;

    fn unused_callback(_: Value) -> Result<Value, DiagnosticCallbackError> {
        Err(DiagnosticCallbackError)
    }

    static CALLBACKS: [DiagnosticCallback; 1] =
        [DiagnosticCallback::new("cleanup_test", unused_callback)];

    fn event() -> OwnedHandle {
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        assert!(!event.is_null());
        unsafe { OwnedHandle::from_raw_handle(event) }
    }

    #[test]
    fn new_pipe_ends_start_noninheritable() {
        let pipes = Pipes::new().unwrap();
        for pipe in [
            &pipes.parent_stdin,
            &pipes.parent_stdout,
            &pipes.parent_stderr,
            &pipes.child_stdin,
            &pipes.child_stdout,
            &pipes.child_stderr,
        ] {
            let mut flags = 0;
            assert_ne!(unsafe { GetHandleInformation(handle(pipe), &mut flags) }, 0);
            assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
        }
    }

    #[test]
    fn unresolved_io_blocks_later_probes_even_after_return() {
        let (release, blocked) = mpsc::channel::<()>();
        let mut child = Child {
            job: job().unwrap(),
            // These synthetic signalled-lifetime stand-ins are never executed.
            process: event(),
            thread: event(),
            process_id: 0,
            identity: None,
            observed_exit: true,
            stop: Arc::new(AtomicBool::new(false)),
            io: IoTasks::default(),
            clean: false,
        };
        child.io.send = Some(thread::spawn(move || {
            let _ = blocked.recv();
            Ok(())
        }));
        let error = child.fail(Kind::TimedOut);
        assert_eq!(error.kind, Kind::CleanupUnconfirmed);
        assert!(!error.replacement_allowed);
        assert!(matches!(
            SUPERVISOR.acquire(),
            Err(Kind::SupervisionBlocked)
        ));
        let allowlist = DiagnosticAllowlist::new(&CALLBACKS).unwrap();
        let blocked_error = probe(
            &DiagnosticRequest {
                kind: "cleanup_test".to_owned(),
                payload: json!({}),
            },
            Duration::from_secs(1),
            &AtomicBool::new(false),
            &allowlist,
        )
        .unwrap_err();
        assert_eq!(blocked_error.kind, Kind::SupervisionBlocked);
        assert!(!blocked_error.replacement_allowed);
        let task = child.io.send.take().unwrap();
        drop(child);
        release.send(()).unwrap();
        task.join().unwrap().unwrap();
    }
}
