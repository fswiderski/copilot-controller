//! Encapsulated native HID transport boundary.
//!
//! Nothing here runs from `doctor` or previews. Enumeration and open happen only
//! when an explicit opt-in caller constructs a backend via [`native_backend`] and
//! invokes it. Enumeration returns metadata only; devices are opened explicitly.
//!
//! # Deadlines
//! Platform HID calls are synchronous. Report reads accept a timeout that the
//! native backends attempt to honor (overlapped I/O on Windows, run-loop timeout
//! on macOS), but the behavior has not been validated against hardware. Windows
//! writes use a fixed best-effort deadline. Windows HID and macOS IOKit access
//! require a private capability issued only inside the verified supervised worker.
//! `IOHIDDeviceSetReport` can block indefinitely; the parent deadline and child
//! watchdog bound the worker lifetime on a responsive OS, not an individual
//! write timeout. Callers must run any live use inside a
//! disposable, supervised worker process that can be killed; this module does not
//! provide a bounded live probe.
//!
//! Windows reads typed BusTypeGuid evidence with a bounded native parent walk
//! across exact logical HID bus nodes (never past unknown/missing transport),
//! but cannot supply a documented native interface number or original report
//! descriptor (the report-descriptor IOCTL is for HID minidrivers, not applications).
//! The macOS backend reads those from native properties: direct IOHID properties
//! first, then a bounded IOService-parent walk for `IOUSBHostInterface`
//! `bInterfaceNumber` with exact VID/PID identity gating.
//! Consumers requiring either artifact fail closed
//! with [`HidError::EvidenceUnavailable`] rather than infer it from parsed caps or
//! a device path.
//!
//! A successful transport call is not device qualification or action readiness.

use std::fmt;
use std::time::Duration;

mod platform;
pub use platform::{HidFeature, PlatformSupport, platform_support};

#[cfg(all(
    any(
        feature = "akp03-hid",
        feature = "micro-hid",
        feature = "mx-creative-hid"
    ),
    target_os = "macos",
    target_arch = "aarch64"
))]
mod iokit;
#[cfg(all(
    test,
    any(
        feature = "akp03-hid",
        feature = "micro-hid",
        feature = "mx-creative-hid"
    ),
    target_os = "macos",
    target_arch = "aarch64"
))]
pub(crate) use iokit::fixture_output;
#[cfg(all(
    any(
        feature = "akp03-hid",
        feature = "micro-hid",
        feature = "mx-creative-hid"
    ),
    windows,
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod win32;
#[cfg(any(
    test,
    all(
        any(
            feature = "akp03-hid",
            feature = "micro-hid",
            feature = "mx-creative-hid"
        ),
        windows,
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]
mod win32_evidence;

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;

/// Largest report (including any report-ID byte) accepted or returned.
pub const MAX_REPORT_LEN: usize = 1025;
/// Largest original HID report descriptor accepted by the evidence boundary.
pub const MAX_REPORT_DESCRIPTOR_LEN: usize = 1024;
const MAX_DEVICE_OUTPUT_REPORT_LEN: usize = 4095;
/// Largest number of descriptors a single enumeration may return.
pub const MAX_ENUMERATED_DEVICES: usize = 64;
/// Longest accepted read timeout.
pub const MAX_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest accepted string/path metadata, in bytes.
pub const MAX_METADATA_LEN: usize = 512;

