use super::super::{
    ErrorKind, ExecutableNames, ForegroundBackend, ForegroundEvidence, MonotonicStamp, Observation,
    PlatformError, PlatformResult, ProcessEvidence, ProcessIdentity, ProcessInventory,
    ProcessInventoryBackend, collect_foreground, collect_process_inventory, observe_clock,
};
use super::{Handle, last_error, native_error};
use std::mem;
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::{
        ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, FILETIME,
        GetLastError, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        RemoteDesktop::ProcessIdToSessionId,
        Threading::{
            GetCurrentProcessId, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
        },
    },
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

pub(super) struct HeldProcess {
    pub(super) handle: Handle,
    pub(super) pid: u32,
    stamp: MonotonicStamp,
}

pub(super) fn hold_process(pid: u32, epoch: Uuid) -> PlatformResult<HeldProcess> {
    if pid == 0 {
        return Err(PlatformError::new(ErrorKind::InvalidData, "process_id"));
    }
    let stamp = observe_clock(epoch)?.stamp;
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if raw.is_null() {
        let code = unsafe { GetLastError() };
        if code == ERROR_INVALID_PARAMETER {
            return Err(PlatformError::with_code(
                ErrorKind::ProcessExited,
                "OpenProcess",
                code,
            ));
        }
        return Err(native_error("OpenProcess", code));
    }
    let held = HeldProcess {
        handle: Handle(raw),
        pid,
        stamp,
    };
    check_running(&held)?;
    Ok(held)
}

pub(super) fn check_running(process: &HeldProcess) -> PlatformResult<()> {
    match unsafe { WaitForSingleObject(process.handle.0, 0) } {
        WAIT_TIMEOUT => Ok(()),
        WAIT_OBJECT_0 => Err(PlatformError::new(
            ErrorKind::ProcessExited,
            "process_lifetime",
        )),
        WAIT_FAILED => Err(PlatformError::with_code(
            ErrorKind::NativeFailure,
            "WaitForSingleObject",
            unsafe { GetLastError() },
        )),
        _ => Err(PlatformError::new(
            ErrorKind::InvalidData,
            "process_wait_result",
        )),
    }
}

pub(super) fn image_path(process: &HeldProcess) -> PlatformResult<Vec<u16>> {
    let mut buffer = vec![0u16; 32_768];
    let mut length = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process.handle.0, 0, buffer.as_mut_ptr(), &mut length) }
        == 0
    {
        let code = unsafe { GetLastError() };
        if code == ERROR_INSUFFICIENT_BUFFER {
            return Err(PlatformError::with_code(
                ErrorKind::LimitExceeded,
                "process_image_name",
                code,
            ));
        }
        return Err(native_error("QueryFullProcessImageNameW", code));
    }
    let length = length as usize;
    if length == 0 || length >= buffer.len() {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "process_image_name",
        ));
    }
    buffer.truncate(length);
    if buffer.contains(&0) {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "process_image_name",
        ));
    }
    Ok(buffer)
}

pub(super) fn inspect_held(
    process: &HeldProcess,
    names: ExecutableNames<'_>,
) -> PlatformResult<ProcessEvidence> {
    check_running(process)?;
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if unsafe {
        GetProcessTimes(
            process.handle.0,
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return Err(last_error("GetProcessTimes"));
    }
    let creation_time_100ns =
        (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    if creation_time_100ns == 0 {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "process_creation_time",
        ));
    }
    let mut session_id = 0;
    if unsafe { ProcessIdToSessionId(process.pid, &mut session_id) } == 0 {
        return Err(last_error("ProcessIdToSessionId"));
    }
    let path = image_path(process)?;
    let start = path
        .iter()
        .rposition(|c| *c == b'\\' as u16 || *c == b'/' as u16)
        .map_or(0, |i| i + 1);
    let basename = String::from_utf16(&path[start..])
        .map_err(|_| PlatformError::new(ErrorKind::InvalidData, "process_image_basename"))?;
    if basename.is_empty() || basename.contains('\0') {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "process_image_basename",
        ));
    }
    check_running(process)?;
    Ok(ProcessEvidence {
        identity: ProcessIdentity {
            pid: process.pid,
            creation_time_100ns,
            session_id,
        },
        class: names.classify(&basename),
        stamp: process.stamp,
    })
}

