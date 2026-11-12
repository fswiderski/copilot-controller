use super::fake::FakeBackend;
use super::*;

const ID: HidIdentity = HidIdentity {
    vendor_id: 0x1234,
    product_id: 0x5678,
};

fn desc(id: HidIdentity, page: u16, path: &str) -> HidDescriptor {
    HidDescriptor {
        identity: id,
        usage_page: page,
        usage: 1,
        product: None,
        serial: None,
        max_input_report_len: 64,
        max_output_report_len: 32,
        interface_number: None,
        bus: None,
        path: HidPath::new(path).unwrap(),
    }
}

fn other() -> HidIdentity {
    HidIdentity {
        vendor_id: 0x1234,
        product_id: 0x9999,
    }
}

#[test]
fn matcher_requires_exact_identity_and_optional_usage() {
    let d = desc(ID, 0xff00, "a");
    assert!(HidMatcher::new(ID).matches(&d));
    assert!(!HidMatcher::new(other()).matches(&d));
    let m = HidMatcher {
        usage_page: Some(1),
        ..HidMatcher::new(ID)
    };
    assert!(!m.matches(&d));
    let m = HidMatcher {
        usage_page: Some(0xff00),
        usage: Some(1),
        ..HidMatcher::new(ID)
    };
    assert!(m.matches(&d));
}

#[test]
fn enumerate_is_metadata_only_and_bounded() {
    let b = FakeBackend::new(vec![
        desc(ID, 1, "a"),
        desc(ID, 2, "b"),
        desc(other(), 1, "c"),
    ]);
    let m = HidMatcher::new(ID);
    assert_eq!(enumerate(&b, &m, 8).unwrap().len(), 2);
    assert_eq!(b.state.borrow().open_calls, 0);
    for limit in [0, MAX_ENUMERATED_DEVICES + 1] {
        assert_eq!(
            enumerate(&b, &m, limit),
            Err(HidError::InvalidArgument(ArgumentError::LimitInvalid))
        );
    }
    // A backend exceeding the limit or ignoring the matcher is rejected.
    assert_eq!(enumerate(&b, &m, 1), Err(HidError::TooManyDevices));
    let mut wide = FakeBackend::new(vec![desc(other(), 1, "c")]);
    wide.ignore_matcher = true;
    assert!(matches!(
        enumerate(&wide, &m, 4),
        Err(HidError::Protocol(_))
    ));
}

#[test]
fn path_rejects_empty_nul_and_oversize() {
    assert!(HidPath::new("").is_err());
    assert!(HidPath::new("a\0b").is_err());
    assert!(HidPath::new("x".repeat(MAX_METADATA_LEN + 1)).is_err());
    assert!(HidPath::new("x".repeat(MAX_METADATA_LEN)).is_ok());
}

#[test]
fn open_rejects_unknown_and_oversized_descriptors() {
    let b = FakeBackend::new(vec![desc(ID, 1, "a")]);
    assert_eq!(
        HidDevice::open(&b, &desc(ID, 1, "zzz")).unwrap_err(),
        HidError::NotFound
    );
    let mut big = desc(ID, 1, "a");
    big.max_output_report_len = MAX_REPORT_LEN + 1;
    assert!(matches!(
        HidDevice::open(&b, &big),
        Err(HidError::InvalidArgument(_))
    ));
    assert_eq!(b.state.borrow().open_calls, 1);
}

#[test]
fn write_bounds() {
    let d = desc(ID, 1, "a");
    let b = FakeBackend::new(vec![d.clone()]);
    let mut dev = HidDevice::open(&b, &d).unwrap();
    assert_eq!(
        dev.write_report(&[]),
        Err(HidError::InvalidArgument(ArgumentError::EmptyReport))
    );
    assert_eq!(
        dev.write_report(&[0; 33]),
        Err(HidError::InvalidArgument(
            ArgumentError::ReportExceedsDevice
        ))
    );
    assert_eq!(dev.write_report(&[0; 32]), Ok(32));
    assert_eq!(b.state.borrow().written.len(), 1);

    let mut unknown = d.clone();
    unknown.max_output_report_len = 0;
    let b = FakeBackend::new(vec![unknown.clone()]);
    let mut dev = HidDevice::open(&b, &unknown).unwrap();
    assert_eq!(
        dev.write_report(&vec![0; MAX_REPORT_LEN + 1]),
        Err(HidError::InvalidArgument(ArgumentError::ReportTooLarge))
    );
    assert_eq!(
        dev.write_report(&vec![0; MAX_REPORT_LEN]),
        Ok(MAX_REPORT_LEN)
    );
    b.state.borrow_mut().write_count = Some(MAX_REPORT_LEN + 1);
    assert!(matches!(dev.write_report(&[1]), Err(HidError::Protocol(_))));
}