/// USB-style vendor/product identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HidIdentity {
    pub vendor_id: u16,
    pub product_id: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceKind {
    InterfaceNumber,
    BusClassification,
    RawReportDescriptor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HidBus {
    Usb,
    Bluetooth,
    Unknown,
}

/// Opaque backend-specific device location. Never parsed by callers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HidPath(String);

impl HidPath {
    pub fn new(path: impl Into<String>) -> Result<Self, HidError> {
        let path = path.into();
        if path.is_empty() || path.len() > MAX_METADATA_LEN || path.contains('\0') {
            return Err(HidError::InvalidArgument(ArgumentError::Path));
        }
        Ok(Self(path))
    }

    #[allow(dead_code)] // used only by native backends
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Metadata for one enumerated interface. Holding one grants no device access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HidDescriptor {
    pub identity: HidIdentity,
    pub usage_page: u16,
    pub usage: u16,
    pub product: Option<String>,
    pub serial: Option<String>,
    /// Maximum input report size in bytes, including the report-ID byte; 0 if unknown.
    pub max_input_report_len: usize,
    /// Maximum output report size in bytes, including the report-ID byte; 0 if unknown.
    pub max_output_report_len: usize,
    /// Exact native interface number when the OS exposes it; never inferred from a path.
    pub interface_number: Option<i32>,
    /// Native bus classification; never inferred from a device path.
    pub bus: Option<HidBus>,
    /// Native USB LocationID when exposed; never inferred.
    pub location_id: Option<u32>,
    pub path: HidPath,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportDescriptorEvidence {
    pub bytes: Vec<u8>,
    pub source_path: String,
    pub property_type: String,
}

/// Selection criteria; vendor and product are mandatory so no blanket scan exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HidMatcher {
    pub identity: HidIdentity,
    pub usage_page: Option<u16>,
    pub usage: Option<u16>,
}

impl HidMatcher {
    pub fn new(identity: HidIdentity) -> Self {
        Self {
            identity,
            usage_page: None,
            usage: None,
        }
    }

    pub fn matches(&self, d: &HidDescriptor) -> bool {
        self.identity == d.identity
            && self.usage_page.is_none_or(|p| p == d.usage_page)
            && self.usage.is_none_or(|u| u == d.usage)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    /// No device-specific HID feature was compiled in.
    FeatureDisabled,
    UnsupportedPlatform,
    /// The transport lacks supervised write authority.
    UnsupervisedWrite,
    /// Native macOS access requires the private supervised-child capability.
    UnsupervisedAccess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgumentError {
    Path,
    EmptyReport,
    ReportTooLarge,
    ReportExceedsDevice,
    BufferTooLarge,
    BufferEmpty,
    TimeoutTooLarge,
    LimitInvalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HidOp {
    Enumerate,
    Open,
    Close,
    Write,
    Read,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HidError {
    /// The backend is not present; this is never equivalent to "no devices".
    Unavailable(UnavailableReason),
    InvalidArgument(ArgumentError),
    /// More devices matched than the caller's limit allows.
    TooManyDevices,
    /// The device is gone or its identity no longer matches the descriptor.
    NotFound,
    /// The OS or device returned inconsistent or oversized data.
    Protocol(&'static str),
    /// The platform did not expose a required, non-derivable identity artifact.
    EvidenceUnavailable(EvidenceKind),
    Timeout,
    /// OS failure with the platform's raw numeric code.
    Os {
        op: HidOp,
        code: i64,
    },
}

impl fmt::Display for HidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for HidError {}

/// Backend that enumerates metadata and opens devices on explicit request.
pub trait HidBackend {
    /// Returns at most `limit` matching descriptors, or `TooManyDevices`.
    fn enumerate(&self, matcher: &HidMatcher, limit: usize)
    -> Result<Vec<HidDescriptor>, HidError>;
    fn open(&self, descriptor: &HidDescriptor) -> Result<Box<dyn HidTransport>, HidError>;

    /// Opens with native callback retention restricted to exact report IDs and
    /// normalized total lengths. Implementations without this support fail closed.
    fn open_filtered(
        &self,
        _descriptor: &HidDescriptor,
        _reports: &[(u8, usize)],
    ) -> Result<Box<dyn HidTransport>, HidError> {
        Err(HidError::Unavailable(
            UnavailableReason::UnsupportedPlatform,
        ))
    }

    /// Opens a composite-interface candidate whose primary usage may differ
    /// after reopening. Implementations should preserve every other identity check.
    fn open_allowing_usage_change(
        &self,
        descriptor: &HidDescriptor,
    ) -> Result<Box<dyn HidTransport>, HidError> {
        self.open(descriptor)
    }

    /// Reads original HID report-descriptor bytes without opening transport when supported.
    fn original_report_descriptor(&self, _descriptor: &HidDescriptor) -> Result<Vec<u8>, HidError> {
        Err(HidError::EvidenceUnavailable(
            EvidenceKind::RawReportDescriptor,
        ))
    }

    fn original_report_descriptor_evidence(
        &self,
        descriptor: &HidDescriptor,
    ) -> Result<ReportDescriptorEvidence, HidError> {
        Ok(ReportDescriptorEvidence {
            bytes: self.original_report_descriptor(descriptor)?,
            source_path: descriptor.path.as_str().to_owned(),
            property_type: "backend".into(),
        })
    }

    /// Attempts a single exclusive open/close sequence for diagnostics.
    fn open_exclusive_probe(
        &self,
        _descriptor: &HidDescriptor,
    ) -> Result<ExclusiveOpenProbe, HidError> {
        Err(HidError::Unavailable(
            UnavailableReason::UnsupportedPlatform,
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExclusiveOpenProbe {
    pub requested: bool,
    pub acquired: bool,
    pub released: bool,
    pub open_code: i64,
    pub close_code: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MxCreativeDisplayFeature {
    DeviceFirmware,
    KeepAlive,
    Brightness,
    Display,
    Controls,
}

impl MxCreativeDisplayFeature {
    pub(crate) const fn id(self) -> u16 {
        match self {
            Self::DeviceFirmware => 0x0003,
            Self::KeepAlive => 0x0008,
            Self::Brightness => 0x8040,
            Self::Display => 0x19a1,
            Self::Controls => 0x1b04,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MxCreativeDisplayQuery {
    Discover(MxCreativeDisplayFeature),
    KeepAliveRange { feature_index: u8 },
    BrightnessInfo { feature_index: u8 },
    BrightnessCurrent { feature_index: u8 },
    KeepAliveCheck { feature_index: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MxCreativeFirmwareQuery {
    Discover(MxCreativeDisplayFeature),
    Count {
        feature_index: u8,
    },
    Version {
        feature_index: u8,
        firmware_index: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MxCreativeFirmwareReply {
    FeatureIndex(u8),
    Count(u8),
    Version([u8; 16]),
    HidppError(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MxCreativeDisplayReply {
    FeatureIndex(u8),
    KeepAliveRange { minimum_ms: u16, maximum_ms: u16 },
    BrightnessInfo([u8; 16]),
    BrightnessCurrent(u8),
    Acknowledged,
    HidppError(u8),
}

pub(crate) struct MxCreativeDisplayInfoBackend {
    inner: Box<dyn HidBackend>,
}

impl MxCreativeDisplayInfoBackend {
    pub(crate) fn enumerate(
        &self,
        matcher: &HidMatcher,
        limit: usize,
    ) -> Result<Vec<HidDescriptor>, HidError> {
        enumerate(self.inner.as_ref(), matcher, limit)
    }

    pub(crate) fn original_report_descriptor_evidence(
        &self,
        descriptor: &HidDescriptor,
    ) -> Result<ReportDescriptorEvidence, HidError> {
        self.inner.original_report_descriptor_evidence(descriptor)
    }

    pub(crate) fn open(
        &self,
        descriptor: &HidDescriptor,
    ) -> Result<MxCreativeDisplayInfoDevice, HidError> {
        MxCreativeDisplayInfoDevice::open(self.inner.as_ref(), descriptor)
    }

    #[cfg(test)]
    pub(crate) fn from_test_backend(inner: Box<dyn HidBackend>) -> Self {
        Self { inner }
    }
}

pub(crate) struct MxCreativeDisplayInfoDevice {
    inner: MxCreativeHidppDevice,
}

pub(crate) struct MxCreativeFirmwareInfoBackend {
    inner: Box<dyn HidBackend>,
}

impl MxCreativeFirmwareInfoBackend {
    pub(crate) fn enumerate(
        &self,
        matcher: &HidMatcher,
        limit: usize,
    ) -> Result<Vec<HidDescriptor>, HidError> {
        enumerate(self.inner.as_ref(), matcher, limit)
    }

    pub(crate) fn original_report_descriptor_evidence(
        &self,
        descriptor: &HidDescriptor,
    ) -> Result<ReportDescriptorEvidence, HidError> {
        self.inner.original_report_descriptor_evidence(descriptor)
    }

    pub(crate) fn open(
        &self,
        descriptor: &HidDescriptor,
    ) -> Result<MxCreativeFirmwareInfoDevice, HidError> {
        MxCreativeFirmwareInfoDevice::open(self.inner.as_ref(), descriptor)
    }

    #[cfg(test)]
    pub(crate) fn from_test_backend(inner: Box<dyn HidBackend>) -> Self {
        Self { inner }
    }
}

pub(crate) struct MxCreativeFirmwareInfoDevice {
    inner: MxCreativeHidppDevice,
}

struct MxCreativeHidppDevice {
    transport: Box<dyn HidTransport>,
    next_software_id: u8,
    requests_sent: u8,
}

impl MxCreativeHidppDevice {
    const REPORT_ID: u8 = 0x11;
    const REPORT_LEN: usize = 20;
    const DEVICE_INDEX: u8 = 0xff;

    pub(crate) fn open(
        backend: &dyn HidBackend,
        descriptor: &HidDescriptor,
    ) -> Result<Self, HidError> {
        validate_read_descriptor(descriptor)?;
        if descriptor.max_output_report_len < Self::REPORT_LEN {
            return Err(HidError::InvalidArgument(
                ArgumentError::ReportExceedsDevice,
            ));
        }
        let transport =
            backend.open_filtered(descriptor, &[(Self::REPORT_ID, Self::REPORT_LEN)])?;
        if !transport.supports_write() {
            return Err(HidError::Unavailable(UnavailableReason::UnsupervisedWrite));
        }
        Ok(Self {
            transport,
            next_software_id: 1,
            requests_sent: 0,
        })
    }

    pub(crate) const fn requests_sent(&self) -> u8 {
        self.requests_sent
    }

    pub(crate) fn report_descriptor(&mut self) -> Result<Vec<u8>, HidError> {
        self.transport.report_descriptor()
    }

    fn query(
        &mut self,
        feature_index: u8,
        function: u8,
        parameters: [u8; 2],
        timeout: Duration,
    ) -> Result<Result<[u8; 16], u8>, HidError> {
        if timeout.is_zero() || timeout > Duration::from_secs(2) {
            return Err(HidError::InvalidArgument(ArgumentError::TimeoutTooLarge));
        }
        let software_id = self.next_software_id;
        self.next_software_id = self
            .next_software_id
            .checked_add(1)
            .filter(|next| *next <= 0x0f)
            .ok_or(HidError::Protocol(
                "MX display query software IDs exhausted",
            ))?;
        let frame = display_query_frame(feature_index, function, software_id, parameters);
        if self.transport.write(&frame)? != frame.len() {
            return Err(HidError::Protocol(
                "MX display query report was not fully written",
            ));
        }
        self.requests_sent = self.requests_sent.checked_add(1).ok_or(HidError::Protocol(
            "MX display query request count exhausted",
        ))?;

        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or(HidError::InvalidArgument(ArgumentError::TimeoutTooLarge))?;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(HidError::Timeout);
            }
            let mut buffer = [0; Self::REPORT_LEN];
            let count = self.transport.read(&mut buffer, remaining)?;
            if count != Self::REPORT_LEN || buffer[0] != Self::REPORT_ID {
                return Err(HidError::Protocol("unexpected MX display response framing"));
            }
            if buffer[1] != Self::DEVICE_INDEX {
                return Err(HidError::Protocol("unexpected MX display response device"));
            }
            if buffer[2] == 0xff {
                if buffer[4] & 0x0f != software_id {
                    continue;
                }
                if buffer[3] != feature_index || buffer[4] >> 4 != function {
                    return Err(HidError::Protocol(
                        "uncorrelated MX display HID++ error function",
                    ));
                }
                return Ok(Err(buffer[5]));
            }
            if buffer[3] & 0x0f != software_id {
                continue;
            }
            if buffer[2] != feature_index || buffer[3] >> 4 != function {
                return Err(HidError::Protocol(
                    "uncorrelated MX display response function",
                ));
            }
            return Ok(Ok(buffer[4..20]
                .try_into()
                .expect("fixed HID++ response payload length")));
        }
    }
}

impl MxCreativeDisplayInfoDevice {
    fn open(backend: &dyn HidBackend, descriptor: &HidDescriptor) -> Result<Self, HidError> {
        Ok(Self {
            inner: MxCreativeHidppDevice::open(backend, descriptor)?,
        })
    }

    pub(crate) const fn requests_sent(&self) -> u8 {
        self.inner.requests_sent()
    }

    pub(crate) fn report_descriptor(&mut self) -> Result<Vec<u8>, HidError> {
        self.inner.report_descriptor()
    }

    pub(crate) fn query(
        &mut self,
        query: MxCreativeDisplayQuery,
        timeout: Duration,
    ) -> Result<MxCreativeDisplayReply, HidError> {
        let (feature_index, function, parameters) = match query {
            MxCreativeDisplayQuery::Discover(feature) => (
                0,
                0,
                [
                    u8::try_from(feature.id() >> 8).expect("16-bit shift"),
                    feature.id() as u8,
                ],
            ),
            MxCreativeDisplayQuery::KeepAliveRange { feature_index }
                if (1..=0xfe).contains(&feature_index) =>
            {
                (feature_index, 0, [0, 0])
            }
            MxCreativeDisplayQuery::BrightnessInfo { feature_index }
                if (1..=0xfe).contains(&feature_index) =>
            {
                (feature_index, 0, [0, 0])
            }
            MxCreativeDisplayQuery::BrightnessCurrent { feature_index }
                if (1..=0xfe).contains(&feature_index) =>
            {
                (feature_index, 1, [0, 0])
            }
            MxCreativeDisplayQuery::KeepAliveCheck { feature_index }
                if (1..=0xfe).contains(&feature_index) =>
            {
                (feature_index, 1, 1000u16.to_be_bytes())
            }
            MxCreativeDisplayQuery::KeepAliveRange { .. }
            | MxCreativeDisplayQuery::BrightnessInfo { .. }
            | MxCreativeDisplayQuery::BrightnessCurrent { .. }
            | MxCreativeDisplayQuery::KeepAliveCheck { .. } => {
                return Err(HidError::InvalidArgument(ArgumentError::ReportTooLarge));
            }
        };
        match self
            .inner
            .query(feature_index, function, parameters, timeout)?
        {
            Err(code) => Ok(MxCreativeDisplayReply::HidppError(code)),
            Ok(payload) => Ok(match query {
                MxCreativeDisplayQuery::Discover(_) => {
                    MxCreativeDisplayReply::FeatureIndex(payload[0])
                }
                MxCreativeDisplayQuery::KeepAliveRange { .. } => {
                    MxCreativeDisplayReply::KeepAliveRange {
                        minimum_ms: u16::from_be_bytes([payload[0], payload[1]]),
                        maximum_ms: u16::from_be_bytes([payload[2], payload[3]]),
                    }
                }
                MxCreativeDisplayQuery::BrightnessInfo { .. } => {
                    MxCreativeDisplayReply::BrightnessInfo(payload)
                }
                MxCreativeDisplayQuery::BrightnessCurrent { .. } => {
                    MxCreativeDisplayReply::BrightnessCurrent(payload[0])
                }
                MxCreativeDisplayQuery::KeepAliveCheck { .. } => {
                    MxCreativeDisplayReply::Acknowledged
                }
            }),
        }
    }
}

impl MxCreativeFirmwareInfoDevice {
    fn open(backend: &dyn HidBackend, descriptor: &HidDescriptor) -> Result<Self, HidError> {
        Ok(Self {
            inner: MxCreativeHidppDevice::open(backend, descriptor)?,
        })
    }

    pub(crate) const fn requests_sent(&self) -> u8 {
        self.inner.requests_sent()
    }

    pub(crate) fn report_descriptor(&mut self) -> Result<Vec<u8>, HidError> {
        self.inner.report_descriptor()
    }

    pub(crate) fn query(
        &mut self,
        query: MxCreativeFirmwareQuery,
        timeout: Duration,
    ) -> Result<MxCreativeFirmwareReply, HidError> {
        let (feature_index, function, parameters) = match query {
            MxCreativeFirmwareQuery::Discover(MxCreativeDisplayFeature::DeviceFirmware) => {
                (0, 0, [0, 3])
            }
            MxCreativeFirmwareQuery::Discover(_) => {
                return Err(HidError::InvalidArgument(ArgumentError::ReportTooLarge));
            }
            MxCreativeFirmwareQuery::Count { feature_index } => (feature_index, 0, [0, 0]),
            MxCreativeFirmwareQuery::Version {
                feature_index,
                firmware_index,
            } if firmware_index < 8 => (feature_index, 1, [firmware_index, 0]),
            MxCreativeFirmwareQuery::Version { .. } => {
                return Err(HidError::InvalidArgument(ArgumentError::ReportTooLarge));
            }
        };
        if feature_index == 0xff
            || (function != 0 && feature_index == 0)
            || (function == 0
                && feature_index == 0
                && !matches!(query, MxCreativeFirmwareQuery::Discover(_)))
        {
            return Err(HidError::InvalidArgument(ArgumentError::ReportTooLarge));
        }
        match self
            .inner
            .query(feature_index, function, parameters, timeout)?
        {
            Err(code) => Ok(MxCreativeFirmwareReply::HidppError(code)),
            Ok(payload) => Ok(match query {
                MxCreativeFirmwareQuery::Discover(_) => {
                    MxCreativeFirmwareReply::FeatureIndex(payload[0])
                }
                MxCreativeFirmwareQuery::Count { .. } => MxCreativeFirmwareReply::Count(payload[0]),
                MxCreativeFirmwareQuery::Version { .. } => {
                    MxCreativeFirmwareReply::Version(payload)
                }
            }),
        }
    }
}

fn display_query_frame(
    feature_index: u8,
    function: u8,
    software_id: u8,
    parameters: [u8; 2],
) -> [u8; MxCreativeHidppDevice::REPORT_LEN] {
    debug_assert!(function <= 0x0f);
    debug_assert!((1..=0x0f).contains(&software_id));
    let mut frame = [0; MxCreativeHidppDevice::REPORT_LEN];
    frame[0] = MxCreativeHidppDevice::REPORT_ID;
    frame[1] = MxCreativeHidppDevice::DEVICE_INDEX;
    frame[2] = feature_index;
    frame[3] = (function << 4) | software_id;
    frame[4] = parameters[0];
    frame[5] = parameters[1];
    frame
}

/// Raw report I/O. Implementations may assume arguments were bounds-checked by
/// [`HidDevice`], the only way callers reach them.
pub trait HidTransport {
    /// Whether this transport may write at all; `false` fails closed before any I/O.
    fn supports_write(&self) -> bool {
        true
    }
    fn write(&mut self, report: &[u8]) -> Result<usize, HidError>;
    /// Reads one input report into `buf`, returning its length.
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, HidError>;
    /// Returns original HID report-descriptor bytes only when provided by the OS.
    fn report_descriptor(&mut self) -> Result<Vec<u8>, HidError> {
        Err(HidError::EvidenceUnavailable(
            EvidenceKind::RawReportDescriptor,
        ))
    }
}

struct ReadOnlyTransport {
    inner: Box<dyn HidTransport>,
}

impl HidTransport for ReadOnlyTransport {
    fn supports_write(&self) -> bool {
        false
    }

    fn write(&mut self, _report: &[u8]) -> Result<usize, HidError> {
        Err(HidError::Unavailable(UnavailableReason::UnsupervisedWrite))
    }

    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, HidError> {
        self.inner.read(buf, timeout)
    }

    fn report_descriptor(&mut self) -> Result<Vec<u8>, HidError> {
        self.inner.report_descriptor()
    }
}

/// An explicitly opened device with bounds enforced on every call.
pub struct HidDevice {
    descriptor: HidDescriptor,
    transport: Box<dyn HidTransport>,
}

impl fmt::Debug for HidDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HidDevice")
            .field("descriptor", &self.descriptor)
            .finish()
    }
}

impl HidDevice {
    /// Explicit open. The descriptor must come from the same backend's enumeration.
    pub fn open(backend: &dyn HidBackend, descriptor: &HidDescriptor) -> Result<Self, HidError> {
        Self::open_with(backend, descriptor, false)
    }

    /// Open a composite device while allowing its primary usage to change on
    /// reopen; identity, path, interface, and transport must still match.
    pub fn open_allowing_usage_change(
        backend: &dyn HidBackend,
        descriptor: &HidDescriptor,
    ) -> Result<Self, HidError> {
        Self::open_with(backend, descriptor, true)
    }

    pub fn open_filtered(
        backend: &dyn HidBackend,
        descriptor: &HidDescriptor,
        reports: &[(u8, usize)],
    ) -> Result<Self, HidError> {
        validate_read_descriptor(descriptor)?;
        if reports.is_empty()
            || reports.len() > 16
            || reports
                .iter()
                .any(|(_, len)| *len == 0 || *len > descriptor.max_input_report_len)
        {
            return Err(HidError::InvalidArgument(ArgumentError::BufferEmpty));
        }
        let mut ids = reports.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        ids.sort_unstable();
        if ids.windows(2).any(|ids| ids[0] == ids[1]) {
            return Err(HidError::InvalidArgument(ArgumentError::LimitInvalid));
        }
        Ok(Self {
            descriptor: descriptor.clone(),
            transport: Box::new(ReadOnlyTransport {
                inner: backend.open_filtered(descriptor, reports)?,
            }),
        })
    }

    fn open_with(
        backend: &dyn HidBackend,
        descriptor: &HidDescriptor,
        allow_usage_change: bool,
    ) -> Result<Self, HidError> {
        validate_descriptor(descriptor)?;
        let transport = if allow_usage_change {
            backend.open_allowing_usage_change(descriptor)?
        } else {
            backend.open(descriptor)?
        };
        Ok(Self {
            descriptor: descriptor.clone(),
            transport,
        })
    }

    pub fn descriptor(&self) -> &HidDescriptor {
        &self.descriptor
    }

    /// Reads a bounded original descriptor; parsed or equivalent descriptors are not accepted.
    pub fn report_descriptor(&mut self) -> Result<Vec<u8>, HidError> {
        let bytes = self.transport.report_descriptor()?;
        if bytes.is_empty() || bytes.len() > MAX_REPORT_DESCRIPTOR_LEN {
            return Err(HidError::Protocol(
                "report descriptor length is outside 1..=1024",
            ));
        }
        Ok(bytes)
    }

    /// Writes one output report (first byte is the report ID, 0 if unnumbered).
    pub fn write_report(&mut self, report: &[u8]) -> Result<usize, HidError> {
        if report.is_empty() {
            return Err(HidError::InvalidArgument(ArgumentError::EmptyReport));
        }
        if report.len() > MAX_REPORT_LEN {
            return Err(HidError::InvalidArgument(ArgumentError::ReportTooLarge));
        }
        let max = self.descriptor.max_output_report_len;
        if max != 0 && report.len() > max {
            return Err(HidError::InvalidArgument(
                ArgumentError::ReportExceedsDevice,
            ));
        }
        if !self.transport.supports_write() {
            return Err(HidError::Unavailable(UnavailableReason::UnsupervisedWrite));
        }
        let n = self.transport.write(report)?;
        if n > report.len() {
            return Err(HidError::Protocol("write count exceeds report"));
        }
        Ok(n)
    }

    /// Reads at most `max_len` bytes of one input report within `timeout`.
    pub fn read_report(&mut self, max_len: usize, timeout: Duration) -> Result<Vec<u8>, HidError> {
        if max_len == 0 {
            return Err(HidError::InvalidArgument(ArgumentError::BufferEmpty));
        }
        if max_len > MAX_REPORT_LEN {
            return Err(HidError::InvalidArgument(ArgumentError::BufferTooLarge));
        }
        if timeout > MAX_READ_TIMEOUT {
            return Err(HidError::InvalidArgument(ArgumentError::TimeoutTooLarge));
        }
        let mut buf = vec![0u8; max_len];
        let n = self.transport.read(&mut buf, timeout)?;
        if n > max_len {
            return Err(HidError::Protocol("read count exceeds buffer"));
        }
        buf.truncate(n);
        Ok(buf)
    }
}

fn validate_descriptor(d: &HidDescriptor) -> Result<(), HidError> {
    if d.max_input_report_len > MAX_REPORT_LEN || d.max_output_report_len > MAX_REPORT_LEN {
        return Err(HidError::InvalidArgument(ArgumentError::ReportTooLarge));
    }
    Ok(())
}

fn validate_read_descriptor(d: &HidDescriptor) -> Result<(), HidError> {
    if d.max_input_report_len > MAX_REPORT_LEN
        || d.max_output_report_len > MAX_DEVICE_OUTPUT_REPORT_LEN
    {
        return Err(HidError::InvalidArgument(ArgumentError::ReportTooLarge));
    }
    Ok(())
}

/// Enumerates metadata only; validates limit and filters through `matcher` again
/// so a misbehaving backend cannot widen the result.
pub fn enumerate(
    backend: &dyn HidBackend,
    matcher: &HidMatcher,
    limit: usize,
) -> Result<Vec<HidDescriptor>, HidError> {
    if limit == 0 || limit > MAX_ENUMERATED_DEVICES {
        return Err(HidError::InvalidArgument(ArgumentError::LimitInvalid));
    }
    let found = backend.enumerate(matcher, limit)?;
    if found.len() > limit {
        return Err(HidError::TooManyDevices);
    }
    for d in &found {
        validate_read_descriptor(d)?;
        if !matcher.matches(d) {
            return Err(HidError::Protocol("backend returned unmatched descriptor"));
        }
    }
    Ok(found)
}

/// Explicit opt-in constructor for the platform backend. Fails closed with a
/// typed `Unavailable` when the feature, OS, or architecture is unsupported
/// (always on Linux). On Windows/macOS, also requires the private supervised-child
/// capability; ordinary in-process API calls fail closed. Construction touches
/// no device.
pub fn native_backend() -> Result<Box<dyn HidBackend>, HidError> {
    match platform_support(
        cfg!(any(
            feature = "akp03-hid",
            feature = "micro-hid",
            feature = "mx-creative-hid"
        )),
        std::env::consts::OS,
        std::env::consts::ARCH,
    ) {
        PlatformSupport::Supported(_) => native_backend_impl(),
        PlatformSupport::Unavailable(r) => Err(HidError::Unavailable(r)),
    }
}

/// Backend constructor admitted only by the dedicated MX Creative review scope.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) fn mx_creative_review_backend() -> Result<Box<dyn HidBackend>, HidError> {
    #[cfg(all(
        feature = "mx-creative-hid",
        target_os = "macos",
        target_arch = "aarch64"
    ))]
    {
        Ok(review_readonly_backend(Box::new(
            iokit::IoKitHid::new_for_mx_creative_review()?,
        )))
    }
    #[cfg(not(feature = "mx-creative-hid"))]
    {
        Err(HidError::Unavailable(UnavailableReason::FeatureDisabled))
    }
    #[cfg(all(
        feature = "mx-creative-hid",
        not(all(target_os = "macos", target_arch = "aarch64"))
    ))]
    {
        Err(HidError::Unavailable(
            UnavailableReason::UnsupportedPlatform,
        ))
    }
}

/// Backend constructor admitted only by the dedicated MX Creative display-info
/// authority. The transport remains hidden behind
/// [`MxCreativeDisplayInfoDevice`]'s typed query enum.
pub(crate) fn mx_creative_display_info_backend() -> Result<MxCreativeDisplayInfoBackend, HidError> {
    #[cfg(all(
        feature = "mx-creative-hid",
        target_os = "macos",
        target_arch = "aarch64"
    ))]
    {
        Ok(MxCreativeDisplayInfoBackend {
            inner: Box::new(iokit::IoKitHid::new_for_mx_creative_display_info()?),
        })
    }
    #[cfg(not(feature = "mx-creative-hid"))]
    {
        Err(HidError::Unavailable(UnavailableReason::FeatureDisabled))
    }
    #[cfg(all(
        feature = "mx-creative-hid",
        not(all(target_os = "macos", target_arch = "aarch64"))
    ))]
    {
        Err(HidError::Unavailable(
            UnavailableReason::UnsupportedPlatform,
        ))
    }
}

pub(crate) fn mx_creative_firmware_info_backend() -> Result<MxCreativeFirmwareInfoBackend, HidError>
{
    #[cfg(all(
        feature = "mx-creative-hid",
        target_os = "macos",
        target_arch = "aarch64"
    ))]
    {
        Ok(MxCreativeFirmwareInfoBackend {
            inner: Box::new(iokit::IoKitHid::new_for_mx_creative_firmware_info()?),
        })
    }
    #[cfg(not(feature = "mx-creative-hid"))]
    {
        Err(HidError::Unavailable(UnavailableReason::FeatureDisabled))
    }
    #[cfg(all(
        feature = "mx-creative-hid",
        not(all(target_os = "macos", target_arch = "aarch64"))
    ))]
    {
        Err(HidError::Unavailable(
            UnavailableReason::UnsupportedPlatform,
        ))
    }
}

