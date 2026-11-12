use std::{
    mem, ptr,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Devices::Communication::{
        CLEAR_COMM_ERROR_FLAGS, COMMTIMEOUTS, COMSTAT, ClearCommError, DCB, GetCommState,
        GetCommTimeouts, NOPARITY, ONESTOPBIT, SetCommState, SetCommTimeouts,
    },
    Foundation::{
        CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING,
        ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
        LocalFree, WAIT_ABANDONED, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    },
    Security::{GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser},
    Storage::FileSystem::{CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, ReadFile, WriteFile},
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Threading::{
            CreateEventW, CreateMutexW, GetCurrentProcess, OpenProcessToken, ReleaseMutex, Sleep,
            WaitForSingleObject,
        },
    },
};

use super::{
    Evidence, Options, Phase, PhaseProgress, ProbeErrorKind, ProbeFailure, ProbeProgress,
    ProbeReport, Progress, ReadResult, SerialDiagnostics, SerialSettings, Stage, Transport,
    TransportError, run_with_evidence, static_modem_modes, win32_error_kind,
};

const DTR_MASK: u32 = 0x30;
const RTS_MASK: u32 = 0x3000;
const FLOW_AND_SUBSTITUTION_MASK: u32 = 0x0000_4fce;

struct WinHandle(HANDLE);

impl WinHandle {
    fn new(handle: HANDLE) -> Result<Self, u32> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(unsafe { GetLastError() })
        } else {
            Ok(Self(handle))
        }
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for WinHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct MutexLease {
    handle: WinHandle,
    owned: bool,
}

impl Drop for MutexLease {
    fn drop(&mut self) {
        if self.owned {
            unsafe {
                ReleaseMutex(self.handle.raw());
            }
        }
    }
}

struct Operation {
    overlapped: OVERLAPPED,
    _event: WinHandle,
    buffer: Vec<u8>,
}

impl Operation {
    fn new(buffer: Vec<u8>) -> Result<Box<Self>, u32> {
        let event = WinHandle::new(unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) })?;
        Ok(Box::new(Self {
            overlapped: OVERLAPPED {
                hEvent: event.raw(),
                ..OVERLAPPED::default()
            },
            _event: event,
            buffer,
        }))
    }
}

struct Trace {
    start: Instant,
    phases: Vec<PhaseProgress>,
    phase_started_ms: u32,
}

impl Trace {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            phases: vec![PhaseProgress {
                phase: Phase::Preparing,
                started_ms: 0,
                elapsed_ms: 0,
            }],
            phase_started_ms: 0,
        }
    }

    fn elapsed(&self) -> u32 {
        self.start.elapsed().as_millis().min(u128::from(u32::MAX)) as u32
    }

    fn enter(&mut self, phase: Phase) {
        let now = self.elapsed();
        if let Some(last) = self.phases.last_mut() {
            last.elapsed_ms = now.saturating_sub(self.phase_started_ms);
        }
        if self.phases.len() < 14
            && phase_rank(phase) > phase_rank(self.phases.last().unwrap().phase)
        {
            self.phases.push(PhaseProgress {
                phase,
                started_ms: now,
                elapsed_ms: 0,
            });
            self.phase_started_ms = now;
        }
    }

    fn progress(
        &mut self,
        opened_at_ms: Option<u32>,
        configured_at_ms: Option<u32>,
        serial: Option<SerialDiagnostics>,
    ) -> ProbeProgress {
        let mut progress = Progress::new(Evidence::NativeTransportObservation);
        progress.seed(&self.phases);
        progress.record.elapsed_ms = self.elapsed();
        progress.record.port_opened_at_ms = opened_at_ms;
        progress.record.port_configured_at_ms = configured_at_ms;
        progress.record.serial = serial;
        if let Some(last) = progress.record.phases.last_mut() {
            last.elapsed_ms = progress.record.elapsed_ms.saturating_sub(last.started_ms);
        }
        progress.record
    }
}

fn phase_rank(phase: Phase) -> u8 {
    match phase {
        Phase::Preparing => 0,
        Phase::Discovery => 1,
        Phase::Open => 2,
        Phase::AssociationCheck => 3,
        Phase::Configure => 4,
        Phase::UpgradeWrite => 5,
        Phase::UpgradeResponse => 6,
        Phase::HeaderValidation => 7,
        Phase::IdentityWrite => 8,
        Phase::IdentityResponse => 9,
        Phase::FirmwareWrite => 10,
        Phase::FirmwareResponse => 11,
        Phase::Pattern => 12,
        Phase::Capture => 13,
    }
}