#[test]
fn read_bounds() {
    let d = desc(ID, 1, "a");
    let b = FakeBackend::new(vec![d.clone()]);
    let mut dev = HidDevice::open(&b, &d).unwrap();
    let t = Duration::from_millis(10);
    assert!(dev.read_report(0, t).is_err());
    assert!(dev.read_report(MAX_REPORT_LEN + 1, t).is_err());
    assert_eq!(
        dev.read_report(8, MAX_READ_TIMEOUT + Duration::from_millis(1)),
        Err(HidError::InvalidArgument(ArgumentError::TimeoutTooLarge))
    );
    assert_eq!(dev.read_report(8, t), Err(HidError::Timeout));
    b.state.borrow_mut().inputs.push_back(vec![7; 20]);
    assert_eq!(dev.read_report(8, t).unwrap(), vec![7; 8]);
    b.state.borrow_mut().inputs.push_back(vec![1, 2]);
    b.state.borrow_mut().read_count = Some(9);
    assert!(matches!(dev.read_report(8, t), Err(HidError::Protocol(_))));
}

#[test]
fn original_report_descriptor_is_bounded_and_fail_closed_when_missing() {
    let d = desc(ID, 1, "descriptor");
    let b = FakeBackend::new(vec![d.clone()]);
    let mut device = HidDevice::open(&b, &d).unwrap();
    assert_eq!(
        device.report_descriptor(),
        Err(HidError::EvidenceUnavailable(
            EvidenceKind::RawReportDescriptor
        ))
    );

    for bytes in [
        vec![0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0xc0],
        (0..MAX_REPORT_DESCRIPTOR_LEN).map(|n| n as u8).collect(),
    ] {
        b.state.borrow_mut().report_descriptor = Some(bytes.clone());
        assert_eq!(device.report_descriptor().unwrap(), bytes);
    }
    for bytes in [Vec::new(), vec![0x06; MAX_REPORT_DESCRIPTOR_LEN + 1]] {
        b.state.borrow_mut().report_descriptor = Some(bytes);
        assert!(matches!(
            device.report_descriptor(),
            Err(HidError::Protocol(_))
        ));
    }
}

#[cfg(all(windows, any(feature = "micro-hid", feature = "akp03-hid")))]
#[test]
fn windows_native_backend_denies_unscoped_access_without_io() {
    assert!(matches!(
        native_backend(),
        Err(HidError::Unavailable(UnavailableReason::UnsupervisedAccess))
    ));
}

#[test]
fn platform_gating_fails_closed() {
    use UnavailableReason::*;
    let un = PlatformSupport::Unavailable;
    assert_eq!(
        platform_support(false, "windows", "x86_64"),
        un(FeatureDisabled)
    );
    assert_eq!(
        platform_support(false, "linux", "x86_64"),
        un(FeatureDisabled)
    );
    assert_eq!(
        platform_support(true, "linux", "x86_64"),
        un(UnsupportedPlatform)
    );
    assert_eq!(
        platform_support(true, "linux", "aarch64"),
        un(UnsupportedPlatform)
    );
    assert_eq!(
        platform_support(true, "macos", "x86_64"),
        un(UnsupportedPlatform)
    );
    assert_eq!(
        platform_support(true, "windows", "x86"),
        un(UnsupportedPlatform)
    );
    assert_eq!(
        platform_support(true, "windows", "aarch64"),
        PlatformSupport::Supported(HidFeature::WindowsHid)
    );
    assert_eq!(
        platform_support(true, "windows", "x86_64"),
        PlatformSupport::Supported(HidFeature::WindowsHid)
    );
    assert_eq!(
        platform_support(true, "macos", "aarch64"),
        PlatformSupport::Supported(HidFeature::MacosIoKit)
    );
}

#[cfg(not(any(feature = "akp03-hid", feature = "micro-hid")))]
#[test]
fn native_backend_unavailable_without_feature() {
    assert!(matches!(
        native_backend(),
        Err(HidError::Unavailable(UnavailableReason::FeatureDisabled))
    ));
}

#[cfg(all(any(feature = "akp03-hid", feature = "micro-hid"), target_os = "linux"))]
#[test]
fn native_backend_unavailable_on_linux_with_feature() {
    assert!(matches!(
        native_backend(),
        Err(HidError::Unavailable(
            UnavailableReason::UnsupportedPlatform
        ))
    ));
}

#[test]
fn unsupervised_write_fails_closed_before_any_io() {
    let d = desc(ID, 1, "a");
    let b = FakeBackend::new(vec![d.clone()]);
    b.state.borrow_mut().writes_unsupported = true;
    let mut dev = HidDevice::open(&b, &d).unwrap();
    assert_eq!(
        dev.write_report(&[0, 1]),
        Err(HidError::Unavailable(UnavailableReason::UnsupervisedWrite))
    );
    assert!(b.state.borrow().written.is_empty());
    // Argument errors still take precedence, and reads are unaffected.
    assert!(matches!(
        dev.write_report(&[]),
        Err(HidError::InvalidArgument(_))
    ));
    b.state.borrow_mut().inputs.push_back(vec![1]);
    assert_eq!(
        dev.read_report(4, Duration::from_millis(1)).unwrap(),
        vec![1]
    );
}
