use super::{ErrorKind, MonotonicStamp, PlatformError, PlatformResult};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockSource {
    WindowsQpc,
    LinuxMonotonic,
    MacosUptimeRaw,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ClockSample {
    pub source: ClockSource,
    pub stamp: MonotonicStamp,
}

pub fn stamp_from_timespec(
    epoch: Uuid,
    seconds: i64,
    nanos: i64,
) -> PlatformResult<MonotonicStamp> {
    if epoch.is_nil() || seconds < 0 || !(0..1_000_000_000).contains(&nanos) {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "monotonic_clock",
        ));
    }
    let ticks = (seconds as u64)
        .checked_mul(1_000_000_000)
        .and_then(|ticks| ticks.checked_add(nanos as u64))
        .ok_or_else(|| PlatformError::new(ErrorKind::InvalidData, "monotonic_clock"))?;
    Ok(MonotonicStamp {
        epoch,
        ticks,
        frequency: 1_000_000_000,
    })
}

pub fn observe_clock(epoch: Uuid) -> PlatformResult<ClockSample> {
    if epoch.is_nil() {
        return Err(PlatformError::new(ErrorKind::InvalidData, "clock_epoch"));
    }
    platform_clock(epoch)
}

#[cfg(windows)]
fn platform_clock(epoch: Uuid) -> PlatformResult<ClockSample> {
    use windows_sys::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };
    let mut counter = 0i64;
    let mut frequency = 0i64;
    // Both calls write only their scalar output and do not acquire a device.
    let ok = unsafe {
        QueryPerformanceFrequency(&mut frequency) != 0 && QueryPerformanceCounter(&mut counter) != 0
    };
    if !ok {
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        return Err(PlatformError::with_code(
            ErrorKind::NativeFailure,
            "monotonic_clock",
            code,
        ));
    }
    if frequency <= 0 || counter < 0 {
        return Err(PlatformError::new(
            ErrorKind::InvalidData,
            "monotonic_clock",
        ));
    }
    Ok(ClockSample {
        source: ClockSource::WindowsQpc,
        stamp: MonotonicStamp {
            epoch,
            ticks: counter as u64,
            frequency: frequency as u64,
        },
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn platform_clock(epoch: Uuid) -> PlatformResult<ClockSample> {
    #[cfg(target_os = "linux")]
    let (clock, source) = (libc::CLOCK_MONOTONIC, ClockSource::LinuxMonotonic);
    #[cfg(target_os = "macos")]
    let (clock, source) = (libc::CLOCK_UPTIME_RAW, ClockSource::MacosUptimeRaw);
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(clock, &mut value) } != 0 {
        let code = std::io::Error::last_os_error()
            .raw_os_error()
            .and_then(|code| u32::try_from(code).ok());
        return Err(PlatformError {
            kind: ErrorKind::NativeFailure,
            operation: "monotonic_clock",
            native_code: code,
        });
    }
    let stamp = stamp_from_timespec(epoch, value.tv_sec, value.tv_nsec)?;
    Ok(ClockSample { source, stamp })
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn platform_clock(_epoch: Uuid) -> PlatformResult<ClockSample> {
    Err(PlatformError::new(
        ErrorKind::UnsupportedPlatform,
        "monotonic_clock",
    ))
}
