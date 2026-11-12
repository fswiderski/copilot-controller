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
//! The macOS backend reads those only from native properties when available.
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
    any(feature = "akp03-hid", feature = "micro-hid"),
    target_os = "macos",
    target_arch = "aarch64"
))]
mod iokit;
#[cfg(all(
    test,
    any(feature = "akp03-hid", feature = "micro-hid"),
    target_os = "macos",
    target_arch = "aarch64"
))]
pub(crate) use iokit::fixture_output;
#[cfg(all(
    any(feature = "akp03-hid", feature = "micro-hid"),
    windows,
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod win32;
#[cfg(any(
    test,
    all(
        any(feature = "akp03-hid", feature = "micro-hid"),
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
    pub path: HidPath,
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
    /// Neither `akp03-hid` nor `micro-hid` was compiled in.
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

    /// Opens a composite-interface candidate whose primary usage may differ
    /// after reopening. Implementations should preserve every other identity check.
    fn open_allowing_usage_change(
        &self,
        descriptor: &HidDescriptor,
    ) -> Result<Box<dyn HidTransport>, HidError> {
        self.open(descriptor)
    }
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
        validate_descriptor(d)?;
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
        cfg!(any(feature = "akp03-hid", feature = "micro-hid")),
        std::env::consts::OS,
        std::env::consts::ARCH,
    ) {
        PlatformSupport::Supported(_) => native_backend_impl(),
        PlatformSupport::Unavailable(r) => Err(HidError::Unavailable(r)),
    }
}

#[cfg(all(
    any(feature = "akp03-hid", feature = "micro-hid"),
    windows,
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn native_backend_impl() -> Result<Box<dyn HidBackend>, HidError> {
    Ok(Box::new(win32::WindowsHid::new()?))
}

#[cfg(all(
    any(feature = "akp03-hid", feature = "micro-hid"),
    target_os = "macos",
    target_arch = "aarch64"
))]
fn native_backend_impl() -> Result<Box<dyn HidBackend>, HidError> {
    Ok(Box::new(iokit::IoKitHid::new()?))
}

#[cfg(not(any(
    all(
        any(feature = "akp03-hid", feature = "micro-hid"),
        windows,
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(
        any(feature = "akp03-hid", feature = "micro-hid"),
        target_os = "macos",
        target_arch = "aarch64"
    )
)))]
fn native_backend_impl() -> Result<Box<dyn HidBackend>, HidError> {
    Err(HidError::Unavailable(
        UnavailableReason::UnsupportedPlatform,
    ))
}