pub(super) struct WindowsSerialTransport {
    file: Option<WinHandle>,
    lease: Option<MutexLease>,
    started: Instant,
    trace: Trace,
    phases: Vec<PhaseProgress>,
    opened_at_ms: Option<u32>,
    configured_at_ms: Option<u32>,
    diagnostics: SerialDiagnostics,
    settings: SerialSettings,
    poisoned: bool,
}

impl WindowsSerialTransport {
    pub(super) fn open(cancelled: &std::sync::atomic::AtomicBool) -> Result<Self, ProbeFailure> {
        use std::sync::atomic::Ordering;

        let mut trace = Trace::new();
        let mut opened_at_ms = None;
        let mut configured_at_ms = None;
        let mut lease = None;
        let mut file = None;
        let mut settings = None;

        let result = (|| {
            if cancelled.load(Ordering::Acquire) {
                return Err((ProbeErrorKind::Cancelled, Stage::Open, None));
            }
            trace.enter(Phase::Discovery);
            let acquired =
                acquire_mutex().map_err(|(kind, code)| (kind, Stage::Discovery, code))?;
            lease = Some(acquired);
            let port = discover_port().map_err(|kind| (kind, Stage::Discovery, None))?;
            if cancelled.load(Ordering::Acquire) {
                return Err((ProbeErrorKind::Cancelled, Stage::Open, None));
            }

            trace.enter(Phase::Open);
            let path = format!(r"\\.\{port}");
            let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
            let raw = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    windows_sys::Win32::Foundation::GENERIC_READ
                        | windows_sys::Win32::Foundation::GENERIC_WRITE,
                    0,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    ptr::null_mut(),
                )
            };
            file = Some(
                WinHandle::new(raw)
                    .map_err(|code| (win32_error_kind(code), Stage::Open, Some(code)))?,
            );
            opened_at_ms = Some(trace.elapsed());
            if cancelled.load(Ordering::Acquire) {
                return Err((ProbeErrorKind::Cancelled, Stage::Open, None));
            }

            trace.enter(Phase::AssociationCheck);
            let current = discover_port().map_err(|kind| (kind, Stage::Discovery, None))?;
            if current != port {
                return Err((ProbeErrorKind::AssociationChanged, Stage::Discovery, None));
            }
            if cancelled.load(Ordering::Acquire) {
                return Err((ProbeErrorKind::Cancelled, Stage::Configure, None));
            }

            trace.enter(Phase::Configure);
            let serial = file.as_ref().expect("opened handle").raw();
            let mut dcb = DCB {
                DCBlength: mem::size_of::<DCB>() as u32,
                ..DCB::default()
            };
            if unsafe { GetCommState(serial, &mut dcb) } == 0 {
                let code = unsafe { GetLastError() };
                return Err((win32_error_kind(code), Stage::Configure, Some(code)));
            }
            let Some((dtr_mode, rts_mode)) = static_modem_modes(dcb._bitfield) else {
                return Err((
                    ProbeErrorKind::InvalidSerialSettings,
                    Stage::Configure,
                    None,
                ));
            };
            let dtr = dcb._bitfield & DTR_MASK;
            let rts = dcb._bitfield & RTS_MASK;
            dcb.BaudRate = 256_000;
            dcb.ByteSize = 8;
            dcb.Parity = NOPARITY;
            dcb.StopBits = ONESTOPBIT;
            dcb._bitfield = (dcb._bitfield & !FLOW_AND_SUBSTITUTION_MASK) | 1 | dtr | rts;
            if unsafe { SetCommState(serial, &dcb) } == 0 {
                let code = unsafe { GetLastError() };
                return Err((win32_error_kind(code), Stage::Configure, Some(code)));
            }
            let timeouts = COMMTIMEOUTS {
                ReadIntervalTimeout: u32::MAX,
                ReadTotalTimeoutMultiplier: u32::MAX,
                ReadTotalTimeoutConstant: 50,
                WriteTotalTimeoutMultiplier: 0,
                WriteTotalTimeoutConstant: 1_000,
            };
            if unsafe { SetCommTimeouts(serial, &timeouts) } == 0 {
                let code = unsafe { GetLastError() };
                return Err((win32_error_kind(code), Stage::Configure, Some(code)));
            }
            let mut verified = DCB {
                DCBlength: mem::size_of::<DCB>() as u32,
                ..DCB::default()
            };
            if unsafe { GetCommState(serial, &mut verified) } == 0 {
                let code = unsafe { GetLastError() };
                return Err((win32_error_kind(code), Stage::Configure, Some(code)));
            }
            if verified.BaudRate != 256_000
                || verified.ByteSize != 8
                || verified.Parity != NOPARITY
                || verified.StopBits != ONESTOPBIT
                || verified._bitfield & DTR_MASK != dtr
                || verified._bitfield & RTS_MASK != rts
                || verified._bitfield & FLOW_AND_SUBSTITUTION_MASK != 0
                || verified._bitfield & 1 == 0
            {
                return Err((
                    ProbeErrorKind::InvalidSerialSettings,
                    Stage::Configure,
                    None,
                ));
            }
            let mut verified_timeouts = COMMTIMEOUTS::default();
            if unsafe { GetCommTimeouts(serial, &mut verified_timeouts) } == 0 {
                let code = unsafe { GetLastError() };
                return Err((win32_error_kind(code), Stage::Configure, Some(code)));
            }
            if verified_timeouts.ReadIntervalTimeout != u32::MAX
                || verified_timeouts.ReadTotalTimeoutMultiplier != u32::MAX
                || verified_timeouts.ReadTotalTimeoutConstant != 50
                || verified_timeouts.WriteTotalTimeoutMultiplier != 0
                || verified_timeouts.WriteTotalTimeoutConstant != 1_000
            {
                return Err((
                    ProbeErrorKind::InvalidSerialSettings,
                    Stage::Configure,
                    None,
                ));
            }
            configured_at_ms = Some(trace.elapsed());
            settings = Some(SerialSettings {
                baud_rate: 256_000,
                data_bits: 8,
                parity: "none".into(),
                stop_bits: 1,
                binary: true,
                flow_control: "none".into(),
                nul_stripping: false,
                dtr: dtr_mode,
                rts: rts_mode,
            });
            if cancelled.load(Ordering::Acquire) {
                return Err((ProbeErrorKind::Cancelled, Stage::Configure, None));
            }
            Ok(())
        })();

        if let Err((kind, stage, native_code)) = result {
            return Err(ProbeFailure {
                kind,
                stage,
                native_code,
                progress: Box::new(trace.progress(opened_at_ms, configured_at_ms, None)),
            });
        }
        Ok(Self {
            file,
            lease,
            started: trace.start,
            phases: trace.phases.clone(),
            trace,
            opened_at_ms,
            configured_at_ms,
            diagnostics: SerialDiagnostics::default(),
            settings: settings.expect("serial setup completed"),
            poisoned: false,
        })
    }

    fn elapsed(&self) -> u64 {
        self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }

    fn raw_file(&self) -> HANDLE {
        self.file.as_ref().map_or(ptr::null_mut(), WinHandle::raw)
    }

    fn run_operation(
        &mut self,
        operation: Box<Operation>,
        deadline_ms: u64,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<(Vec<u8>, usize, Option<ProbeErrorKind>), TransportError> {
        use std::sync::atomic::Ordering;

        let file = self.raw_file();
        if file.is_null() || self.poisoned {
            return Err(TransportError {
                kind: ProbeErrorKind::TransportContract,
                native_code: None,
            });
        }
        let mut requested_interrupt = None;
        let mut cancel_requested = false;
        let mut cleanup_deadline = None;
        let mut transferred = 0_u32;
        loop {
            let completed =
                unsafe { GetOverlappedResult(file, &operation.overlapped, &mut transferred, 0) };
            if completed != 0 {
                if requested_interrupt.is_none() {
                    if cancelled.load(Ordering::Acquire) {
                        requested_interrupt = Some(ProbeErrorKind::Cancelled);
                    } else if self.elapsed() >= deadline_ms {
                        requested_interrupt = Some(ProbeErrorKind::Timeout);
                    }
                }
                return Ok((operation.buffer, transferred as usize, requested_interrupt));
            }
            let code = unsafe { GetLastError() };
            if code != ERROR_IO_INCOMPLETE && code != ERROR_IO_PENDING {
                if requested_interrupt.is_some()
                    && (code == ERROR_OPERATION_ABORTED || code == ERROR_NOT_FOUND)
                {
                    return Ok((operation.buffer, 0, requested_interrupt));
                }
                return Err(TransportError {
                    kind: win32_error_kind(code),
                    native_code: Some(code),
                });
            }

            if !cancel_requested
                && (cancelled.load(Ordering::Acquire) || self.elapsed() >= deadline_ms)
            {
                requested_interrupt = Some(if cancelled.load(Ordering::Acquire) {
                    ProbeErrorKind::Cancelled
                } else {
                    ProbeErrorKind::Timeout
                });
                unsafe {
                    CancelIoEx(file, &operation.overlapped);
                }
                cancel_requested = true;
                cleanup_deadline = Some(Instant::now() + Duration::from_millis(500));
            } else if let Some(cleanup_deadline) = cleanup_deadline
                && Instant::now() >= cleanup_deadline
            {
                self.poisoned = true;
                mem::forget(operation);
                return Err(TransportError {
                    kind: ProbeErrorKind::CleanupPending,
                    native_code: None,
                });
            }
            unsafe {
                Sleep(5);
            }
        }
    }

    fn line_status(&mut self) -> Result<(), TransportError> {
        self.diagnostics.line_status_checks = self.diagnostics.line_status_checks.saturating_add(1);
        let mut flags: CLEAR_COMM_ERROR_FLAGS = 0;
        let mut status = COMSTAT::default();
        if unsafe { ClearCommError(self.raw_file(), &mut flags, &mut status) } == 0 {
            self.diagnostics.line_status_failures =
                self.diagnostics.line_status_failures.saturating_add(1);
            let code = unsafe { GetLastError() };
            return Err(TransportError {
                kind: win32_error_kind(code),
                native_code: Some(code),
            });
        }
        self.diagnostics.last_input_queue_bytes = Some(status.cbInQue);
        self.diagnostics.last_output_queue_bytes = Some(status.cbOutQue);
        if flags != 0 {
            self.diagnostics.line_error_observations =
                self.diagnostics.line_error_observations.saturating_add(1);
            self.diagnostics.line_error_flags |= flags;
            return Err(TransportError {
                kind: ProbeErrorKind::SerialLineError,
                native_code: None,
            });
        }
        Ok(())
    }
}

