use super::super::{
    DesktopBackend, DesktopContext, DesktopSnapshot, ErrorKind, MonotonicStamp, Observation,
    PlatformError, PlatformResult, collect_desktop, observe_clock, process::object_name_matches,
};
use super::{last_error, native_error};
use std::{ffi::c_void, ptr};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError, HANDLE},
    System::{
        RemoteDesktop::{
            ProcessIdToSessionId, WTS_INFO_CLASS, WTSClientProtocolType, WTSConnectState,
            WTSFreeMemory, WTSGetActiveConsoleSessionId, WTSQuerySessionInformationW,
        },
        StationsAndDesktops::{
            CloseDesktop, DESKTOP_READOBJECTS, GetProcessWindowStation, GetThreadDesktop,
            GetUserObjectInformationW, HDESK, OpenInputDesktop, UOI_NAME,
        },
        Threading::{GetCurrentProcessId, GetCurrentThreadId},
    },
};

struct WtsBuffer(*mut u16);
impl Drop for WtsBuffer {
    fn drop(&mut self) {
        unsafe { WTSFreeMemory(self.0.cast()) };
    }
}

struct InputDesktop(HDESK);
impl Drop for InputDesktop {
    fn drop(&mut self) {
        unsafe { CloseDesktop(self.0) };
    }
}

fn query_wts(session: u32, class: WTS_INFO_CLASS, expected: usize) -> PlatformResult<Vec<u8>> {
    let mut buffer = ptr::null_mut();
    let mut length = 0u32;
    if unsafe {
        WTSQuerySessionInformationW(ptr::null_mut(), session, class, &mut buffer, &mut length)
    } == 0
    {
        return Err(last_error("WTSQuerySessionInformationW"));
    }
    if buffer.is_null() {
        return Err(PlatformError::new(ErrorKind::InvalidData, "wts_scalar"));
    }
    let allocation = WtsBuffer(buffer);
    if length as usize != expected {
        return Err(PlatformError::new(ErrorKind::InvalidData, "wts_scalar"));
    }
    let bytes = unsafe { std::slice::from_raw_parts(allocation.0.cast::<u8>(), expected) };
    Ok(bytes.to_vec())
}

fn object_is(handle: HANDLE, expected: &str) -> PlatformResult<bool> {
    if handle.is_null() {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "desktop_object_handle",
        ));
    }
    let mut units = [0u16; 256];
    let mut needed = 0u32;
    let success = unsafe {
        GetUserObjectInformationW(
            handle,
            UOI_NAME,
            units.as_mut_ptr().cast::<c_void>(),
            512,
            &mut needed,
        )
    };
    if success == 0 {
        let code = unsafe { GetLastError() };
        if code == ERROR_INSUFFICIENT_BUFFER && needed > 512 {
            return Err(PlatformError::with_code(
                ErrorKind::LimitExceeded,
                "desktop_object_name",
                code,
            ));
        }
        return Err(native_error("GetUserObjectInformationW", code));
    }
    object_name_matches(&units, needed as usize, expected)
}

pub(super) struct Desktop {
    pub(super) epoch: Uuid,
}

impl DesktopBackend for Desktop {
    fn stamp(&mut self) -> PlatformResult<MonotonicStamp> {
        Ok(observe_clock(self.epoch)?.stamp)
    }

    fn process_session(&mut self) -> PlatformResult<u32> {
        let mut session = 0;
        if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } == 0 {
            return Err(last_error("ProcessIdToSessionId"));
        }
        Ok(session)
    }

    fn snapshot(&mut self, session: u32) -> PlatformResult<DesktopSnapshot> {
        let console = unsafe { WTSGetActiveConsoleSessionId() };
        let active_console_session_id = (console != u32::MAX).then_some(console);
        let state = query_wts(session, WTSConnectState, 4)?;
        let protocol = query_wts(session, WTSClientProtocolType, 2)?;
        let station = unsafe { GetProcessWindowStation() };
        if station.is_null() {
            return Err(last_error("GetProcessWindowStation"));
        }
        let thread_desktop = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
        if thread_desktop.is_null() {
            return Err(last_error("GetThreadDesktop"));
        }
        let input = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
        if input.is_null() {
            return Err(last_error("OpenInputDesktop"));
        }
        let input = InputDesktop(input);
        Ok(DesktopSnapshot {
            active_console_session_id,
            connection_state: i32::from_ne_bytes(state.try_into().expect("checked length")),
            client_protocol: u16::from_ne_bytes(protocol.try_into().expect("checked length")),
            interactive_station: object_is(station.cast(), "WinSta0")?,
            thread_default_desktop: object_is(thread_desktop.cast(), "Default")?,
            input_default_desktop: object_is(input.0.cast(), "Default")?,
        })
    }
}

pub fn observe_desktop(epoch: Uuid) -> Observation<DesktopContext> {
    collect_desktop(&mut Desktop { epoch }).into()
}