#[cfg(any(
    test,
    all(
        feature = "mx-creative-hid",
        target_os = "macos",
        target_arch = "aarch64"
    )
))]
fn review_readonly_backend(inner: Box<dyn HidBackend>) -> Box<dyn HidBackend> {
    Box::new(ReviewReadOnlyBackend { inner })
}

#[cfg(any(
    test,
    all(
        feature = "mx-creative-hid",
        target_os = "macos",
        target_arch = "aarch64"
    )
))]
struct ReviewReadOnlyBackend {
    inner: Box<dyn HidBackend>,
}

#[cfg(any(
    test,
    all(
        feature = "mx-creative-hid",
        target_os = "macos",
        target_arch = "aarch64"
    )
))]
impl HidBackend for ReviewReadOnlyBackend {
    fn enumerate(
        &self,
        matcher: &HidMatcher,
        limit: usize,
    ) -> Result<Vec<HidDescriptor>, HidError> {
        self.inner.enumerate(matcher, limit)
    }

    fn open(&self, _descriptor: &HidDescriptor) -> Result<Box<dyn HidTransport>, HidError> {
        Err(HidError::Unavailable(UnavailableReason::UnsupervisedWrite))
    }

    fn open_filtered(
        &self,
        descriptor: &HidDescriptor,
        reports: &[(u8, usize)],
    ) -> Result<Box<dyn HidTransport>, HidError> {
        Ok(Box::new(ReviewReadOnlyTransport {
            inner: self.inner.open_filtered(descriptor, reports)?,
        }))
    }

