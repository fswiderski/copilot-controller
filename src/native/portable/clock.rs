use std::{fmt, time::Duration};

use serde::Serialize;

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use crate::domain::ClockSource;
pub use crate::domain::RefusalReason as EvidenceAgeError;
use crate::domain::{ClockSample, MonotonicStamp, RuntimeEpoch};

use super::contracts::{PlatformError, PlatformErrorKind};

const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Convert an OS timespec without normalizing invalid or overflowing values.
pub fn timespec_to_ticks(seconds: i64, nanoseconds: i64) -> Result<u64, PlatformError> {
    let invalid = || PlatformError::new(PlatformErrorKind::InvalidData, "monotonic_clock", None);
    let seconds = u64::try_from(seconds).map_err(|_| invalid())?;
    let nanoseconds = u64::try_from(nanoseconds).map_err(|_| invalid())?;
    if nanoseconds >= NANOS_PER_SECOND {
        return Err(invalid());
    }
    seconds
        .checked_mul(NANOS_PER_SECOND)
        .and_then(|ticks| ticks.checked_add(nanoseconds))
        .ok_or_else(invalid)
}

/// Sample the system-shared native counter. The caller must retain the epoch
/// and ensure comparisons are within the same boot.
pub fn sample_monotonic(epoch: RuntimeEpoch) -> Result<ClockSample, PlatformError> {
    if epoch.is_nil() {
        return Err(PlatformError::new(
            PlatformErrorKind::InvalidData,
            "clock_epoch",
            None,
        ));
    }
    native_sample(epoch)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn native_sample(epoch: RuntimeEpoch) -> Result<ClockSample, PlatformError> {
    #[cfg(target_os = "linux")]
    let (clock, source) = (libc::CLOCK_MONOTONIC, ClockSource::LinuxMonotonic);
    #[cfg(target_os = "macos")]
    let (clock, source) = (libc::CLOCK_UPTIME_RAW, ClockSource::MacosUptimeRaw);

    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // clock_gettime initializes the timespec only on success.
    if unsafe { libc::clock_gettime(clock, time.as_mut_ptr()) } != 0 {
        return Err(PlatformError::last_native_error("monotonic_clock"));
    }
    // The successful call above initialized the entire structure.
    let time = unsafe { time.assume_init() };
    #[allow(clippy::useless_conversion)]
    let seconds = i64::from(time.tv_sec);
    #[allow(clippy::useless_conversion)]
    let nanoseconds = i64::from(time.tv_nsec);
    let ticks = timespec_to_ticks(seconds, nanoseconds)?;
    Ok(ClockSample {
        source,
        stamp: MonotonicStamp {
            epoch,
            ticks,
            frequency: NANOS_PER_SECOND,
        },
    })
}

#[cfg(windows)]
fn native_sample(epoch: RuntimeEpoch) -> Result<ClockSample, PlatformError> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn QueryPerformanceCounter(value: *mut i64) -> i32;
        fn QueryPerformanceFrequency(value: *mut i64) -> i32;
    }

    let mut frequency = 0_i64;
    let mut counter = 0_i64;
    // Both pointers refer to writable i64 values for the duration of the calls.
    if unsafe { QueryPerformanceFrequency(&mut frequency) } == 0
        || unsafe { QueryPerformanceCounter(&mut counter) } == 0
    {
        return Err(PlatformError::last_native_error("monotonic_clock"));
    }
    if frequency <= 0 || counter < 0 {
        return Err(PlatformError::new(
            PlatformErrorKind::InvalidData,
            "monotonic_clock",
            None,
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

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn native_sample(_epoch: RuntimeEpoch) -> Result<ClockSample, PlatformError> {
    Err(PlatformError::new(
        PlatformErrorKind::UnsupportedPlatform,
        "monotonic_clock",
        None,
    ))
}

/// Compare untagged evidence using the foundation's exact rational deadline.
/// Returns only ClockMismatch or StaleState. Callers comparing native samples
/// must separately verify their source.
pub fn check_evidence_age(
    evidence: MonotonicStamp,
    now: MonotonicStamp,
    maximum_age: Duration,
) -> Result<(), EvidenceAgeError> {
    now.elapsed_since(evidence)
        .map_err(|_| EvidenceAgeError::ClockMismatch)?;
    if now.ticks - evidence.ticks <= 1 {
        return Err(EvidenceAgeError::ClockMismatch);
    }
    match now.elapsed_within(evidence, maximum_age) {
        Ok(true) => Ok(()),
        Ok(false) => Err(EvidenceAgeError::StaleState),
        Err(_) => Err(EvidenceAgeError::ClockMismatch),
    }
}

/// Source-aware counterpart for native samples.
pub fn check_sample_age(
    evidence: ClockSample,
    now: ClockSample,
    maximum_age: Duration,
) -> Result<(), EvidenceAgeError> {
    now.elapsed_since(evidence)
        .map_err(|_| EvidenceAgeError::ClockMismatch)?;
    check_evidence_age(evidence.stamp, now.stamp, maximum_age)
}

/// Validate the complete, non-restarted diagnostic interval.
pub fn check_diagnostic_interval(
    requested: MonotonicStamp,
    started: MonotonicStamp,
    finished: MonotonicStamp,
    received: MonotonicStamp,
) -> Result<(), EvidenceAgeError> {
    let budget = Duration::from_secs(12);
    check_evidence_age(requested, started, budget)?;
    finished
        .elapsed_since(started)
        .map_err(|_| EvidenceAgeError::ClockMismatch)?;
    check_evidence_age(finished, received, budget)?;
    check_evidence_age(requested, received, budget)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleCut {
    Lock,
    Sleep,
    Wake,
    SessionChanged,
    SourceLost,
    ClockDiscontinuity,
    RuntimeReplaced,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleError {
    InvalidMaximumGap,
    ClockDiscontinuity,
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for LifecycleError {}

/// Explicit cuts invalidate observations; this does not subscribe to OS events.
#[derive(Debug)]
pub struct Lifecycle {
    maximum_gap: Duration,
    epoch: RuntimeEpoch,
    last_sample: Option<ClockSample>,
    last_cut: Option<LifecycleCut>,
}

impl Lifecycle {
    pub fn new(maximum_gap: Duration) -> Result<Self, LifecycleError> {
        if maximum_gap.is_zero() {
            return Err(LifecycleError::InvalidMaximumGap);
        }
        Ok(Self {
            maximum_gap,
            epoch: RuntimeEpoch::fresh(),
            last_sample: None,
            last_cut: None,
        })
    }

    pub fn epoch(&self) -> RuntimeEpoch {
        self.epoch
    }

    pub fn last_sample(&self) -> Option<ClockSample> {
        self.last_sample
    }

    pub fn last_cut(&self) -> Option<LifecycleCut> {
        self.last_cut
    }

    /// The coordinator must discard all observations from the previous epoch.
    pub fn invalidate(&mut self, reason: LifecycleCut) -> RuntimeEpoch {
        let previous = self.epoch;
        loop {
            let next = RuntimeEpoch::fresh();
            if !next.is_nil() && next != previous {
                self.epoch = next;
                break;
            }
        }
        self.last_sample = None;
        self.last_cut = Some(reason);
        self.epoch
    }

    pub fn observe(&mut self, sample: ClockSample) -> Result<(), LifecycleError> {
        let valid_first = sample.stamp.epoch == self.epoch && sample.stamp.frequency > 0;
        let valid_gap = self.last_sample.is_none_or(|last| {
            sample
                .elapsed_within(last, self.maximum_gap)
                .unwrap_or(false)
        });
        if !valid_first || !valid_gap {
            self.invalidate(LifecycleCut::ClockDiscontinuity);
            return Err(LifecycleError::ClockDiscontinuity);
        }
        self.last_sample = Some(sample);
        Ok(())
    }
}
