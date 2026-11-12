use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::super::artifacts::{
    ArtifactPin, FLASH_SIZE, IMAGE_SIZE, PARTITION_END, PARTITION_START, SETTINGS_SIZE, sha256,
};
use super::super::backend::{
    BackendError, HidCandidate, HidOpened, HidTransport, MaintenanceBackend, RegistryNode,
    RegistryQuery, RegistrySnapshot, ToolInvocation, ToolOperation, ToolOutput, WorkerContext,
    WorkerPlatform,
};
use super::super::identity::DescriptorPin;
use super::super::options::{CONFIRMATION, UpdateOptions};
use super::super::report::UpdateOutcome;
use super::super::transaction::run_synthetic_model;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

fn private_dir(path: &Path) {
    let builder = DirBuilder::new();
    #[cfg(unix)]
    let mut builder = builder;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).unwrap();
}

pub struct Fixture {
    pub root: PathBuf,
    initial_metadata: fs::Metadata,
}

impl Fixture {
    pub fn new() -> Self {
        let base = std::env::var_os("COPILOT_FIRMWARE_FIXTURE_ROOT")
            .expect("COPILOT_FIRMWARE_FIXTURE_ROOT must explicitly name the fixture root");
        let base = PathBuf::from(base);
        assert!(base.is_absolute());
        assert!(fs::symlink_metadata(&base).unwrap().file_type().is_dir());
        let base = fs::canonicalize(base).unwrap();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = base.join(format!(
            "firmware-model-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        private_dir(&root);
        let initial_metadata = fs::symlink_metadata(&root).unwrap();
        Self {
            root,
            initial_metadata,
        }
    }

    pub fn options(&self) -> UpdateOptions {
        UpdateOptions {
            image: self.root.join("synthetic-image.bin"),
            esptool: self.root.join("synthetic-tool.bin"),
            backup_dir: self.root.join("recovery"),
            port: OsString::from("/dev/cu.usbmodemTest123"),
            confirm: CONFIRMATION.to_owned(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let current = fs::symlink_metadata(&self.root).unwrap();
        assert!(current.file_type().is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(current.dev(), self.initial_metadata.dev());
            assert_eq!(current.ino(), self.initial_metadata.ino());
        }
        #[cfg(not(unix))]
        assert_eq!(current.created().ok(), self.initial_metadata.created().ok());
        fs::remove_dir_all(&self.root).unwrap();
    }
}

pub fn write_private(path: &Path, bytes: &[u8]) {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
    }
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

fn change_byte(path: &Path, offset: u64) {
    let mut file = OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&[0x91]).unwrap();
    file.sync_all().unwrap();
}

pub fn image_bytes(size: usize) -> Vec<u8> {
    let mut image = vec![0x31; size];
    image[0] = 0xe9;
    image[PARTITION_START..PARTITION_END].fill(0xa5);
    image
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateMutation {
    Vendor,
    Product,
    UsagePage,
    Usage,
    Transport,
    Interface,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenedMutation {
    Vendor,
    Product,
    Interface,
    Transport,
    Path,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Fault {
    None,
    FeatureDisabled,
    ContextError,
    UnsupportedPlatform,
    Unsupervised,
    Lease,
    Umask,
    UnsafeParent,
    ExistingBackup,
    #[cfg(unix)]
    ExistingBackupSymlink,
    ImageHash,
    ToolHash,
    ImageHeader,
    ImageExtent,
    CancelBeforeContext,
    ToolExit(ToolOperation),
    ToolTimeout(ToolOperation),
    ToolOverflow(ToolOperation),
    ToolStderrOverflow(ToolOperation),
    ToolCancelled(ToolOperation),
    ToolVersion,
    ToolNonUtf8,
    ToolTempEvidence,
    InitialRegistryError,
    InitialRegistryAbsent,
    DuplicateApplication,
    ZeroLocation,
    RegistryTooDeep,
    EnumerationError,
    NoHid,
    DuplicateHid,
    Candidate(CandidateMutation),
    OpenError,
    OpenedIdentity,
    Opened(OpenedMutation),
    Descriptor,
    DescriptorLength,
    DescriptorTooLong,
    VersionWriteShort,
    VersionReadError,
    StalledVersionClock,
    VersionFloatId,
    VersionStringId,
    VersionErrorKey,
    VersionMalformed,
    VersionBadFrame,
    VersionNonObject,
    TooManyReports,
    StartVersion(&'static str),
    LocationChanged,
    BootWriteShort,
    BootWriteError,
    BootCloseError,
    WrongBootPid,
    WrongBootVid,
    WrongBootLocation,
    WrongBootPort,
    DuplicateBootPort,
    StalledClock,
    BindingLostBefore(ToolOperation),
    TamperToolAfter(ToolOperation),
    TamperImageAfter(ToolOperation),
    TamperToolDirectoryAfter(ToolOperation),
    #[cfg(unix)]
    StagedToolSymlink,
    SecurityMissing,
    SecurityContradictory,
    FlashSizeMissing,
    FlashSizeWrong,
    FlashSizeDuplicate,
    BackupMissing,
    BackupShort,
    #[cfg(unix)]
    BackupPermissions,
    #[cfg(unix)]
    BackupSymlink,
    PartitionMismatch,
    ChecksumConflict,
    BackupChangedAfterVerify,
    ImageChangedAfterVerify,
    BackupChangedBeforeWrite,
    ChecksumChangedBeforeWrite,
    BackupChangedBeforeReset,
    BackupChangedAfterReset,
    SettingsMissing,
    SettingsShort,
    SettingsMismatch,
    #[cfg(unix)]
    SettingsSymlink,
    FinalAbsent,
    FinalLocation,
    FinalLocationAfterQuery,
    FinalDuplicate,
    FinalOpen,
    FinalDescriptor,
    FinalReadError,
    FinalVersion(&'static str),
    FinalClose,
    CancelAfterFinalClose,
    CandidateLocation(Option<u64>, bool),
    OpenedLocation(Option<u64>, bool),
    SyncFailure(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UsbState {
    Application,
    Bootloader,
    FinalApplication,
}

pub struct FakeBackend {
    pub fault: Fault,
    pub calls: Vec<ToolInvocation>,
    pub hid_reports: Vec<[u8; 64]>,
    pub queries: Vec<RegistryQuery>,
    pub events: Vec<&'static str>,
    pub close_count: usize,
    pub open_count: usize,
    pub clock: Duration,
    pub sync_count: usize,
    state: UsbState,
    app_queries: usize,
    boot_queries: usize,
    final_queries: usize,
    pending: VecDeque<Vec<u8>>,
    descriptor: Vec<u8>,
    final_tampered: bool,
}

impl FakeBackend {
    pub fn new(fault: Fault, descriptor: Vec<u8>) -> Self {
        Self {
            fault,
            calls: Vec::new(),
            hid_reports: Vec::new(),
            queries: Vec::new(),
            events: Vec::new(),
            close_count: 0,
            open_count: 0,
            clock: Duration::ZERO,
            sync_count: 0,
            state: UsbState::Application,
            app_queries: 0,
            boot_queries: 0,
            final_queries: 0,
            pending: VecDeque::new(),
            descriptor,
            final_tampered: false,
        }
    }

    pub fn operations(&self) -> Vec<ToolOperation> {
        self.calls.iter().map(|call| call.operation).collect()
    }

    fn next_operation(&self) -> Option<ToolOperation> {
        match self.calls.last().map(|call| call.operation) {
            Some(ToolOperation::Version) => Some(ToolOperation::Security),
            Some(ToolOperation::Security) => Some(ToolOperation::FlashId),
            Some(ToolOperation::FlashId) => Some(ToolOperation::ReadBackup),
            Some(ToolOperation::ReadBackup) => Some(ToolOperation::VerifyBackup),
            Some(ToolOperation::VerifyBackup) => Some(ToolOperation::WriteImage),
            Some(ToolOperation::WriteImage) => Some(ToolOperation::VerifyImage),
            Some(ToolOperation::VerifyImage) => Some(ToolOperation::ReadSettings),
            Some(ToolOperation::ReadSettings) => Some(ToolOperation::Reset),
            _ => None,
        }
    }

    fn application(location: u64) -> RegistryNode {
        RegistryNode {
            vendor_id: Some(0x303a),
            product_id: Some(0x8298),
            location_id: Some(location),
            ..RegistryNode::default()
        }
    }

    fn response(&mut self, final_query: bool) {
        let json = if !final_query {
            match self.fault {
                Fault::VersionFloatId => r#"{"id":101.0,"result":{"version":"0.6.3"}}"#.to_owned(),
                Fault::VersionStringId => r#"{"id":"101","result":{"version":"0.6.3"}}"#.to_owned(),
                Fault::VersionErrorKey => {
                    r#"{"id":101,"error":null,"result":{"version":"0.6.3"}}"#.to_owned()
                }
                Fault::VersionMalformed => r#"{"id":101,"result":{"version":3}}"#.to_owned(),
                Fault::VersionNonObject => r#"[1]"#.to_owned(),
                Fault::StartVersion(version) => {
                    format!(r#"{{"id":101,"result":{{"version":"{version}"}}}}"#)
                }
                _ => r#"{"id":101,"result":{"version":"v0.1.50"}}"#.to_owned(),
            }
        } else {
            let version = match self.fault {
                Fault::FinalVersion(version) => version,
                _ => "0.6.3",
            };
            format!(r#"{{"id":101,"result":{{"version":"{version}"}}}}"#)
        };
        if !final_query && self.fault == Fault::VersionBadFrame {
            let mut packet = vec![0_u8; 64];
            packet[0] = 6;
            packet[1] = 2;
            packet[2] = 62;
            self.pending.push_back(packet);
        } else if !final_query && self.fault == Fault::TooManyReports {
            for _ in 0..128 {
                self.pending.push_back(vec![1]);
            }
        } else {
            self.pending.push_back(vec![1]);
            for chunk in json.as_bytes().chunks(61) {
                let mut packet = vec![0_u8; 64];
                packet[0] = 6;
                packet[1] = 2;
                packet[2] = chunk.len() as u8;
                packet[3..3 + chunk.len()].copy_from_slice(chunk);
                self.pending.push_back(packet);
            }
        }
    }
}

impl MaintenanceBackend for FakeBackend {
    type Lease = ();

    fn worker_context(&mut self) -> Result<WorkerContext, BackendError> {
        self.events.push("context");
        if self.fault == Fault::ContextError {
            return Err(BackendError::Failure("context unavailable".to_owned()));
        }
        Ok(WorkerContext {
            platform: if self.fault == Fault::UnsupportedPlatform {
                WorkerPlatform::Unsupported
            } else {
                WorkerPlatform::MacosAarch64
            },
            process_id: 17,
            process_group_id: if self.fault == Fault::Unsupervised {
                16
            } else {
                17
            },
        })
    }

    fn acquire_lease(&mut self) -> Result<Self::Lease, BackendError> {
        self.events.push("lease");
        if self.fault == Fault::Lease {
            Err(BackendError::Failure("lease unavailable".to_owned()))
        } else {
            Ok(())
        }
    }

    fn set_private_umask(&mut self, mask: u32) -> Result<(), BackendError> {
        self.events.push("umask");
        assert_eq!(mask, 0o077);
        if self.fault == Fault::Umask {
            Err(BackendError::Failure("umask unavailable".to_owned()))
        } else {
            Ok(())
        }
    }

    fn authorize_backup_path(&mut self, _path: &Path) -> Result<(), BackendError> {
        self.events.push("authorize synthetic workspace");
        if self.fault == Fault::UnsafeParent {
            Err(BackendError::Failure("unsafe parent".to_owned()))
        } else {
            Ok(())
        }
    }

    fn full_sync(&mut self, file: &File) -> Result<(), BackendError> {
        self.sync_count += 1;
        self.events.push("synthetic full sync");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let workspace = self.calls[0].program.parent().unwrap();
            let expected = match self.sync_count {
                1 => workspace.join("full-flash.bin"),
                2 => workspace.join("SHA256SUMS"),
                3 => workspace.to_owned(),
                4 => workspace.parent().unwrap().to_owned(),
                _ => panic!("unexpected sync"),
            };
            let actual = file.metadata().unwrap();
            let expected = fs::metadata(expected).unwrap();
            assert_eq!(
                (actual.dev(), actual.ino()),
                (expected.dev(), expected.ino())
            );
        }
        if self.fault == Fault::SyncFailure(self.sync_count) {
            return Err(BackendError::Failure("synthetic sync failed".into()));
        }
        file.sync_all()
            .map_err(|error| BackendError::Failure(error.to_string()))
    }

    fn check_cancelled(&mut self) -> Result<(), BackendError> {
        if self.fault == Fault::CancelBeforeContext
            || (self.fault == Fault::CancelAfterFinalClose && self.close_count == 2)
        {
            Err(BackendError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn monotonic_now(&mut self) -> Result<Duration, BackendError> {
        Ok(self.clock)
    }

    fn sleep(&mut self, duration: Duration) -> Result<(), BackendError> {
        if self.fault != Fault::StalledClock && self.fault != Fault::StalledVersionClock {
            self.clock += duration;
        }
        Ok(())
    }

    fn registry(&mut self, query: &RegistryQuery) -> Result<RegistrySnapshot, BackendError> {
        assert!(query.timeout <= Duration::from_secs(5));
        assert_eq!(query.stdout_limit, 4_194_304);
        assert_eq!(query.stderr_limit, 4_194_304);
        assert!(query.clear_environment && query.require_top_level_array);
        self.queries.push(query.clone());
        if self.state == UsbState::Application {
            self.app_queries += 1;
            if self.fault == Fault::InitialRegistryError {
                return Err(BackendError::Timeout);
            }
            if self.fault == Fault::InitialRegistryAbsent {
                return Ok(RegistrySnapshot::default());
            }
            if self.fault == Fault::RegistryTooDeep {
                let mut node = Self::application(123);
                for _ in 0..33 {
                    node = RegistryNode {
                        children: vec![node],
                        ..RegistryNode::default()
                    };
                }
                return Ok(RegistrySnapshot { roots: vec![node] });
            }
            let location = if self.fault == Fault::ZeroLocation {
                0
            } else if self.fault == Fault::LocationChanged && self.app_queries == 2 {
                124
            } else {
                123
            };
            let app = Self::application(location);
            let mut roots = vec![app.clone()];
            if self.fault == Fault::DuplicateApplication {
                roots.push(app);
            }
            return Ok(RegistrySnapshot { roots });
        }
        if self.state == UsbState::FinalApplication {
            self.final_queries += 1;
            if self.fault == Fault::BackupChangedAfterReset && !self.final_tampered {
                let workspace = self.calls[0].program.parent().unwrap();
                change_byte(&workspace.join("full-flash.bin"), 0x100);
                self.final_tampered = true;
            }
            if self.fault == Fault::FinalAbsent {
                return Ok(RegistrySnapshot::default());
            }
            let app = Self::application(
                if self.fault == Fault::FinalLocation
                    || (self.fault == Fault::FinalLocationAfterQuery && self.final_queries == 2)
                {
                    124
                } else {
                    123
                },
            );
            let mut roots = vec![app.clone()];
            if self.fault == Fault::FinalDuplicate {
                roots.push(app);
            }
            return Ok(RegistrySnapshot { roots });
        }
        self.boot_queries += 1;
        if self.boot_queries > 1 {
            let next = self.next_operation();
            let workspace = self.calls[0].program.parent().unwrap();
            if (self.fault == Fault::BackupChangedBeforeWrite
                && next == Some(ToolOperation::WriteImage))
                || (self.fault == Fault::BackupChangedBeforeReset
                    && next == Some(ToolOperation::Reset))
            {
                change_byte(&workspace.join("full-flash.bin"), 0x100);
            }
            if self.fault == Fault::ChecksumChangedBeforeWrite
                && next == Some(ToolOperation::WriteImage)
            {
                change_byte(&workspace.join("SHA256SUMS"), 0);
            }
        }
        let lost = self.fault == Fault::StalledClock
            || matches!(self.fault, Fault::BindingLostBefore(operation)
                if self.boot_queries > 1 && self.next_operation() == Some(operation));
        let boot = RegistryNode {
            vendor_id: Some(if self.fault == Fault::WrongBootVid {
                0x303b
            } else {
                0x303a
            }),
            product_id: Some(if self.fault == Fault::WrongBootPid || lost {
                0x1002
            } else {
                0x1001
            }),
            location_id: Some(if self.fault == Fault::WrongBootLocation {
                124
            } else {
                123
            }),
            children: vec![RegistryNode {
                callout_device: Some(
                    if self.fault == Fault::WrongBootPort {
                        "/dev/cu.usbmodemWrong"
                    } else {
                        "/dev/cu.usbmodemTest123"
                    }
                    .to_owned(),
                ),
                ..RegistryNode::default()
            }],
            ..RegistryNode::default()
        };
        let mut roots = vec![boot.clone()];
        if self.fault == Fault::DuplicateBootPort {
            roots.push(boot);
        }
        Ok(RegistrySnapshot { roots })
    }

    fn enumerate_hid(&mut self) -> Result<Vec<HidCandidate>, BackendError> {
        self.events.push("enumerate HID");
        if self.fault == Fault::EnumerationError && self.state == UsbState::Application {
            return Err(BackendError::Failure("enumeration failed".to_owned()));
        }
        if self.fault == Fault::NoHid {
            return Ok(Vec::new());
        }
        let mut candidate = HidCandidate {
            vendor_id: 0x303a,
            product_id: 0x8298,
            usage_page: 0xff00,
            usage: 1,
            interface_number: 2,
            transport: HidTransport::Usb,
            path: OsString::from("synthetic-hid-identity"),
            location_id: Some(123),
        };
        if let Fault::CandidateLocation(location, final_phase) = self.fault
            && (self.state == UsbState::FinalApplication) == final_phase
        {
            candidate.location_id = location;
        }
        match self.fault {
            Fault::Candidate(CandidateMutation::Vendor) => candidate.vendor_id ^= 1,
            Fault::Candidate(CandidateMutation::Product) => candidate.product_id ^= 1,
            Fault::Candidate(CandidateMutation::UsagePage) => candidate.usage_page ^= 1,
            Fault::Candidate(CandidateMutation::Usage) => candidate.usage ^= 1,
            Fault::Candidate(CandidateMutation::Transport) => {
                candidate.transport = HidTransport::Bluetooth
            }
            Fault::Candidate(CandidateMutation::Interface) => candidate.interface_number = 99,
            _ => {}
        }
        let mut candidates = vec![candidate.clone()];
        if self.fault == Fault::DuplicateHid {
            candidates.push(candidate);
        }
        Ok(candidates)
    }

    fn open_hid(&mut self, candidate: &HidCandidate) -> Result<HidOpened, BackendError> {
        self.open_count += 1;
        if (self.fault == Fault::OpenError && self.open_count == 1)
            || (self.fault == Fault::FinalOpen && self.open_count == 2)
        {
            return Err(BackendError::Failure("synthetic open failed".to_owned()));
        }
        let mut descriptor = self.descriptor.clone();
        if (self.fault == Fault::Descriptor && self.open_count == 1)
            || (self.fault == Fault::FinalDescriptor && self.open_count == 2)
        {
            descriptor[0] ^= 1;
        }
        if self.fault == Fault::DescriptorLength && self.open_count == 1 {
            descriptor.push(0x61);
        }
        if self.fault == Fault::DescriptorTooLong && self.open_count == 1 {
            descriptor.resize(1025, 0x61);
        }
        let mut opened = HidOpened {
            vendor_id: candidate.vendor_id,
            product_id: candidate.product_id,
            interface_number: candidate.interface_number,
            transport: candidate.transport,
            path: if self.fault == Fault::OpenedIdentity && self.open_count == 1 {
                OsString::from("different-synthetic-path")
            } else {
                candidate.path.clone()
            },
            report_descriptor: descriptor,
            location_id: candidate.location_id,
        };
        if let Fault::OpenedLocation(location, final_phase) = self.fault
            && (self.state == UsbState::FinalApplication) == final_phase
        {
            opened.location_id = location;
        }
        match self.fault {
            Fault::Opened(OpenedMutation::Vendor) => opened.vendor_id ^= 1,
            Fault::Opened(OpenedMutation::Product) => opened.product_id ^= 1,
            Fault::Opened(OpenedMutation::Interface) => opened.interface_number ^= 1,
            Fault::Opened(OpenedMutation::Transport) => opened.transport = HidTransport::Unknown,
            Fault::Opened(OpenedMutation::Path) => opened.path = "other-synthetic-path".into(),
            _ => {}
        }
        Ok(opened)
    }

    fn write_hid(&mut self, report: &[u8; 64]) -> Result<usize, BackendError> {
        self.hid_reports.push(*report);
        let payload = &report[3..3 + usize::from(report[2])];
        if payload == br#"{"method":"sys.version","params":null,"id":101}"# {
            if self.fault == Fault::VersionWriteShort && self.open_count == 1 {
                return Ok(12);
            }
            if self.fault != Fault::StalledVersionClock {
                self.response(self.open_count == 2);
            }
            Ok(64)
        } else {
            assert_eq!(
                payload,
                br#"{"method":"sys.bootloader","params":null,"id":102}"#
            );
            self.state = UsbState::Bootloader;
            if self.fault == Fault::BootWriteError {
                return Err(BackendError::Failure("bootloader write failed".to_owned()));
            }
            if self.fault == Fault::BootWriteShort {
                return Ok(12);
            }
            Ok(64)
        }
    }

    fn read_hid(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, BackendError> {
        assert!(timeout <= Duration::from_millis(50));
        if (self.fault == Fault::VersionReadError && self.open_count == 1)
            || (self.fault == Fault::FinalReadError && self.open_count == 2)
        {
            return Err(BackendError::Timeout);
        }
        if let Some(packet) = self.pending.pop_front() {
            Ok(Some(packet))
        } else {
            if self.fault != Fault::StalledVersionClock {
                self.clock += timeout;
            }
            Ok(None)
        }
    }

    fn close_hid(&mut self) -> Result<(), BackendError> {
        self.close_count += 1;
        if (self.fault == Fault::BootCloseError && self.close_count == 1)
            || (self.fault == Fault::FinalClose && self.close_count == 2)
        {
            return Err(BackendError::Failure("synthetic close failed".to_owned()));
        }
        Ok(())
    }

    fn invoke_tool(&mut self, invocation: &ToolInvocation) -> Result<ToolOutput, BackendError> {
        self.calls.push(invocation.clone());
        let operation = invocation.operation;
        let workspace = invocation.program.parent().unwrap();
        let backup_path = workspace.join("full-flash.bin");
        if operation == ToolOperation::Version && self.fault == Fault::ToolTempEvidence {
            write_private(
                &workspace.join("tool-tmp/synthetic-evidence"),
                b"retain nonempty staging evidence",
            );
            return Ok(ToolOutput {
                exit_code: 1,
                stdout: b"synthetic version failure\n".to_vec(),
                stderr: Vec::new(),
            });
        }
        #[cfg(unix)]
        if (operation == ToolOperation::ReadBackup && self.fault == Fault::BackupSymlink)
            || (operation == ToolOperation::ReadSettings && self.fault == Fault::SettingsSymlink)
        {
            use std::os::unix::fs::symlink;
            let link = if operation == ToolOperation::ReadBackup {
                backup_path.clone()
            } else {
                workspace.join("settings-after.bin")
            };
            symlink(workspace.join("firmware.bin"), link).unwrap();
            return Ok(ToolOutput {
                exit_code: 0,
                stdout: b"synthetic symlink output\n".to_vec(),
                stderr: Vec::new(),
            });
        }
        if matches!(self.fault, Fault::ToolExit(op) | Fault::ToolTimeout(op)
            | Fault::ToolOverflow(op) | Fault::ToolStderrOverflow(op)
            | Fault::ToolCancelled(op) if op == operation)
        {
            if operation == ToolOperation::ReadBackup {
                write_private(&backup_path, b"partial synthetic recovery evidence");
            }
            if operation == ToolOperation::ReadSettings {
                write_private(
                    &workspace.join("settings-after.bin"),
                    b"partial synthetic settings evidence",
                );
            }
            return match self.fault {
                Fault::ToolTimeout(_) => Err(BackendError::Timeout),
                Fault::ToolCancelled(_) => Err(BackendError::Cancelled),
                Fault::ToolOverflow(_) => Ok(ToolOutput {
                    exit_code: 0,
                    stdout: vec![b'x'; 4_194_305],
                    stderr: Vec::new(),
                }),
                Fault::ToolStderrOverflow(_) => Ok(ToolOutput {
                    exit_code: 0,
                    stdout: Vec::new(),
                    stderr: vec![b'x'; 4_194_305],
                }),
                _ => Ok(ToolOutput {
                    exit_code: 1,
                    stdout: b"synthetic stdout\n".to_vec(),
                    stderr: b"synthetic stderr\n".to_vec(),
                }),
            };
        }

        let mut stdout = match operation {
            ToolOperation::Version => {
                if self.fault == Fault::ToolVersion {
                    b"esptool v5.6.1\n".to_vec()
                } else if self.fault == Fault::ToolNonUtf8 {
                    vec![0xff]
                } else {
                    b"esptool v5.6.0\n".to_vec()
                }
            }
            ToolOperation::Security => {
                if self.fault == Fault::SecurityMissing {
                    b"Secure Boot: Disabled\n".to_vec()
                } else if self.fault == Fault::SecurityContradictory {
                    b"Secure Boot: Disabled\nFlash Encryption: Disabled\nFlags: 0x00000000 (0b0)\nSecure Boot: Enabled\n".to_vec()
                } else {
                    b"Secure Boot: Disabled\nFlash Encryption: Disabled\nFlags: 0x00000000 (0b0)\n"
                        .to_vec()
                }
            }
            ToolOperation::FlashId => match self.fault {
                Fault::FlashSizeMissing => b"no size\n".to_vec(),
                Fault::FlashSizeWrong => b"Detected flash size: 8MB\n".to_vec(),
                Fault::FlashSizeDuplicate => {
                    b"Detected flash size: 16MB\nDetected flash size: 16MB\n".to_vec()
                }
                _ => b"Detected flash size: 16MB\n".to_vec(),
            },
            ToolOperation::ReadBackup => {
                if self.fault != Fault::BackupMissing {
                    let length = if self.fault == Fault::BackupShort {
                        FLASH_SIZE as usize - 1
                    } else {
                        FLASH_SIZE as usize
                    };
                    let mut backup = vec![0x5a; length];
                    if self.fault != Fault::PartitionMismatch {
                        backup[PARTITION_START..PARTITION_END].fill(0xa5);
                    }
                    write_private(&backup_path, &backup);
                    #[cfg(unix)]
                    if self.fault == Fault::BackupPermissions {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&backup_path, fs::Permissions::from_mode(0o400))
                            .unwrap();
                    }
                    if self.fault == Fault::ChecksumConflict {
                        write_private(&workspace.join("SHA256SUMS"), b"existing evidence\n");
                    }
                }
                Vec::new()
            }
            ToolOperation::VerifyBackup => {
                if self.fault == Fault::BackupChangedAfterVerify {
                    change_byte(&backup_path, 0x100);
                }
                if self.fault == Fault::ImageChangedAfterVerify {
                    change_byte(&workspace.join("firmware.bin"), 0x100);
                }
                Vec::new()
            }
            ToolOperation::ReadSettings => {
                if self.fault != Fault::SettingsMissing {
                    let length = if self.fault == Fault::SettingsShort {
                        SETTINGS_SIZE - 1
                    } else {
                        SETTINGS_SIZE
                    };
                    let mut settings = vec![0x5a; length];
                    if self.fault == Fault::SettingsMismatch {
                        settings[0] ^= 1;
                    }
                    write_private(&workspace.join("settings-after.bin"), &settings);
                }
                Vec::new()
            }
            ToolOperation::Reset => {
                self.state = UsbState::FinalApplication;
                Vec::new()
            }
            ToolOperation::WriteImage | ToolOperation::VerifyImage => Vec::new(),
        };
        if self.fault == Fault::TamperToolAfter(operation) {
            change_byte(&workspace.join("esptool"), 0);
        }
        #[cfg(unix)]
        if self.fault == Fault::StagedToolSymlink && operation == ToolOperation::Version {
            use std::os::unix::fs::symlink;
            let staged = workspace.join("esptool");
            fs::remove_file(&staged).unwrap();
            symlink(
                workspace.parent().unwrap().join("synthetic-tool.bin"),
                staged,
            )
            .unwrap();
        }
        if self.fault == Fault::TamperImageAfter(operation) {
            change_byte(&workspace.join("firmware.bin"), 0x100);
        }
        if self.fault == Fault::TamperToolDirectoryAfter(operation) {
            let path = workspace.join("tool-tmp");
            fs::remove_dir(&path).unwrap();
            private_dir(&path);
        }
        if operation != ToolOperation::Version {
            stdout.extend_from_slice(b"synthetic operation complete\n");
        }
        Ok(ToolOutput {
            exit_code: 0,
            stdout,
            stderr: if operation == ToolOperation::Version {
                b"synthetic stderr\n".to_vec()
            } else {
                Vec::new()
            },
        })
    }
}

pub struct Run {
    pub fixture: Fixture,
    pub options: UpdateOptions,
    pub backend: FakeBackend,
    pub outcome: UpdateOutcome,
}

pub fn run(fault: Fault, full_image: bool) -> Run {
    let fixture = Fixture::new();
    let options = fixture.options();
    let size = if fault == Fault::ImageExtent {
        0x810001
    } else if full_image {
        IMAGE_SIZE as usize
    } else {
        PARTITION_END + 19
    };
    let mut image = image_bytes(size);
    if fault == Fault::ImageHeader {
        image[0] = 0;
    }
    let tool = vec![0x47; 256];
    let descriptor = vec![0x61; 36];
    let image_hash = sha256(&image);
    let tool_hash = sha256(&tool);
    let descriptor_hash = sha256(&descriptor);
    write_private(&options.image, &image);
    write_private(&options.esptool, &tool);
    if fault == Fault::ImageHash {
        change_byte(&options.image, 0x100);
    }
    if fault == Fault::ToolHash {
        change_byte(&options.esptool, 0);
    }
    if fault == Fault::ExistingBackup {
        private_dir(&options.backup_dir);
        write_private(
            &options.backup_dir.join("existing-evidence"),
            b"do not replace",
        );
    }
    #[cfg(unix)]
    if fault == Fault::ExistingBackupSymlink {
        use std::os::unix::fs::symlink;
        let target = fixture.root.join("protected-existing");
        private_dir(&target);
        write_private(&target.join("existing-evidence"), b"do not replace");
        symlink(target, &options.backup_dir).unwrap();
    }
    let descriptors = [DescriptorPin {
        interface: 2,
        length: descriptor.len(),
        sha256: &descriptor_hash,
    }];
    let enabled = fault != Fault::FeatureDisabled;
    let image_pin = ArtifactPin {
        size: image.len() as u64,
        sha256: &image_hash,
    };
    let tool_pin = ArtifactPin {
        size: tool.len() as u64,
        sha256: &tool_hash,
    };
    let mut backend = FakeBackend::new(fault, descriptor);
    let outcome = run_synthetic_model(
        &options,
        &mut backend,
        enabled,
        image_pin,
        tool_pin,
        &descriptors,
    );
    Run {
        fixture,
        options,
        backend,
        outcome,
    }
}