    fn open_allowing_usage_change(
        &self,
        _descriptor: &HidDescriptor,
    ) -> Result<Box<dyn HidTransport>, HidError> {
        Err(HidError::Unavailable(UnavailableReason::UnsupervisedWrite))
    }

    fn original_report_descriptor(&self, descriptor: &HidDescriptor) -> Result<Vec<u8>, HidError> {
        self.inner.original_report_descriptor(descriptor)
    }

    fn original_report_descriptor_evidence(
        &self,
        descriptor: &HidDescriptor,
    ) -> Result<ReportDescriptorEvidence, HidError> {
        self.inner.original_report_descriptor_evidence(descriptor)
    }
}

#[cfg(any(
    test,
    all(
        feature = "mx-creative-hid",
        target_os = "macos",
        target_arch = "aarch64"
    )
))]
struct ReviewReadOnlyTransport {
    inner: Box<dyn HidTransport>,
}

#[cfg(any(
    test,
    all(
        feature = "mx-creative-hid",
        target_os = "macos",
        target_arch = "aarch64"
    )
))]
impl HidTransport for ReviewReadOnlyTransport {
    fn supports_write(&self) -> bool {
        false
    }

    fn write(&mut self, _report: &[u8]) -> Result<usize, HidError> {
        Err(HidError::Unavailable(UnavailableReason::UnsupervisedWrite))
    }

