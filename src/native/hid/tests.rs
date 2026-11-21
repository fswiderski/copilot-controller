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
        location_id: None,
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
fn filtered_open_requires_exact_unique_bounded_input_frames() {
    let d = desc(ID, 0xff00, "a");
    let b = FakeBackend::new(vec![d.clone()]);
    for reports in [
        &[][..],
        &[(17, 0)][..],
        &[(17, 65)][..],
        &[(17, 8), (17, 9)][..],
    ] {
        assert!(HidDevice::open_filtered(&b, &d, reports).is_err());
    }

    let mut device = HidDevice::open_filtered(&b, &d, &[(17, 20), (19, 32)]).unwrap();
    assert_eq!(
        device.write_report(&[1, 2]),
        Err(HidError::Unavailable(UnavailableReason::UnsupervisedWrite))
    );
    assert_eq!(
        device.read_report(20, Duration::from_millis(1)),
        Err(HidError::Timeout)
    );
    assert!(b.state.borrow().written.is_empty());
}

#[test]
fn mx_creative_review_backend_structurally_rejects_write_access() {
    let descriptor = desc(ID, 0xff00, "a");
    let inner = FakeBackend::new(vec![descriptor.clone()]);
    let state = inner.state.clone();
    let backend = review_readonly_backend(Box::new(inner));

    assert!(matches!(
        backend.open(&descriptor),
        Err(HidError::Unavailable(UnavailableReason::UnsupervisedWrite))
    ));
    let mut transport = backend.open_filtered(&descriptor, &[(17, 20)]).unwrap();
    assert!(!transport.supports_write());
    assert_eq!(
        transport.write(&[17, 0]),
        Err(HidError::Unavailable(UnavailableReason::UnsupervisedWrite))
    );
    assert!(state.borrow().written.is_empty());
    assert_eq!(state.borrow().open_calls, 1);
}

#[test]
fn mx_display_queries_are_exact_typed_get_frames() {
    let descriptor = HidDescriptor {
        identity: HidIdentity {
            vendor_id: 0x046d,
            product_id: 0xc354,
        },
        usage_page: 0xff43,
        usage: 0x1a02,
        product: Some("MX Creative Keypad".into()),
        serial: Some("must never be returned".into()),
        max_input_report_len: 32,
        max_output_report_len: 4095,
        interface_number: None,
        bus: Some(HidBus::Usb),
        location_id: Some(0x0211_0000),
        path: HidPath::new("iokit:1").unwrap(),
    };
    let backend = FakeBackend::new(vec![descriptor.clone()]);
    let mut unrelated = [0; 20];
    unrelated[..4].copy_from_slice(&[0x11, 0xff, 0, 0x0e]);
    let mut root = [0; 20];
    root[..5].copy_from_slice(&[0x11, 0xff, 0, 0x01, 0x0a]);
    let mut keepalive = [0; 20];
    keepalive[..8].copy_from_slice(&[0x11, 0xff, 0x0a, 0x02, 0x01, 0xf4, 0x27, 0x10]);
    backend.state.borrow_mut().inputs.extend([
        unrelated.to_vec(),
        root.to_vec(),
        keepalive.to_vec(),
    ]);

    let mut device =
        MxCreativeDisplayInfoDevice::open(&backend, &descriptor).expect("query device");
    assert_eq!(
        device.query(
            MxCreativeDisplayQuery::Discover(MxCreativeDisplayFeature::Brightness),
            Duration::from_secs(1)
        ),
        Ok(MxCreativeDisplayReply::FeatureIndex(0x0a))
    );
    assert_eq!(
        device.query(
            MxCreativeDisplayQuery::KeepAliveRange {
                feature_index: 0x0a
            },
            Duration::from_secs(1)
        ),
        Ok(MxCreativeDisplayReply::KeepAliveRange {
            minimum_ms: 500,
            maximum_ms: 10_000
        })
    );
    let writes = backend.state.borrow().written.clone();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0], display_query_frame(0, 0, 1, [0x80, 0x40]));
    assert_eq!(writes[1], display_query_frame(0x0a, 0, 2, [0, 0]));
    assert!(
        writes
            .iter()
            .all(|frame| frame.len() == 20 && frame[0] == 0x11)
    );
}

#[test]
fn mx_display_query_rejects_uncorrelated_and_unsupported_writes() {
    let descriptor = desc(ID, 0xff00, "query");
    let backend = FakeBackend::new(vec![descriptor.clone()]);
    let mut wrong_function = [0; 20];
    wrong_function[..5].copy_from_slice(&[0x11, 0xff, 0, 0x11, 9]);
    backend
        .state
        .borrow_mut()
        .inputs
        .push_back(wrong_function.to_vec());
    let mut device = MxCreativeDisplayInfoDevice::open(&backend, &descriptor).unwrap();
    assert!(matches!(
        device.query(
            MxCreativeDisplayQuery::Discover(MxCreativeDisplayFeature::Display),
            Duration::from_millis(10)
        ),
        Err(HidError::Protocol(
            "uncorrelated MX display response function"
        ))
    ));
    backend.state.borrow_mut().writes_unsupported = true;
    assert!(matches!(
        MxCreativeDisplayInfoDevice::open(&backend, &descriptor),
        Err(HidError::Unavailable(UnavailableReason::UnsupervisedWrite))
    ));
    assert_eq!(backend.state.borrow().written.len(), 1);
}

