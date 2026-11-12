#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use std::process::Command;
use std::time::Duration;

use uuid::Uuid;

use crate::domain::{ClockSample, ClockSource, MonotonicStamp, RuntimeEpoch};

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use super::super::child;
use super::super::{
    clock::{
        EvidenceAgeError, Lifecycle, LifecycleCut, LifecycleError, check_diagnostic_interval,
        check_evidence_age, check_sample_age, sample_monotonic, timespec_to_ticks,
    },
    contracts::PlatformErrorKind,
};

fn epoch(value: u128) -> RuntimeEpoch {
    RuntimeEpoch::from_uuid(Uuid::from_u128(value))
}

fn stamp(ticks: u64, frequency: u64) -> MonotonicStamp {
    MonotonicStamp {
        epoch: epoch(1),
        ticks,
        frequency,
    }
}

fn sample(epoch: RuntimeEpoch, ticks: u64, frequency: u64, source: ClockSource) -> ClockSample {
    ClockSample {
        source,
        stamp: MonotonicStamp {
            epoch,
            ticks,
            frequency,
        },
    }
}

#[test]
fn shared_elapsed_and_exact_age_boundaries() {
    assert_eq!(
        stamp(25, 10).elapsed_since(stamp(10, 10)).unwrap(),
        Duration::new(1, 500_000_000)
    );
    assert_eq!(
        stamp(u64::MAX, 1).elapsed_since(stamp(0, 1)).unwrap(),
        Duration::from_secs(u64::MAX)
    );
    assert_eq!(
        stamp(u64::MAX - 1, u64::MAX)
            .elapsed_since(stamp(0, u64::MAX))
            .unwrap(),
        Duration::new(0, 999_999_999)
    );
    let start = stamp(100, 10_000_000);
    assert_eq!(
        check_evidence_age(start, stamp(200, 10_000_000), Duration::from_secs(3)),
        Ok(())
    );
    for now in [99, 100, 101] {
        assert_eq!(
            check_evidence_age(start, stamp(now, 10_000_000), Duration::from_secs(3)),
            Err(EvidenceAgeError::ClockMismatch)
        );
    }
    let mut invalid = stamp(200, 10_000_000);
    invalid.epoch = epoch(2);
    assert_eq!(
        check_evidence_age(start, invalid, Duration::from_secs(3)),
        Err(EvidenceAgeError::ClockMismatch)
    );
    for frequency in [0, 10_000_001] {
        assert_eq!(
            check_evidence_age(start, stamp(200, frequency), Duration::from_secs(3)),
            Err(EvidenceAgeError::ClockMismatch)
        );
    }
    let start = stamp(0, 2_000_000_000);
    assert_eq!(
        check_evidence_age(
            start,
            stamp(6_000_000_000, start.frequency),
            Duration::from_secs(3)
        ),
        Ok(())
    );
    assert_eq!(
        check_evidence_age(
            start,
            stamp(6_000_000_001, start.frequency),
            Duration::from_secs(3)
        ),
        Err(EvidenceAgeError::StaleState)
    );
    assert_eq!(
        check_evidence_age(
            stamp(0, u64::MAX),
            stamp(u64::MAX, u64::MAX),
            Duration::from_secs(1)
        ),
        Ok(())
    );
    assert_eq!(
        check_evidence_age(stamp(0, u64::MAX), stamp(u64::MAX, u64::MAX), Duration::MAX),
        Ok(())
    );
    let earlier = sample(epoch(1), 0, 10, ClockSource::LinuxMonotonic);
    let same_source = sample(epoch(1), 2, 10, ClockSource::LinuxMonotonic);
    let changed_source = sample(epoch(1), 2, 10, ClockSource::MacosUptimeRaw);
    assert_eq!(
        check_sample_age(earlier, same_source, Duration::from_secs(1)),
        Ok(())
    );
    assert_eq!(
        check_sample_age(earlier, changed_source, Duration::from_secs(1)),
        Err(EvidenceAgeError::ClockMismatch)
    );
}

#[test]
fn native_time_conversion_and_nil_epoch_fail_closed() {
    for (seconds, nanos) in [(-1, 0), (0, -1), (0, 1_000_000_000), (i64::MAX, 0)] {
        assert_eq!(
            timespec_to_ticks(seconds, nanos).unwrap_err().kind,
            PlatformErrorKind::InvalidData
        );
    }
    assert_eq!(timespec_to_ticks(1, 20).unwrap(), 1_000_000_020);
    assert_eq!(
        timespec_to_ticks(18_446_744_073, 709_551_615).unwrap(),
        u64::MAX
    );
    assert!(timespec_to_ticks(18_446_744_073, 709_551_616).is_err());
    let error = sample_monotonic(epoch(0)).unwrap_err();
    assert_eq!(error.kind, PlatformErrorKind::InvalidData);
    assert_eq!(error.operation, "clock_epoch");
}

