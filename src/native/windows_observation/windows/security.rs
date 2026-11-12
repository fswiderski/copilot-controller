use super::super::{
    ErrorKind, PlatformError, PlatformResult, SecurityEvidence, TokenContext,
    qualification::{TokenIds, TokenSnapshot, bounded_sid_bytes, stable_security},
};
use super::{Handle, last_error, native_error, process::HeldProcess};
use std::{ffi::c_void, mem, ptr};
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError, HANDLE, LUID},
    Security::{
        GetTokenInformation, IsValidSid, TOKEN_ELEVATION, TOKEN_INFORMATION_CLASS,
        TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_STATISTICS, TOKEN_USER, TokenElevation,
        TokenIntegrityLevel, TokenIsAppContainer, TokenSessionId, TokenStatistics, TokenUIAccess,
        TokenUser,
    },
    System::{RemoteDesktop::ProcessIdToSessionId, Threading::OpenProcessToken},
};

struct TokenBuffer {
    words: Vec<usize>,
    length: usize,
}

impl TokenBuffer {
    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.length) }
    }

    fn header<T>(&self) -> PlatformResult<&T> {
        if self.length < mem::size_of::<T>()
            || !(self.words.as_ptr() as usize).is_multiple_of(mem::align_of::<T>())
        {
            return Err(PlatformError::new(
                ErrorKind::InvalidData,
                "token_information",
            ));
        }
        Ok(unsafe { &*self.words.as_ptr().cast::<T>() })
    }

    fn sid(&self, sid: *mut c_void) -> PlatformResult<Vec<u8>> {
        let base = self.words.as_ptr() as usize;
        let offset = (sid as usize)
            .checked_sub(base)
            .ok_or_else(|| PlatformError::new(ErrorKind::InvalidData, "token_sid"))?;
        let bytes = bounded_sid_bytes(self.bytes(), offset)?;
        if unsafe { IsValidSid(sid) } == 0 {
            return Err(PlatformError::new(ErrorKind::InvalidData, "token_sid"));
        }
        Ok(bytes.to_vec())
    }
}

fn fixed<T: Default>(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> PlatformResult<T> {
    let mut value = T::default();
    let mut returned = 0u32;
    let size = mem::size_of::<T>();
    if size == 0 || size > 16_384 {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "token_information",
        ));
    }
    if unsafe {
        GetTokenInformation(
            token,
            class,
            (&mut value as *mut T).cast(),
            size as u32,
            &mut returned,
        )
    } == 0
    {
        return Err(last_error("GetTokenInformation"));
    }
    if returned as usize != size {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "token_information",
        ));
    }
    Ok(value)
}

fn variable(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> PlatformResult<TokenBuffer> {
    let mut required = 0u32;
    let success = unsafe { GetTokenInformation(token, class, ptr::null_mut(), 0, &mut required) };
    if success != 0 {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "token_information_size",
        ));
    }
    let code = unsafe { GetLastError() };
    if code != ERROR_INSUFFICIENT_BUFFER {
        return Err(native_error("GetTokenInformation", code));
    }
    if required == 0 {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "token_information_size",
        ));
    }
    if required > 16_384 {
        return Err(PlatformError::new(
            ErrorKind::LimitExceeded,
            "token_information_size",
        ));
    }
    let words_len = (required as usize).div_ceil(mem::size_of::<usize>());
    let mut words = vec![0usize; words_len];
    let mut returned = 0;
    if unsafe {
        GetTokenInformation(
            token,
            class,
            words.as_mut_ptr().cast(),
            required,
            &mut returned,
        )
    } == 0
    {
        let code = unsafe { GetLastError() };
        if code == ERROR_INSUFFICIENT_BUFFER {
            return Err(PlatformError::with_code(
                ErrorKind::LimitExceeded,
                "token_information_size",
                code,
            ));
        }
        return Err(native_error("GetTokenInformation", code));
    }
    if returned == 0 || returned > required {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "token_information_size",
        ));
    }
    Ok(TokenBuffer {
        words,
        length: returned as usize,
    })
}

fn luid(value: LUID) -> (u32, i32) {
    (value.LowPart, value.HighPart)
}

fn ids(statistics: TOKEN_STATISTICS) -> TokenIds {
    TokenIds {
        token: luid(statistics.TokenId),
        modified: luid(statistics.ModifiedId),
        authentication: luid(statistics.AuthenticationId),
    }
}

fn boolean(value: u32) -> PlatformResult<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(PlatformError::new(ErrorKind::InvalidData, "token_boolean")),
    }
}

fn snapshot(process: &HeldProcess) -> PlatformResult<TokenSnapshot> {
    super::process::check_running(process)?;
    let mut raw = ptr::null_mut();
    if unsafe { OpenProcessToken(process.handle.0, TOKEN_QUERY, &mut raw) } == 0 {
        return Err(last_error("OpenProcessToken"));
    }
    if raw.is_null() {
        return Err(PlatformError::new(ErrorKind::InvalidData, "token_handle"));
    }
    let token = Handle(raw);
    let before = ids(fixed::<TOKEN_STATISTICS>(token.0, TokenStatistics)?);
    let user = variable(token.0, TokenUser)?;
    let sid = user.sid(user.header::<TOKEN_USER>()?.User.Sid)?;
    let label = variable(token.0, TokenIntegrityLevel)?;
    let integrity = label.sid(label.header::<TOKEN_MANDATORY_LABEL>()?.Label.Sid)?;
    if integrity.len() != 12 || integrity[1] != 1 || integrity[2..8] != [0, 0, 0, 0, 0, 16] {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "token_integrity_sid",
        ));
    }
    let integrity_rid = u32::from_le_bytes(integrity[8..12].try_into().expect("checked length"));
    let session_id = fixed::<u32>(token.0, TokenSessionId)?;
    let elevation = fixed::<TOKEN_ELEVATION>(token.0, TokenElevation)?;
    let ui_access = fixed::<u32>(token.0, TokenUIAccess)?;
    let app_container = fixed::<u32>(token.0, TokenIsAppContainer)?;
    let after = ids(fixed::<TOKEN_STATISTICS>(token.0, TokenStatistics)?);
    if before != after {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "token_statistics",
        ));
    }
    let mut process_session = 0;
    if unsafe { ProcessIdToSessionId(process.pid, &mut process_session) } == 0 {
        return Err(last_error("ProcessIdToSessionId"));
    }
    if session_id != process_session {
        return Err(PlatformError::new(
            ErrorKind::ObservationChanged,
            "token_process_session",
        ));
    }
    super::process::check_running(process)?;
    Ok(TokenSnapshot {
        ids: before,
        sid,
        context: TokenContext {
            session_id,
            integrity_rid,
            elevated: boolean(elevation.TokenIsElevated)?,
            ui_access: boolean(ui_access)?,
            app_container: boolean(app_container)?,
        },
    })
}

pub(super) fn observe_security(
    observer: &HeldProcess,
    target: &HeldProcess,
) -> PlatformResult<SecurityEvidence> {
    let observer_first = snapshot(observer)?;
    let target_first = snapshot(target)?;
    let observer_second = snapshot(observer)?;
    let target_second = snapshot(target)?;
    stable_security(observer_first, target_first, observer_second, target_second)
}