#[test]
fn mx_display_query_accepts_correlated_hidpp_errors_without_echoing_payload() {
    let descriptor = desc(ID, 0xff00, "query-error");
    let backend = FakeBackend::new(vec![descriptor.clone()]);
    let mut error = [0; 20];
    error[..6].copy_from_slice(&[0x11, 0xff, 0xff, 0x00, 0x01, 0x09]);
    backend.state.borrow_mut().inputs.push_back(error.to_vec());
    let mut device = MxCreativeDisplayInfoDevice::open(&backend, &descriptor).unwrap();
    assert_eq!(
        device.query(
            MxCreativeDisplayQuery::Discover(MxCreativeDisplayFeature::Controls),
            Duration::from_secs(1)
        ),
        Ok(MxCreativeDisplayReply::HidppError(0x09))
    );
    assert_eq!(
        device.query(
            MxCreativeDisplayQuery::Discover(MxCreativeDisplayFeature::Controls),
            Duration::ZERO
        ),
        Err(HidError::InvalidArgument(ArgumentError::TimeoutTooLarge))
    );
    assert_eq!(backend.state.borrow().written.len(), 1);
}

#[test]
fn mx_display_query_rejects_wrong_hidpp_error_echo_and_ignores_other_swids() {
    let descriptor = desc(ID, 0xff00, "query-error-correlation");
    let backend = FakeBackend::new(vec![descriptor.clone()]);
    let mut other = [0; 20];
    other[..6].copy_from_slice(&[0x11, 0xff, 0xff, 0x00, 0x0e, 0x09]);
    let mut valid = [0; 20];
    valid[..6].copy_from_slice(&[0x11, 0xff, 0xff, 0x00, 0x01, 0x06]);
    backend
        .state
        .borrow_mut()
        .inputs
        .extend([other.to_vec(), valid.to_vec()]);
    let mut device = MxCreativeDisplayInfoDevice::open(&backend, &descriptor).unwrap();
    assert_eq!(
        device.query(
            MxCreativeDisplayQuery::Discover(MxCreativeDisplayFeature::Brightness),
            Duration::from_secs(1)
        ),
        Ok(MxCreativeDisplayReply::HidppError(0x06))
    );

    let mut wrong_feature = [0; 20];
    wrong_feature[..6].copy_from_slice(&[0x11, 0xff, 0xff, 0x01, 0x02, 0x09]);
    backend
        .state
        .borrow_mut()
        .inputs
        .push_back(wrong_feature.to_vec());
    assert!(matches!(
        device.query(
            MxCreativeDisplayQuery::Discover(MxCreativeDisplayFeature::Display),
            Duration::from_secs(1)
        ),
        Err(HidError::Protocol(
            "uncorrelated MX display HID++ error function"
        ))
    ));
}

#[test]
fn mx_display_check_frames_are_getters_plus_one_fixed_keepalive_function() {
    let descriptor = desc(ID, 0xff00, "query-check");
    let backend = FakeBackend::new(vec![descriptor.clone()]);
    let mut info = [0; 20];
    info[..5].copy_from_slice(&[0x11, 0xff, 9, 0x01, 0x64]);
    let mut current = [0; 20];
    current[..5].copy_from_slice(&[0x11, 0xff, 9, 0x12, 42]);
    let mut hold = [0; 20];
    hold[..4].copy_from_slice(&[0x11, 0xff, 6, 0x13]);
    backend
        .state
        .borrow_mut()
        .inputs
        .extend([info.to_vec(), current.to_vec(), hold.to_vec()]);
    let mut device = MxCreativeDisplayInfoDevice::open(&backend, &descriptor).unwrap();
    assert_eq!(
        device.query(
            MxCreativeDisplayQuery::BrightnessInfo { feature_index: 9 },
            Duration::from_secs(1)
        ),
        Ok(MxCreativeDisplayReply::BrightnessInfo([
            0x64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
        ]))
    );
    assert_eq!(
        device.query(
            MxCreativeDisplayQuery::BrightnessCurrent { feature_index: 9 },
            Duration::from_secs(1)
        ),
        Ok(MxCreativeDisplayReply::BrightnessCurrent(42))
    );
    assert_eq!(
        device.query(
            MxCreativeDisplayQuery::KeepAliveCheck { feature_index: 6 },
            Duration::from_secs(1)
        ),
        Ok(MxCreativeDisplayReply::Acknowledged)
    );
    let writes = backend.state.borrow().written.clone();
    assert_eq!(
        writes
            .iter()
            .map(|frame| [frame[2], frame[3], frame[4], frame[5]])
            .collect::<Vec<_>>(),
        [[9, 0x01, 0, 0], [9, 0x12, 0, 0], [6, 0x13, 0x03, 0xe8]]
    );
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

#[test]
fn exclusive_probe_defaults_unsupported_and_does_not_change_normal_open() {
    let descriptor = desc(ID, 1, "exclusive");
    let backend = FakeBackend::new(vec![descriptor.clone()]);
    assert_eq!(
        backend.open_exclusive_probe(&descriptor),
        Err(HidError::Unavailable(
            UnavailableReason::UnsupportedPlatform
        ))
    );
    assert_eq!(backend.state.borrow().exclusive_open_calls, 1);
    assert_eq!(backend.state.borrow().exclusive_open_options, vec![1]);
    let _device = HidDevice::open(&backend, &descriptor).unwrap();
    assert_eq!(backend.state.borrow().open_calls, 1);
}