    fn read(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, HidError> {
        self.inner.read(buffer, timeout)
    }

    fn report_descriptor(&mut self) -> Result<Vec<u8>, HidError> {
        self.inner.report_descriptor()
    }
}

#[cfg(all(
    any(
        feature = "akp03-hid",
        feature = "micro-hid",
        feature = "mx-creative-hid"
    ),
    windows,
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn native_backend_impl() -> Result<Box<dyn HidBackend>, HidError> {
    Ok(Box::new(win32::WindowsHid::new()?))
}

#[cfg(all(
    any(
        feature = "akp03-hid",
        feature = "micro-hid",
        feature = "mx-creative-hid"
    ),
    target_os = "macos",
    target_arch = "aarch64"
))]
fn native_backend_impl() -> Result<Box<dyn HidBackend>, HidError> {
    Ok(Box::new(iokit::IoKitHid::new()?))
}

#[cfg(not(any(
    all(
        any(
            feature = "akp03-hid",
            feature = "micro-hid",
            feature = "mx-creative-hid"
        ),
        windows,
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(
        any(
            feature = "akp03-hid",
            feature = "micro-hid",
            feature = "mx-creative-hid"
        ),
        target_os = "macos",
        target_arch = "aarch64"
    )
)))]
fn native_backend_impl() -> Result<Box<dyn HidBackend>, HidError> {
    Err(HidError::Unavailable(
        UnavailableReason::UnsupportedPlatform,
    ))
}