#[test]
fn lifecycle_exact_gap_and_all_discontinuities_cut_epoch() {
    assert_eq!(
        Lifecycle::new(Duration::ZERO).unwrap_err(),
        LifecycleError::InvalidMaximumGap
    );
    let mut tracker = Lifecycle::new(Duration::from_secs(3)).unwrap();
    let initial = tracker.epoch();
    assert!(!initial.is_nil());
    tracker
        .observe(sample(initial, 10, 10, ClockSource::LinuxMonotonic))
        .unwrap();
    tracker
        .observe(sample(initial, 10, 10, ClockSource::LinuxMonotonic))
        .unwrap();
    tracker
        .observe(sample(initial, 40, 10, ClockSource::LinuxMonotonic))
        .unwrap();
    assert_eq!(tracker.epoch(), initial);
    assert_eq!(tracker.last_cut(), None);

    let mut tracker = Lifecycle::new(Duration::from_secs(3)).unwrap();
    let before = tracker.epoch();
    tracker
        .observe(sample(before, 10, 10, ClockSource::LinuxMonotonic))
        .unwrap();
    assert_eq!(
        tracker.observe(sample(before, 41, 10, ClockSource::LinuxMonotonic)),
        Err(LifecycleError::ClockDiscontinuity)
    );
    assert_ne!(tracker.epoch(), before);

    let mut tracker = Lifecycle::new(Duration::from_secs(3)).unwrap();
    let before = tracker.epoch();
    tracker
        .observe(sample(
            before,
            0,
            2_000_000_000,
            ClockSource::LinuxMonotonic,
        ))
        .unwrap();
    assert_eq!(
        tracker.observe(sample(
            before,
            6_000_000_001,
            2_000_000_000,
            ClockSource::LinuxMonotonic
        )),
        Err(LifecycleError::ClockDiscontinuity)
    );
    assert_ne!(tracker.epoch(), before);
    assert_eq!(tracker.last_sample(), None);
    assert_eq!(tracker.last_cut(), Some(LifecycleCut::ClockDiscontinuity));

    for variant in 0..5 {
        let mut tracker = Lifecycle::new(Duration::from_secs(3)).unwrap();
        let before = tracker.epoch();
        tracker
            .observe(sample(before, 10, 10, ClockSource::LinuxMonotonic))
            .unwrap();
        let invalid = match variant {
            0 => sample(before, 11, 10, ClockSource::MacosUptimeRaw),
            1 => sample(before, 9, 10, ClockSource::LinuxMonotonic),
            2 => sample(before, 11, 0, ClockSource::LinuxMonotonic),
            3 => sample(before, 11, 11, ClockSource::LinuxMonotonic),
            _ => sample(epoch(99), 11, 10, ClockSource::LinuxMonotonic),
        };
        assert_eq!(
            tracker.observe(invalid),
            Err(LifecycleError::ClockDiscontinuity)
        );
        assert_ne!(tracker.epoch(), before);
        assert_eq!(tracker.last_sample(), None);
    }
}

#[test]
fn explicit_cuts_persist_after_first_new_sample() {
    let mut tracker = Lifecycle::new(Duration::from_secs(3)).unwrap();
    for reason in [
        LifecycleCut::Lock,
        LifecycleCut::Sleep,
        LifecycleCut::Wake,
        LifecycleCut::SessionChanged,
        LifecycleCut::SourceLost,
        LifecycleCut::RuntimeReplaced,
    ] {
        let previous = tracker.epoch();
        let next = tracker.invalidate(reason);
        assert_ne!(previous, next);
        assert_eq!(tracker.last_sample(), None);
        tracker
            .observe(sample(next, 0, 10, ClockSource::MacosUptimeRaw))
            .unwrap();
        assert_eq!(tracker.last_cut(), Some(reason));
    }
}

#[test]
fn diagnostic_interval_has_one_overall_exact_budget() {
    let interval = |start, finish, received| {
        check_diagnostic_interval(
            stamp(100, 10),
            stamp(start, 10),
            stamp(finish, 10),
            stamp(received, 10),
        )
    };
    assert_eq!(interval(110, 120, 130), Ok(()));
    for (start, finish, received) in [
        (100, 120, 130),
        (101, 120, 130),
        (110, 109, 130),
        (110, 120, 121),
        (110, 120, 119),
    ] {
        assert_eq!(
            interval(start, finish, received),
            Err(EvidenceAgeError::ClockMismatch)
        );
    }
    assert_eq!(interval(110, 200, 220), Ok(()));
    assert_eq!(interval(110, 200, 221), Err(EvidenceAgeError::StaleState));
    let mut wrong_epoch = stamp(120, 10);
    wrong_epoch.epoch = epoch(2);
    assert_eq!(
        check_diagnostic_interval(stamp(100, 10), stamp(110, 10), wrong_epoch, stamp(130, 10)),
        Err(EvidenceAgeError::ClockMismatch)
    );
    assert_eq!(
        check_diagnostic_interval(
            stamp(100, 10),
            stamp(110, 10),
            stamp(120, 11),
            stamp(130, 10)
        ),
        Err(EvidenceAgeError::ClockMismatch)
    );
}

#[test]
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn native_counter_origin_is_shared_with_controlled_child() {
    let epoch = RuntimeEpoch::fresh();
    let before = sample_monotonic(epoch).unwrap();
    let mut command = Command::new(std::env::var_os("PORTABLE_TEST_HELPER").expect("helper path"));
    command.arg("clock").arg(epoch.to_string());
    let output = child::capture(&mut command, Duration::from_secs(5)).unwrap();
    let after = sample_monotonic(epoch).unwrap();
    let text = std::str::from_utf8(&output).unwrap();
    let mut fields = text.split_whitespace();
    assert_eq!(
        RuntimeEpoch::parse_str(fields.next().unwrap()).unwrap(),
        epoch
    );
    let ticks: u64 = fields.next().unwrap().parse().unwrap();
    let frequency: u64 = fields.next().unwrap().parse().unwrap();
    assert_eq!(fields.next(), None);
    assert_eq!(before.source, after.source);
    assert_eq!(before.stamp.frequency, frequency);
    assert_eq!(after.stamp.frequency, frequency);
    assert!(before.stamp.ticks <= ticks && ticks <= after.stamp.ticks);
}