impl Transport for WindowsSerialTransport {
    fn now_ms(&mut self) -> u64 {
        self.elapsed()
    }

    fn progress_origin_ms(&self) -> Option<u64> {
        Some(0)
    }

    fn initial_phases(&self) -> &[PhaseProgress] {
        &self.phases
    }

    fn port_opened_at_ms(&self) -> Option<u32> {
        self.opened_at_ms
    }

    fn port_configured_at_ms(&self) -> Option<u32> {
        self.configured_at_ms
    }

    fn serial_diagnostics(&self) -> Option<SerialDiagnostics> {
        Some(self.diagnostics.clone())
    }

    fn serial_settings(&self) -> Option<SerialSettings> {
        Some(self.settings.clone())
    }

    fn write(
        &mut self,
        bytes: &[u8],
        deadline_ms: u64,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<usize, TransportError> {
        self.diagnostics.write_calls = self.diagnostics.write_calls.saturating_add(1);
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            self.diagnostics.write_failures = self.diagnostics.write_failures.saturating_add(1);
            return Err(TransportError {
                kind: ProbeErrorKind::Cancelled,
                native_code: None,
            });
        }
        if self.elapsed() >= deadline_ms {
            self.diagnostics.write_failures = self.diagnostics.write_failures.saturating_add(1);
            return Err(TransportError {
                kind: ProbeErrorKind::Timeout,
                native_code: None,
            });
        }
        let mut operation = Operation::new(bytes.to_vec()).map_err(|code| {
            self.diagnostics.write_failures = self.diagnostics.write_failures.saturating_add(1);
            TransportError {
                kind: win32_error_kind(code),
                native_code: Some(code),
            }
        })?;
        let started = unsafe {
            WriteFile(
                self.raw_file(),
                operation.buffer.as_ptr(),
                operation.buffer.len() as u32,
                ptr::null_mut(),
                &mut operation.overlapped,
            )
        };
        if started == 0 {
            let code = unsafe { GetLastError() };
            if code != ERROR_IO_PENDING {
                self.diagnostics.write_failures = self.diagnostics.write_failures.saturating_add(1);
                return Err(TransportError {
                    kind: win32_error_kind(code),
                    native_code: Some(code),
                });
            }
        }
        let (_, count, interrupted) = match self.run_operation(operation, deadline_ms, cancelled) {
            Ok(completion) => completion,
            Err(error) => {
                self.diagnostics.write_failures = self.diagnostics.write_failures.saturating_add(1);
                return Err(error);
            }
        };
        self.diagnostics.completed_write_bytes = self
            .diagnostics
            .completed_write_bytes
            .saturating_add(count as u32);
        if let Some(kind) = interrupted {
            self.diagnostics.write_failures = self.diagnostics.write_failures.saturating_add(1);
            return Err(TransportError {
                kind,
                native_code: None,
            });
        }
        Ok(count)
    }

