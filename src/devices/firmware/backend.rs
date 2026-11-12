use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendError {
    Cancelled,
    Timeout,
    Failure(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("maintenance cancelled"),
            Self::Timeout => formatter.write_str("operation timed out"),
            Self::Failure(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for BackendError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerPlatform {
    MacosAarch64,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerContext {
    pub platform: WorkerPlatform,
    pub process_id: u32,
    pub process_group_id: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HidTransport {
    Usb,
    Bluetooth,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HidCandidate {
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub interface_number: i32,
    pub transport: HidTransport,
    pub path: OsString,
    /// Physical USB host-device location, obtained from registry ancestry.
    pub location_id: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HidOpened {
    pub vendor_id: u16,
    pub product_id: u16,
    pub interface_number: i32,
    pub transport: HidTransport,
    pub path: OsString,
    /// Independently obtained from the opened HID's registry ancestry.
    pub location_id: Option<u64>,
    pub report_descriptor: Vec<u8>,
}

/// A parsed registry dictionary. Native parsing must reject invalid property types.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RegistryNode {
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub location_id: Option<u64>,
    pub callout_device: Option<String>,
    pub children: Vec<RegistryNode>,
}

/// A parsed top-level registry array; parsing and bounded collection are backend duties.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RegistrySnapshot {
    pub roots: Vec<RegistryNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryQuery {
    pub timeout: Duration,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
    pub clear_environment: bool,
    pub require_top_level_array: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolOperation {
    Version,
    Security,
    FlashId,
    ReadBackup,
    VerifyBackup,
    WriteImage,
    VerifyImage,
    ReadSettings,
    Reset,
}

impl ToolOperation {
    pub(crate) fn log_name(self) -> &'static str {
        match self {
            Self::Version => "Version.log",
            Self::Security => "Security.log",
            Self::FlashId => "FlashId.log",
            Self::ReadBackup => "ReadBackup.log",
            Self::VerifyBackup => "VerifyBackup.log",
            Self::WriteImage => "WriteImage.log",
            Self::VerifyImage => "VerifyImage.log",
            Self::ReadSettings => "ReadSettings.log",
            Self::Reset => "Reset.log",
        }
    }
}

/// Declarative operation only. Constructing it never starts a process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolInvocation {
    pub operation: ToolOperation,
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
    pub clear_environment: bool,
    pub null_stdin: bool,
    pub timeout: Duration,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Injectable model boundary, not a native maintenance implementation.
///
/// Any future real backend must enforce bounded/cancellable operations, retain
/// the shared lease, validate the approved safe APFS parent and its ancestors,
/// resist filesystem substitution, qualify native HID/registry parsing, and
/// implement the process-group supervisor before it can be connected to a CLI.
/// A backend is trusted to perform only the operation requested and to report
/// its result honestly; a fake backend proves no physical-device property.
pub trait MaintenanceBackend {
    type Lease;

    fn worker_context(&mut self) -> Result<WorkerContext, BackendError>;
    fn acquire_lease(&mut self) -> Result<Self::Lease, BackendError>;
    fn set_private_umask(&mut self, mask: u32) -> Result<(), BackendError>;
    fn authorize_backup_path(&mut self, path: &Path) -> Result<(), BackendError>;
    /// Flush this open file or directory through to permanent storage. On macOS
    /// this requires F_FULLFSYNC, including for directories; never fall back to
    /// fsync if unsupported or unsuccessful. A synthetic backend is not proof
    /// of durability. The native worker must supply a real implementation.
    fn full_sync(&mut self, file: &File) -> Result<(), BackendError>;
    fn check_cancelled(&mut self) -> Result<(), BackendError>;
    fn monotonic_now(&mut self) -> Result<Duration, BackendError>;
    fn sleep(&mut self, duration: Duration) -> Result<(), BackendError>;

    fn registry(&mut self, query: &RegistryQuery) -> Result<RegistrySnapshot, BackendError>;
    fn enumerate_hid(&mut self) -> Result<Vec<HidCandidate>, BackendError>;
    fn open_hid(&mut self, candidate: &HidCandidate) -> Result<HidOpened, BackendError>;
    fn write_hid(&mut self, report: &[u8; 64]) -> Result<usize, BackendError>;
    fn read_hid(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, BackendError>;
    fn close_hid(&mut self) -> Result<(), BackendError>;

    /// Honor the exact invocation and output limits; do not retry an operation.
    fn invoke_tool(&mut self, invocation: &ToolInvocation) -> Result<ToolOutput, BackendError>;
}