pub fn observe_process(
    pid: u32,
    epoch: Uuid,
    names: ExecutableNames<'_>,
) -> Observation<ProcessEvidence> {
    (|| inspect_held(&hold_process(pid, epoch)?, names))().into()
}

pub fn observe_current_process(
    epoch: Uuid,
    names: ExecutableNames<'_>,
) -> Observation<ProcessEvidence> {
    observe_process(unsafe { GetCurrentProcessId() }, epoch, names)
}

struct Snapshot<'a> {
    handle: Handle,
    first: bool,
    examined: u32,
    epoch: Uuid,
    names: ExecutableNames<'a>,
}

impl ProcessInventoryBackend for Snapshot<'_> {
    fn next_process(&mut self) -> PlatformResult<Option<(u32, String)>> {
        let mut entry: PROCESSENTRY32W = unsafe { mem::zeroed() };
        entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as u32;
        let success = unsafe {
            if self.first {
                self.first = false;
                Process32FirstW(self.handle.0, &mut entry)
            } else {
                Process32NextW(self.handle.0, &mut entry)
            }
        };
        if success == 0 {
            let code = unsafe { GetLastError() };
            if code == ERROR_NO_MORE_FILES {
                return Ok(None);
            }
            return Err(native_error("process_snapshot_enumeration", code));
        }
        if self.examined == 16_384 {
            return Err(PlatformError::new(
                ErrorKind::LimitExceeded,
                "process_enumeration",
            ));
        }
        self.examined += 1;
        let end = entry
            .szExeFile
            .iter()
            .position(|unit| *unit == 0)
            .ok_or_else(|| PlatformError::new(ErrorKind::InvalidData, "process_snapshot_name"))?;
        let name = String::from_utf16(&entry.szExeFile[..end])
            .map_err(|_| PlatformError::new(ErrorKind::InvalidData, "process_snapshot_name"))?;
        Ok(Some((entry.th32ProcessID, name)))
    }

    fn inspect(&mut self, pid: u32) -> PlatformResult<ProcessEvidence> {
        inspect_held(&hold_process(pid, self.epoch)?, self.names)
    }
}

pub fn observe_process_inventory(
    epoch: Uuid,
    names: ExecutableNames<'_>,
) -> Observation<ProcessInventory> {
    (|| {
        if epoch.is_nil() {
            return Err(PlatformError::new(ErrorKind::InvalidData, "clock_epoch"));
        }
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if raw == INVALID_HANDLE_VALUE || raw.is_null() {
            return Err(last_error("CreateToolhelp32Snapshot"));
        }
        let mut snapshot = Snapshot {
            handle: Handle(raw),
            first: true,
            examined: 0,
            epoch,
            names,
        };
        collect_process_inventory(&mut snapshot, names)
    })()
    .into()
}

pub(super) struct Foreground<'a> {
    pub(super) epoch: Uuid,
    pub(super) names: ExecutableNames<'a>,
}

impl ForegroundBackend for Foreground<'_> {
    type Held = HeldProcess;

    fn foreground_window(&mut self) -> PlatformResult<Option<u64>> {
        let hwnd = unsafe { GetForegroundWindow() };
        Ok((!hwnd.is_null()).then_some(hwnd as usize as u64))
    }

    fn window_owner(&mut self, window: u64) -> PlatformResult<(u32, u32)> {
        let hwnd = usize::try_from(window)
            .map_err(|_| PlatformError::new(ErrorKind::InvalidData, "foreground_window"))?
            as *mut _;
        let mut pid = 0;
        let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        Ok((thread, pid))
    }

    fn hold_process(&mut self, pid: u32) -> PlatformResult<Self::Held> {
        hold_process(pid, self.epoch)
    }

    fn inspect_held(&mut self, held: &Self::Held) -> PlatformResult<ProcessEvidence> {
        inspect_held(held, self.names)
    }

    fn check_running(&mut self, held: &Self::Held) -> PlatformResult<()> {
        check_running(held)
    }
}

pub fn observe_foreground(
    epoch: Uuid,
    names: ExecutableNames<'_>,
) -> Observation<ForegroundEvidence> {
    if epoch.is_nil() {
        return Observation::Unavailable(PlatformError::new(ErrorKind::InvalidData, "clock_epoch"));
    }
    collect_foreground(&mut Foreground { epoch, names }).into()
}