    fn read(
        &mut self,
        bytes: &mut [u8],
        deadline_ms: u64,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<ReadResult, TransportError> {
        self.diagnostics.read_calls = self.diagnostics.read_calls.saturating_add(1);
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            self.diagnostics.read_failures = self.diagnostics.read_failures.saturating_add(1);
            return Err(TransportError {
                kind: ProbeErrorKind::Cancelled,
                native_code: None,
            });
        }
        if self.elapsed() >= deadline_ms {
            self.diagnostics.read_failures = self.diagnostics.read_failures.saturating_add(1);
            return Err(TransportError {
                kind: ProbeErrorKind::Timeout,
                native_code: None,
            });
        }
        let capacity = bytes.len().min(512);
        let mut operation = Operation::new(vec![0; capacity]).map_err(|code| {
            self.diagnostics.read_failures = self.diagnostics.read_failures.saturating_add(1);
            TransportError {
                kind: win32_error_kind(code),
                native_code: Some(code),
            }
        })?;
        let started = unsafe {
            ReadFile(
                self.raw_file(),
                operation.buffer.as_mut_ptr(),
                capacity as u32,
                ptr::null_mut(),
                &mut operation.overlapped,
            )
        };
        if started == 0 {
            let code = unsafe { GetLastError() };
            if code != ERROR_IO_PENDING {
                self.diagnostics.read_failures = self.diagnostics.read_failures.saturating_add(1);
                return Err(TransportError {
                    kind: win32_error_kind(code),
                    native_code: Some(code),
                });
            }
        }
        let (buffer, count, interrupted) =
            match self.run_operation(operation, deadline_ms, cancelled) {
                Ok(completion) => completion,
                Err(error) => {
                    self.diagnostics.read_failures =
                        self.diagnostics.read_failures.saturating_add(1);
                    return Err(error);
                }
            };
        self.diagnostics.completed_read_bytes = self
            .diagnostics
            .completed_read_bytes
            .saturating_add(count as u32);
        if let Err(error) = self.line_status() {
            self.diagnostics.read_failures = self.diagnostics.read_failures.saturating_add(1);
            return Err(error);
        }
        if let Some(kind) = interrupted {
            self.diagnostics.read_failures = self.diagnostics.read_failures.saturating_add(1);
            return Err(TransportError {
                kind,
                native_code: None,
            });
        }
        if count == 0 {
            self.diagnostics.idle_reads = self.diagnostics.idle_reads.saturating_add(1);
            Ok(ReadResult::Idle)
        } else {
            if count > bytes.len() {
                self.diagnostics.read_failures = self.diagnostics.read_failures.saturating_add(1);
                return Err(TransportError {
                    kind: ProbeErrorKind::TransportContract,
                    native_code: None,
                });
            }
            bytes[..count].copy_from_slice(&buffer[..count]);
            Ok(ReadResult::Data(count))
        }
    }
}

impl Drop for WindowsSerialTransport {
    fn drop(&mut self) {
        if self.poisoned {
            if let Some(file) = self.file.take() {
                mem::forget(file);
            }
            if let Some(lease) = self.lease.take() {
                mem::forget(lease);
            }
        }
    }
}

pub(super) fn run_native(
    mut options: Options,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<ProbeReport, ProbeFailure> {
    use std::sync::atomic::Ordering;

    if let Err(kind) = options.validate() {
        return Err(native_failure(kind, Stage::Options, None));
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(native_failure(ProbeErrorKind::Cancelled, Stage::Open, None));
    }
    let wrapper_started = Instant::now();
    let mut transport = WindowsSerialTransport::open(cancelled)?;
    let elapsed = transport.now_ms();
    let remaining = u64::from(options.total_timeout_ms).saturating_sub(elapsed);
    if remaining < 1_000 || u64::from(options.capture_ms) > remaining {
        return Err(ProbeFailure {
            kind: ProbeErrorKind::Timeout,
            stage: Stage::Configure,
            native_code: None,
            progress: Box::new(transport.trace.progress(
                transport.opened_at_ms,
                transport.configured_at_ms,
                Some(transport.diagnostics.clone()),
            )),
        });
    }
    options.total_timeout_ms = remaining as u32;
    options.io_timeout_ms = options.io_timeout_ms.min(options.total_timeout_ms);
    let mut report = run_with_evidence(
        options,
        transport,
        cancelled,
        Evidence::NativeTransportObservation,
    )?;
    report.elapsed_ms = wrapper_started
        .elapsed()
        .as_millis()
        .min(u128::from(u32::MAX)) as u32;
    Ok(report)
}

fn native_failure(kind: ProbeErrorKind, stage: Stage, native_code: Option<u32>) -> ProbeFailure {
    let progress = Progress::new(Evidence::NativeTransportObservation);
    ProbeFailure {
        kind,
        stage,
        native_code,
        progress: Box::new(progress.record),
    }
}

fn discover_port() -> Result<String, ProbeErrorKind> {
    let observation = crate::native::windows_observation::observe_serial();
    let selection = super::selection_error(observation)?;
    super::select_ct_port(&selection)
}

fn acquire_mutex() -> Result<MutexLease, (ProbeErrorKind, Option<u32>)> {
    let sid = current_user_sid().map_err(|code| (win32_error_kind(code), Some(code)))?;
    let name: Vec<u16> = format!(r"Local\copilot-controller-ct-serial-{sid}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        let code = unsafe { GetLastError() };
        return Err((win32_error_kind(code), Some(code)));
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let raw = unsafe { CreateMutexW(&attributes, 0, name.as_ptr()) };
    let create_error = raw.is_null().then(|| unsafe { GetLastError() });
    unsafe {
        LocalFree(descriptor.cast());
    }
    let handle = match create_error {
        Some(code) => return Err((win32_error_kind(code), Some(code))),
        None => WinHandle::new(raw).map_err(|code| (win32_error_kind(code), Some(code)))?,
    };
    match unsafe { WaitForSingleObject(handle.raw(), 0) } {
        WAIT_OBJECT_0 => Ok(MutexLease {
            handle,
            owned: true,
        }),
        WAIT_ABANDONED => {
            unsafe {
                ReleaseMutex(handle.raw());
            }
            Err((ProbeErrorKind::BusyOrDenied, None))
        }
        WAIT_TIMEOUT => Err((ProbeErrorKind::BusyOrDenied, None)),
        WAIT_FAILED => {
            let code = unsafe { GetLastError() };
            Err((win32_error_kind(code), Some(code)))
        }
        _ => Err((ProbeErrorKind::NativeFailure, None)),
    }
}

fn current_user_sid() -> Result<String, u32> {
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(unsafe { GetLastError() });
    }
    let token = WinHandle::new(token)?;
    let mut required = 0_u32;
    unsafe {
        GetTokenInformation(token.raw(), TokenUser, ptr::null_mut(), 0, &mut required);
    }
    let code = unsafe { GetLastError() };
    if code != ERROR_INSUFFICIENT_BUFFER || required == 0 || required > 4096 {
        return Err(code);
    }
    let words = (required as usize).div_ceil(mem::size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    if unsafe {
        GetTokenInformation(
            token.raw(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(unsafe { GetLastError() });
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut sid_string = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_string) } == 0 {
        return Err(unsafe { GetLastError() });
    }
    let result = unsafe {
        let mut length = 0;
        while length < 256 && *sid_string.add(length) != 0 {
            length += 1;
        }
        if length == 256 {
            Err(87)
        } else {
            String::from_utf16(std::slice::from_raw_parts(sid_string, length)).map_err(|_| 87)
        }
    };
    unsafe {
        LocalFree(sid_string.cast());
    }
    result
}
