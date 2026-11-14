use std::{
    ffi::{CStr, CString, c_char, c_void},
    fs, io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use super::{
    Evidence, Options, Phase, PhaseProgress, ProbeErrorKind, ProbeFailure, ProbeReport, Progress,
    ReadResult, SerialDiagnostics, SerialSettings, Stage, Transport, TransportError,
    run_with_evidence,
};

const USB_VENDOR_ID: i32 = 0x2ec2;
const USB_PRODUCT_ID: i32 = 0x0003;
const DATA_CLASS: i32 = 10;
const DATA_SUBCLASS: i32 = 0;
const DATA_INTERFACE_NUMBER: i32 = 1;
const CONTROL_CLASS: i32 = 2;
const CONTROL_SUBCLASS: i32 = 2;
const CONTROL_PROTOCOL: i32 = 1;
const CONTROL_INTERFACE_NUMBER: i32 = 0;
const IOSSIOSPEED: libc::c_ulong = 0x8008_5402;

#[cfg(test)]
const IOSSIOSPEED_GROUP: u8 = b'T';
#[cfg(test)]
const IOSSIOSPEED_NUMBER: u8 = 2;

const LOCK_FILE_NAME: &str = "copilot-controller-ct-serial.lock";

type CFTypeRef = *const c_void;
type CFMutableDictionaryRef = *mut c_void;
type CFIndex = isize;
type KernReturn = i32;
type IoObject = u32;
type IoIterator = u32;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFAllocatorDefault: CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetValue(number: CFTypeRef, kind: CFIndex, out: *mut c_void) -> u8;
    fn CFStringGetCString(string: CFTypeRef, buf: *mut c_char, len: CFIndex, enc: u32) -> u8;
    fn CFStringCreateWithCString(
        allocator: CFTypeRef,
        string: *const c_char,
        encoding: u32,
    ) -> CFTypeRef;
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingServices(
        master_port: u32,
        matching: CFMutableDictionaryRef,
        existing: *mut IoIterator,
    ) -> KernReturn;
    fn IOIteratorNext(iterator: IoIterator) -> IoObject;
    fn IOObjectRelease(object: IoObject) -> KernReturn;
    fn IOObjectConformsTo(object: IoObject, class_name: *const c_char) -> bool;
    fn IORegistryEntryGetRegistryEntryID(entry: IoObject, id: *mut u64) -> KernReturn;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: CFTypeRef,
        allocator: CFTypeRef,
        options: u32,
    ) -> CFTypeRef;
    fn IORegistryEntryGetParentEntry(
        entry: IoObject,
        plane: *const c_char,
        parent: *mut IoObject,
    ) -> KernReturn;
}

struct Cf(CFTypeRef);

impl Cf {
    fn new(value: CFTypeRef) -> Option<Self> {
        (!value.is_null()).then_some(Self(value))
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

struct IoOwned(IoObject);

impl IoOwned {
    fn new(value: IoObject) -> Option<Self> {
        (value != 0).then_some(Self(value))
    }
}

impl Drop for IoOwned {
    fn drop(&mut self) {
        let _ = unsafe { IOObjectRelease(self.0) };
    }
}

struct IoIter(IoIterator);

impl IoIter {
    fn for_class(class_name: &str) -> Result<Self, ProbeErrorKind> {
        let class_name =
            CString::new(class_name).map_err(|_| ProbeErrorKind::DiscoveryIncomplete)?;
        let matching = unsafe { IOServiceMatching(class_name.as_ptr()) };
        if matching.is_null() {
            return Err(ProbeErrorKind::DiscoveryIncomplete);
        }
        let mut iterator = 0;
        if unsafe { IOServiceGetMatchingServices(0, matching, &mut iterator) } != 0 || iterator == 0
        {
            return Err(ProbeErrorKind::DiscoveryIncomplete);
        }
        Ok(Self(iterator))
    }

    fn next(&mut self) -> Option<IoOwned> {
        IoOwned::new(unsafe { IOIteratorNext(self.0) })
    }
}

impl Drop for IoIter {
    fn drop(&mut self) {
        let _ = unsafe { IOObjectRelease(self.0) };
    }
}

fn cf_string(value: &str) -> Result<Cf, ProbeErrorKind> {
    let key = CString::new(value).map_err(|_| ProbeErrorKind::DiscoveryIncomplete)?;
    let created =
        unsafe { CFStringCreateWithCString(kCFAllocatorDefault, key.as_ptr(), 0x0800_0100) };
    Cf::new(created).ok_or(ProbeErrorKind::DiscoveryIncomplete)
}

fn registry_id(entry: IoObject) -> Option<u64> {
    let mut id = 0;
    (unsafe { IORegistryEntryGetRegistryEntryID(entry, &mut id) } == 0).then_some(id)
}

fn parent_entry(entry: IoObject) -> Option<IoOwned> {
    let mut parent = 0;
    let plane = c"IOService";
    (unsafe { IORegistryEntryGetParentEntry(entry, plane.as_ptr(), &mut parent) } == 0)
        .then(|| IoOwned::new(parent))
        .flatten()
}

fn property(entry: IoObject, key: &str) -> Option<Cf> {
    let key = cf_string(key).ok()?;
    let value = unsafe { IORegistryEntryCreateCFProperty(entry, key.0, kCFAllocatorDefault, 0) };
    Cf::new(value)
}

fn property_i32(entry: IoObject, key: &str) -> Option<i32> {
    let value = property(entry, key)?;
    if unsafe { CFGetTypeID(value.0) } != unsafe { CFNumberGetTypeID() } {
        return None;
    }
    let mut output = 0i32;
    (unsafe { CFNumberGetValue(value.0, 3, (&raw mut output).cast()) } != 0).then_some(output)
}

fn property_string(entry: IoObject, key: &str) -> Option<String> {
    let value = property(entry, key)?;
    if unsafe { CFGetTypeID(value.0) } != unsafe { CFStringGetTypeID() } {
        return None;
    }
    let mut buffer = [0 as c_char; 1024];
    if unsafe {
        CFStringGetCString(
            value.0,
            buffer.as_mut_ptr(),
            buffer.len() as CFIndex,
            0x0800_0100,
        )
    } == 0
    {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn object_conforms(entry: IoObject, class_name: &CStr) -> bool {
    unsafe { IOObjectConformsTo(entry, class_name.as_ptr()) }
}

fn usb_device_parent_id(entry: IoObject) -> Option<u64> {
    let class = c"IOUSBHostDevice";
    let mut cursor = parent_entry(entry)?;
    for _ in 0..16 {
        if object_conforms(cursor.0, class) {
            return registry_id(cursor.0);
        }
        cursor = parent_entry(cursor.0)?;
    }
    None
}

fn usb_interface_parent_id(entry: IoObject) -> Option<u64> {
    let class = c"IOUSBHostInterface";
    let mut cursor = parent_entry(entry)?;
    for _ in 0..16 {
        if object_conforms(cursor.0, class) {
            return registry_id(cursor.0);
        }
        cursor = parent_entry(cursor.0)?;
    }
    None
}

#[derive(Clone, Debug)]
struct DeviceFact {
    id: u64,
    vendor_id: Option<i32>,
    product_id: Option<i32>,
    location_id: Option<u32>,
}

#[derive(Clone, Debug)]
struct ControlFact {
    device_id: Option<u64>,
    interface_class: Option<i32>,
    interface_subclass: Option<i32>,
    interface_protocol: Option<i32>,
    interface_number: Option<i32>,
}

#[derive(Clone, Debug)]
struct SerialFact {
    device_id: Option<u64>,
    data_interface_id: Option<u64>,
    serial_id: Option<u64>,
    callout_device: Option<String>,
    data_class: Option<i32>,
    data_subclass: Option<i32>,
    data_interface_number: Option<i32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CtAssociation {
    callout_device: String,
    serial_entry_id: u64,
    data_interface_entry_id: u64,
    device_entry_id: u64,
    location_id: Option<u32>,
}

fn select_ct_association(
    devices: &[DeviceFact],
    controls: &[ControlFact],
    serials: &[SerialFact],
) -> Result<CtAssociation, ProbeErrorKind> {
    let mut matched_devices = devices
        .iter()
        .filter(|device| {
            device.vendor_id == Some(USB_VENDOR_ID) && device.product_id == Some(USB_PRODUCT_ID)
        })
        .collect::<Vec<_>>();

    match matched_devices.len() {
        0 => return Err(ProbeErrorKind::DeviceUnavailable),
        1 => {}
        _ => return Err(ProbeErrorKind::AmbiguousDevice),
    }

    let target = matched_devices.remove(0);
    let mut complete_controls = 0usize;
    let mut malformed_controls = 0usize;
    for control in controls
        .iter()
        .filter(|control| control.device_id == Some(target.id))
    {
        let Some(class) = control.interface_class else {
            malformed_controls += 1;
            continue;
        };
        let Some(subclass) = control.interface_subclass else {
            malformed_controls += 1;
            continue;
        };
        let Some(protocol) = control.interface_protocol else {
            malformed_controls += 1;
            continue;
        };
        let Some(number) = control.interface_number else {
            malformed_controls += 1;
            continue;
        };
        if class == CONTROL_CLASS
            && subclass == CONTROL_SUBCLASS
            && protocol == CONTROL_PROTOCOL
            && number == CONTROL_INTERFACE_NUMBER
        {
            complete_controls += 1;
        }
    }

    if complete_controls != 1 {
        return Err(if complete_controls == 0 {
            ProbeErrorKind::DiscoveryIncomplete
        } else {
            ProbeErrorKind::AmbiguousDevice
        });
    }
    if malformed_controls > 0 {
        return Err(ProbeErrorKind::DiscoveryIncomplete);
    }

    let mut complete_serials = Vec::new();
    let mut malformed_serials = 0usize;
    for serial in serials
        .iter()
        .filter(|serial| serial.device_id == Some(target.id))
    {
        let Some(serial_entry_id) = serial.serial_id else {
            malformed_serials += 1;
            continue;
        };
        let Some(data_interface_entry_id) = serial.data_interface_id else {
            malformed_serials += 1;
            continue;
        };
        let Some(callout_device) = &serial.callout_device else {
            malformed_serials += 1;
            continue;
        };
        let Some(data_class) = serial.data_class else {
            malformed_serials += 1;
            continue;
        };
        let Some(data_subclass) = serial.data_subclass else {
            malformed_serials += 1;
            continue;
        };
        let Some(data_interface_number) = serial.data_interface_number else {
            malformed_serials += 1;
            continue;
        };
        if !callout_device.starts_with("/dev/cu.")
            || data_class != DATA_CLASS
            || data_subclass != DATA_SUBCLASS
            || data_interface_number != DATA_INTERFACE_NUMBER
        {
            malformed_serials += 1;
            continue;
        }
        complete_serials.push(CtAssociation {
            callout_device: callout_device.clone(),
            serial_entry_id,
            data_interface_entry_id,
            device_entry_id: target.id,
            location_id: target.location_id,
        });
    }

    match complete_serials.len() {
        0 => Err(ProbeErrorKind::DiscoveryIncomplete),
        1 if malformed_serials == 0 => Ok(complete_serials.remove(0)),
        1 => Err(ProbeErrorKind::DiscoveryIncomplete),
        _ => Err(ProbeErrorKind::AmbiguousDevice),
    }
}

fn strict_ct_association() -> Result<CtAssociation, ProbeErrorKind> {
    let mut devices = Vec::new();
    let mut device_iter = IoIter::for_class("IOUSBHostDevice")?;
    while let Some(device) = device_iter.next() {
        let Some(id) = registry_id(device.0) else {
            continue;
        };
        devices.push(DeviceFact {
            id,
            vendor_id: property_i32(device.0, "idVendor"),
            product_id: property_i32(device.0, "idProduct"),
            location_id: property_i32(device.0, "locationID").map(|value| value as u32),
        });
    }

    let mut controls = Vec::new();
    let mut interface_iter = IoIter::for_class("IOUSBHostInterface")?;
    while let Some(interface) = interface_iter.next() {
        controls.push(ControlFact {
            device_id: usb_device_parent_id(interface.0),
            interface_class: property_i32(interface.0, "bInterfaceClass"),
            interface_subclass: property_i32(interface.0, "bInterfaceSubClass"),
            interface_protocol: property_i32(interface.0, "bInterfaceProtocol"),
            interface_number: property_i32(interface.0, "bInterfaceNumber"),
        });
    }

    let mut serials = Vec::new();
    let mut serial_iter = IoIter::for_class("IOSerialBSDClient")?;
    while let Some(serial) = serial_iter.next() {
        let data_interface_id = usb_interface_parent_id(serial.0);
        let device_id = data_interface_id.and_then(interface_device_parent_id);
        serials.push(SerialFact {
            device_id,
            data_interface_id,
            serial_id: registry_id(serial.0),
            callout_device: property_string(serial.0, "IOCalloutDevice"),
            data_class: data_interface_id
                .and_then(|id| find_interface_property(id, "bInterfaceClass")),
            data_subclass: data_interface_id
                .and_then(|id| find_interface_property(id, "bInterfaceSubClass")),
            data_interface_number: data_interface_id
                .and_then(|id| find_interface_property(id, "bInterfaceNumber")),
        });
    }

    select_ct_association(&devices, &controls, &serials)
}

fn find_interface_property(interface_id: u64, key: &str) -> Option<i32> {
    let mut interfaces = IoIter::for_class("IOUSBHostInterface").ok()?;
    while let Some(interface) = interfaces.next() {
        if registry_id(interface.0) == Some(interface_id) {
            return property_i32(interface.0, key);
        }
    }
    None
}

fn interface_device_parent_id(interface_id: u64) -> Option<u64> {
    let mut interfaces = IoIter::for_class("IOUSBHostInterface").ok()?;
    while let Some(interface) = interfaces.next() {
        if registry_id(interface.0) == Some(interface_id) {
            return usb_device_parent_id(interface.0);
        }
    }
    None
}

struct Lease {
    _file: OwnedFd,
}

impl Lease {
    fn acquire() -> Result<Self, TransportError> {
        Self::acquire_named(LOCK_FILE_NAME)
    }

    fn acquire_named(name: &str) -> Result<Self, TransportError> {
        let directory = darwin_user_temp_dir_cstring()?;
        let fd = unsafe {
            libc::open(
                directory.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(errno_error());
        }
        let directory = unsafe { OwnedFd::from_raw_fd(fd) };
        verify_lock_directory(&directory)?;

        let name = CString::new(name).map_err(|_| TransportError {
            kind: ProbeErrorKind::NativeFailure,
            native_code: None,
        })?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(errno_error());
        }
        let file = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
            return Err(errno_error());
        }
        verify_lock_file(&file)?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or_default() as u32;
            return Err(TransportError {
                kind: if error == libc::EWOULDBLOCK as u32 {
                    ProbeErrorKind::BusyOrDenied
                } else {
                    ProbeErrorKind::NativeFailure
                },
                native_code: Some(error),
            });
        }

        Ok(Self { _file: file })
    }
}

fn darwin_user_temp_dir_cstring() -> Result<CString, TransportError> {
    let length = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, std::ptr::null_mut(), 0) };
    if length <= 1 {
        return Err(TransportError {
            kind: ProbeErrorKind::DiscoveryIncomplete,
            native_code: None,
        });
    }
    let mut buffer = vec![0u8; length as usize];
    if unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    } == 0
    {
        return Err(errno_error());
    }
    let c_path = CString::new(
        buffer
            .into_iter()
            .take_while(|byte| *byte != 0)
            .collect::<Vec<_>>(),
    )
    .map_err(|_| TransportError {
        kind: ProbeErrorKind::DiscoveryIncomplete,
        native_code: None,
    })?;
    if !Path::new(c_path.as_c_str().to_str().map_err(|_| TransportError {
        kind: ProbeErrorKind::DiscoveryIncomplete,
        native_code: None,
    })?)
    .is_absolute()
    {
        return Err(TransportError {
            kind: ProbeErrorKind::DiscoveryIncomplete,
            native_code: None,
        });
    }
    Ok(c_path)
}

fn verify_lock_directory(directory: &OwnedFd) -> Result<(), TransportError> {
    let stat = stat_fd(directory.as_raw_fd())?;
    if (stat.mode & libc::S_IFMT) != libc::S_IFDIR {
        return Err(TransportError {
            kind: ProbeErrorKind::DiscoveryIncomplete,
            native_code: None,
        });
    }
    let uid = unsafe { libc::geteuid() };
    if stat.uid != uid {
        return Err(TransportError {
            kind: ProbeErrorKind::DiscoveryIncomplete,
            native_code: None,
        });
    }
    if stat.mode & 0o077 != 0 {
        return Err(TransportError {
            kind: ProbeErrorKind::DiscoveryIncomplete,
            native_code: None,
        });
    }
    Ok(())
}

fn verify_lock_file(file: &OwnedFd) -> Result<(), TransportError> {
    let stat = stat_fd(file.as_raw_fd())?;
    if (stat.mode & libc::S_IFMT) != libc::S_IFREG
        || stat.uid != unsafe { libc::geteuid() }
        || stat.nlink != 1
        || stat.mode & 0o777 != 0o600
    {
        return Err(TransportError {
            kind: ProbeErrorKind::DiscoveryIncomplete,
            native_code: None,
        });
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct FileStat {
    mode: libc::mode_t,
    uid: libc::uid_t,
    nlink: libc::nlink_t,
    dev: u64,
    ino: u64,
}

fn stat_fd(fd: RawFd) -> Result<FileStat, TransportError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(errno_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok(FileStat {
        mode: stat.st_mode,
        uid: stat.st_uid,
        nlink: stat.st_nlink,
        dev: stat.st_dev as u64,
        ino: stat.st_ino,
    })
}

fn open_callout(path: &str) -> Result<OwnedFd, TransportError> {
    if !path.starts_with("/dev/cu.") {
        return Err(TransportError {
            kind: ProbeErrorKind::DiscoveryIncomplete,
            native_code: None,
        });
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| TransportError {
        kind: ProbeErrorKind::DiscoveryIncomplete,
        native_code: error.raw_os_error().map(|value| value as u32),
    })?;
    if !metadata.file_type().is_char_device() || metadata.file_type().is_symlink() {
        return Err(TransportError {
            kind: ProbeErrorKind::DiscoveryIncomplete,
            native_code: None,
        });
    }

    let c_path = CString::new(path).map_err(|_| TransportError {
        kind: ProbeErrorKind::DiscoveryIncomplete,
        native_code: None,
    })?;
    let flags =
        libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let fd = unsafe { libc::open(c_path.as_ptr(), flags) };
    if fd < 0 {
        return Err(errno_error());
    }
    let file = unsafe { OwnedFd::from_raw_fd(fd) };

    let opened = stat_fd(file.as_raw_fd())?;
    if (opened.mode & libc::S_IFMT) != libc::S_IFCHR
        || opened.dev != metadata.dev()
        || opened.ino != metadata.ino()
    {
        return Err(TransportError {
            kind: ProbeErrorKind::AssociationChanged,
            native_code: None,
        });
    }

    if unsafe { libc::ioctl(file.as_raw_fd(), libc::TIOCEXCL as libc::c_ulong) } != 0 {
        let error = io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or_default() as u32;
        return Err(TransportError {
            kind: if error == libc::EBUSY as u32 || error == libc::EPERM as u32 {
                ProbeErrorKind::BusyOrDenied
            } else {
                ProbeErrorKind::NativeFailure
            },
            native_code: Some(error),
        });
    }

    Ok(file)
}

fn configure_serial(fd: RawFd) -> Result<(), TransportError> {
    let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) } != 0 {
        return Err(errno_error());
    }
    let mut termios = unsafe { termios.assume_init() };

    apply_raw_serial_termios(&mut termios);
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) } != 0 {
        return Err(errno_error());
    }

    let speed: libc::speed_t = 256_000;
    if unsafe { libc::ioctl(fd, IOSSIOSPEED, (&raw const speed).cast::<c_void>()) } != 0 {
        return Err(errno_error());
    }

    verify_raw_serial_termios(fd)
}

fn apply_raw_serial_termios(termios: &mut libc::termios) {
    unsafe { libc::cfmakeraw(termios) };
    termios.c_cflag &= !(libc::PARENB | libc::CSTOPB | libc::CSIZE | libc::CRTSCTS);
    termios.c_cflag |= libc::CS8 | libc::CREAD | libc::CLOCAL;
    termios.c_iflag &=
        !(libc::IXON | libc::IXOFF | libc::IXANY | libc::ISTRIP | libc::INLCR | libc::ICRNL);
    termios.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
    termios.c_oflag &= !libc::OPOST;
    termios.c_cc[libc::VMIN] = 1;
    termios.c_cc[libc::VTIME] = 0;
}

fn verify_raw_serial_termios(fd: RawFd) -> Result<(), TransportError> {
    let mut verify = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(fd, verify.as_mut_ptr()) } != 0 {
        return Err(errno_error());
    }
    let verify = unsafe { verify.assume_init() };
    if (verify.c_cflag & libc::CSIZE) != libc::CS8
        || (verify.c_cflag & libc::PARENB) != 0
        || (verify.c_cflag & libc::CSTOPB) != 0
        || (verify.c_cflag & libc::CRTSCTS) != 0
        || (verify.c_iflag
            & (libc::IXON | libc::IXOFF | libc::IXANY | libc::ISTRIP | libc::INLCR | libc::ICRNL))
            != 0
        || (verify.c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG)) != 0
        || (verify.c_oflag & libc::OPOST) != 0
        || verify.c_cc[libc::VMIN] != 1
        || verify.c_cc[libc::VTIME] != 0
    {
        return Err(TransportError {
            kind: ProbeErrorKind::InvalidSerialSettings,
            native_code: None,
        });
    }

    Ok(())
}

fn map_io_error(code: u32) -> TransportError {
    let kind = match code as i32 {
        libc::EACCES | libc::EPERM | libc::EBUSY => ProbeErrorKind::BusyOrDenied,
        libc::ENXIO | libc::ENODEV | libc::EIO | libc::EPIPE => ProbeErrorKind::Disconnected,
        _ => ProbeErrorKind::NativeFailure,
    };
    TransportError {
        kind,
        native_code: Some(code),
    }
}

fn errno_error() -> TransportError {
    map_io_error(
        io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or_default() as u32,
    )
}

pub(super) struct MacosSerialTransport {
    file: OwnedFd,
    _lease: Lease,
    capability: crate::native::windows_worker::CtWorkerCapability,
    started: Instant,
    phases: Vec<PhaseProgress>,
    opened_at_ms: u32,
    configured_at_ms: u32,
    diagnostics: SerialDiagnostics,
}

impl MacosSerialTransport {
    fn open(
        capability: crate::native::windows_worker::CtWorkerCapability,
        cancelled: &AtomicBool,
    ) -> Result<Self, ProbeFailure> {
        if !capability.valid() {
            return Err(native_failure(
                ProbeErrorKind::NativeFailure,
                Stage::Discovery,
                None,
            ));
        }

        let started = Instant::now();
        let mut phases = vec![PhaseProgress {
            phase: Phase::Preparing,
            started_ms: 0,
            elapsed_ms: 0,
        }];

        phases.push(PhaseProgress {
            phase: Phase::Discovery,
            started_ms: 0,
            elapsed_ms: 0,
        });
        if cancelled.load(Ordering::Acquire) {
            return Err(native_failure(ProbeErrorKind::Cancelled, Stage::Open, None));
        }
        let association =
            strict_ct_association().map_err(|kind| native_failure(kind, Stage::Discovery, None))?;

        phases.push(PhaseProgress {
            phase: Phase::Open,
            started_ms: started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
            elapsed_ms: 0,
        });
        if !capability.valid() {
            return Err(native_failure(
                ProbeErrorKind::NativeFailure,
                Stage::Open,
                None,
            ));
        }
        let lease = Lease::acquire()
            .map_err(|error| native_failure(error.kind, Stage::Open, error.native_code))?;
        let file = open_callout(&association.callout_device)
            .map_err(|error| native_failure(error.kind, Stage::Open, error.native_code))?;
        let opened_at_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;

        phases.push(PhaseProgress {
            phase: Phase::AssociationCheck,
            started_ms: opened_at_ms,
            elapsed_ms: 0,
        });
        if !capability.valid() {
            return Err(native_failure(
                ProbeErrorKind::NativeFailure,
                Stage::Discovery,
                None,
            ));
        }
        let reopened =
            strict_ct_association().map_err(|kind| native_failure(kind, Stage::Discovery, None))?;
        if reopened != association {
            return Err(native_failure(
                ProbeErrorKind::AssociationChanged,
                Stage::Discovery,
                None,
            ));
        }

        phases.push(PhaseProgress {
            phase: Phase::Configure,
            started_ms: started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
            elapsed_ms: 0,
        });
        if !capability.valid() {
            return Err(native_failure(
                ProbeErrorKind::NativeFailure,
                Stage::Configure,
                None,
            ));
        }
        configure_serial(file.as_raw_fd())
            .map_err(|error| native_failure(error.kind, Stage::Configure, error.native_code))?;
        let configured_at_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;

        if cancelled.load(Ordering::Acquire) {
            return Err(native_failure(
                ProbeErrorKind::Cancelled,
                Stage::Configure,
                None,
            ));
        }

        Ok(Self {
            file,
            _lease: lease,
            capability,
            started,
            phases,
            opened_at_ms,
            configured_at_ms,
            diagnostics: SerialDiagnostics::default(),
        })
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }

    fn capability_error(&self) -> TransportError {
        TransportError {
            kind: ProbeErrorKind::NativeFailure,
            native_code: None,
        }
    }
}

impl Transport for MacosSerialTransport {
    fn now_ms(&mut self) -> u64 {
        self.elapsed_ms()
    }

    fn write(
        &mut self,
        bytes: &[u8],
        deadline_ms: u64,
        cancelled: &AtomicBool,
    ) -> Result<usize, TransportError> {
        self.diagnostics.write_calls = self.diagnostics.write_calls.saturating_add(1);
        if !self.capability.valid() {
            self.diagnostics.write_failures = self.diagnostics.write_failures.saturating_add(1);
            return Err(self.capability_error());
        }

        let result =
            write_nonblocking(self.file.as_raw_fd(), bytes, deadline_ms, cancelled, || {
                self.elapsed_ms()
            });
        match result {
            Ok(written) => {
                self.diagnostics.completed_write_bytes = self
                    .diagnostics
                    .completed_write_bytes
                    .saturating_add(written as u32);
                Ok(written)
            }
            Err(error) => {
                self.diagnostics.write_failures = self.diagnostics.write_failures.saturating_add(1);
                Err(error)
            }
        }
    }

    fn read(
        &mut self,
        bytes: &mut [u8],
        deadline_ms: u64,
        cancelled: &AtomicBool,
    ) -> Result<ReadResult, TransportError> {
        self.diagnostics.read_calls = self.diagnostics.read_calls.saturating_add(1);
        if !self.capability.valid() {
            self.diagnostics.read_failures = self.diagnostics.read_failures.saturating_add(1);
            return Err(self.capability_error());
        }

        let result = read_nonblocking(self.file.as_raw_fd(), bytes, deadline_ms, cancelled, || {
            self.elapsed_ms()
        });
        match result {
            Ok(ReadResult::Data(read)) => {
                self.diagnostics.completed_read_bytes = self
                    .diagnostics
                    .completed_read_bytes
                    .saturating_add(read as u32);
                Ok(ReadResult::Data(read))
            }
            Ok(ReadResult::Idle) => {
                self.diagnostics.idle_reads = self.diagnostics.idle_reads.saturating_add(1);
                Ok(ReadResult::Idle)
            }
            Ok(ReadResult::Disconnected) => Ok(ReadResult::Disconnected),
            Err(error) => {
                self.diagnostics.read_failures = self.diagnostics.read_failures.saturating_add(1);
                Err(error)
            }
        }
    }

    fn progress_origin_ms(&self) -> Option<u64> {
        Some(0)
    }

    fn initial_phases(&self) -> &[PhaseProgress] {
        &self.phases
    }

    fn port_opened_at_ms(&self) -> Option<u32> {
        Some(self.opened_at_ms)
    }

    fn port_configured_at_ms(&self) -> Option<u32> {
        Some(self.configured_at_ms)
    }

    fn serial_diagnostics(&self) -> Option<SerialDiagnostics> {
        Some(self.diagnostics.clone())
    }

    fn serial_settings(&self) -> Option<SerialSettings> {
        None
    }
}

fn write_nonblocking(
    fd: RawFd,
    bytes: &[u8],
    deadline_ms: u64,
    cancelled: &AtomicBool,
    mut elapsed_ms: impl FnMut() -> u64,
) -> Result<usize, TransportError> {
    let mut interrupted = 0u32;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(TransportError {
                kind: ProbeErrorKind::Cancelled,
                native_code: None,
            });
        }
        if elapsed_ms() >= deadline_ms {
            return Err(TransportError {
                kind: ProbeErrorKind::Timeout,
                native_code: None,
            });
        }
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written > 0 {
            return Ok(written as usize);
        }
        if written == 0 {
            return Ok(0);
        }
        let error = io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or_default();
        if error == libc::EINTR {
            interrupted = interrupted.saturating_add(1);
            if interrupted > 32 {
                return Err(TransportError {
                    kind: ProbeErrorKind::Timeout,
                    native_code: None,
                });
            }
            continue;
        }
        if error == libc::EAGAIN || error == libc::EWOULDBLOCK {
            let remaining = deadline_ms.saturating_sub(elapsed_ms());
            let timeout = i32::try_from(remaining.min(5)).unwrap_or(5);
            let _ = poll_fd(fd, libc::POLLOUT, timeout, cancelled)?;
            continue;
        }
        return Err(map_io_error(error as u32));
    }
}

fn read_nonblocking(
    fd: RawFd,
    bytes: &mut [u8],
    deadline_ms: u64,
    cancelled: &AtomicBool,
    mut elapsed_ms: impl FnMut() -> u64,
) -> Result<ReadResult, TransportError> {
    let mut interrupted = 0u32;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(TransportError {
                kind: ProbeErrorKind::Cancelled,
                native_code: None,
            });
        }
        if elapsed_ms() >= deadline_ms {
            return Err(TransportError {
                kind: ProbeErrorKind::Timeout,
                native_code: None,
            });
        }
        let read = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if read > 0 {
            return Ok(ReadResult::Data(read as usize));
        }
        if read == 0 {
            return Ok(ReadResult::Disconnected);
        }
        let error = io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or_default();
        if error == libc::EINTR {
            interrupted = interrupted.saturating_add(1);
            if interrupted > 32 {
                return Err(TransportError {
                    kind: ProbeErrorKind::Timeout,
                    native_code: None,
                });
            }
            continue;
        }
        if error == libc::EAGAIN || error == libc::EWOULDBLOCK {
            let remaining = deadline_ms.saturating_sub(elapsed_ms());
            let timeout = i32::try_from(remaining.min(50)).unwrap_or(50);
            let ready = poll_fd(fd, libc::POLLIN, timeout, cancelled)?;
            if !ready {
                return Ok(ReadResult::Idle);
            }
            continue;
        }
        return Err(map_io_error(error as u32));
    }
}

fn poll_fd(
    fd: RawFd,
    events: i16,
    timeout_ms: i32,
    cancelled: &AtomicBool,
) -> Result<bool, TransportError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(TransportError {
            kind: ProbeErrorKind::Cancelled,
            native_code: None,
        });
    }
    if timeout_ms <= 0 {
        return Err(TransportError {
            kind: ProbeErrorKind::Timeout,
            native_code: None,
        });
    }
    let mut pollfd = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
    if result > 0 {
        if (pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)) != 0 {
            return Err(TransportError {
                kind: ProbeErrorKind::Disconnected,
                native_code: None,
            });
        }
        Ok(true)
    } else if result == 0 {
        Ok(false)
    } else {
        let error = io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or_default();
        if error == libc::EINTR {
            Ok(true)
        } else {
            Err(map_io_error(error as u32))
        }
    }
}

pub(super) fn run_native(
    options: Options,
    cancelled: &AtomicBool,
) -> Result<ProbeReport, ProbeFailure> {
    if let Err(kind) = options.validate() {
        return Err(native_failure(kind, Stage::Options, None));
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(native_failure(ProbeErrorKind::Cancelled, Stage::Open, None));
    }

    let capability = crate::native::windows_worker::ct_worker_capability()
        .ok_or_else(|| native_failure(ProbeErrorKind::NativeFailure, Stage::Discovery, None))?;
    if !capability.valid() {
        return Err(native_failure(
            ProbeErrorKind::NativeFailure,
            Stage::Discovery,
            None,
        ));
    }

    let wrapper_started = Instant::now();
    let mut transport = MacosSerialTransport::open(capability, cancelled)?;
    let elapsed = transport.now_ms();
    let remaining = u64::from(options.total_timeout_ms).saturating_sub(elapsed);
    if remaining < 1_000 || u64::from(options.capture_ms) > remaining {
        return Err(native_failure(
            ProbeErrorKind::Timeout,
            Stage::Configure,
            None,
        ));
    }

    let mut adjusted = options;
    adjusted.total_timeout_ms = remaining as u32;
    adjusted.io_timeout_ms = adjusted.io_timeout_ms.min(adjusted.total_timeout_ms);
    let mut report = run_with_evidence(
        adjusted,
        transport,
        cancelled,
        Evidence::NativeTransportObservation,
    )?;
    report.elapsed_ms = wrapper_started
        .elapsed()
        .as_millis()
        .min(u128::from(u32::MAX)) as u32;
    Ok(report)
}

fn native_failure(kind: ProbeErrorKind, stage: Stage, native_code: Option<u32>) -> ProbeFailure {
    ProbeFailure {
        kind,
        stage,
        native_code,
        progress: Box::new(Progress::new(Evidence::NativeTransportObservation).record),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::fd::{FromRawFd, OwnedFd},
        os::unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, PermissionsExt},
        },
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let base = PathBuf::from(
                std::env::var_os("PORTABLE_TEST_FIXTURES")
                    .expect("PORTABLE_TEST_FIXTURES must be set"),
            );
            let path = base.join(format!("ct-macos-native-{}", uuid::Uuid::new_v4()));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn remove_named_lockfile(name: &str) {
        if let Ok(base) = darwin_user_temp_dir_cstring()
            && let Ok(path) = base.into_string()
        {
            let _ = fs::remove_file(Path::new(&path).join(name));
        }
    }

    struct HolderProcess {
        child: Option<Child>,
        lock_name: String,
    }

    impl HolderProcess {
        fn new(child: Child, lock_name: String) -> Self {
            Self {
                child: Some(child),
                lock_name,
            }
        }
    }

    impl Drop for HolderProcess {
        fn drop(&mut self) {
            if let Some(child) = self.child.as_mut()
                && child.try_wait().ok().flatten().is_none()
            {
                let _ = child.kill();
                let _ = child.wait();
            }
            remove_named_lockfile(&self.lock_name);
        }
    }

    fn set_nonblocking(fd: RawFd) {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0);
        assert_ne!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            -1
        );
    }

    fn pipe_pair() -> (OwnedFd, OwnedFd) {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        (unsafe { OwnedFd::from_raw_fd(fds[0]) }, unsafe {
            OwnedFd::from_raw_fd(fds[1])
        })
    }

    fn pty_pair() -> (OwnedFd, OwnedFd) {
        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        (unsafe { OwnedFd::from_raw_fd(master) }, unsafe {
            OwnedFd::from_raw_fd(slave)
        })
    }

    fn device(id: u64) -> DeviceFact {
        DeviceFact {
            id,
            vendor_id: Some(USB_VENDOR_ID),
            product_id: Some(USB_PRODUCT_ID),
            location_id: Some(1),
        }
    }

    fn control(device_id: u64) -> ControlFact {
        ControlFact {
            device_id: Some(device_id),
            interface_class: Some(CONTROL_CLASS),
            interface_subclass: Some(CONTROL_SUBCLASS),
            interface_protocol: Some(CONTROL_PROTOCOL),
            interface_number: Some(CONTROL_INTERFACE_NUMBER),
        }
    }

    fn serial(device_id: u64, id: u64) -> SerialFact {
        SerialFact {
            device_id: Some(device_id),
            data_interface_id: Some(id + 1),
            serial_id: Some(id),
            callout_device: Some(format!("/dev/cu.usbmodem{id}")),
            data_class: Some(DATA_CLASS),
            data_subclass: Some(DATA_SUBCLASS),
            data_interface_number: Some(DATA_INTERFACE_NUMBER),
        }
    }

    #[test]
    fn strict_fact_selection_fails_closed_on_ambiguous_or_incomplete_endpoints() {
        let selected =
            select_ct_association(&[device(10)], &[control(10)], &[serial(10, 21)]).unwrap();
        assert_eq!(selected.device_entry_id, 10);

        assert_eq!(
            select_ct_association(&[device(10), device(11)], &[control(10)], &[serial(10, 21)]),
            Err(ProbeErrorKind::AmbiguousDevice)
        );

        let mut incomplete = serial(10, 21);
        incomplete.callout_device = None;
        assert_eq!(
            select_ct_association(&[device(10)], &[control(10)], &[incomplete]),
            Err(ProbeErrorKind::DiscoveryIncomplete)
        );

        let malformed_sibling = SerialFact {
            callout_device: Some("/dev/cu.usbmodemBAD".into()),
            data_class: None,
            data_subclass: Some(DATA_SUBCLASS),
            data_interface_number: Some(DATA_INTERFACE_NUMBER),
            ..serial(10, 22)
        };
        assert_eq!(
            select_ct_association(
                &[device(10)],
                &[control(10)],
                &[serial(10, 21), malformed_sibling],
            ),
            Err(ProbeErrorKind::DiscoveryIncomplete)
        );
    }

    #[test]
    fn iossiospeed_ioctl_encoding_matches_darwin_iow_speed_t() {
        const fn darwin_iow(group: u8, number: u8, size: usize) -> libc::c_ulong {
            0x8000_0000u64
                | (((size & 0x1fff) as u64) << 16)
                | ((group as u64) << 8)
                | number as u64
        }
        let expected = darwin_iow(
            IOSSIOSPEED_GROUP,
            IOSSIOSPEED_NUMBER,
            std::mem::size_of::<libc::speed_t>(),
        );
        assert_eq!(IOSSIOSPEED, expected);
        assert_eq!(IOSSIOSPEED, 0x8008_5402);
    }

    #[test]
    fn lease_is_kernel_released_after_holder_sigkill() {
        const TEST: &str = "devices::ct::probe::macos_native::tests::lease_is_kernel_released_after_holder_sigkill";
        if std::env::var("CT_MACOS_NATIVE_ISOLATED").as_deref() == Ok("lease-holder") {
            let lock = std::env::var("CT_MACOS_LOCK_NAME").unwrap();
            let ready = PathBuf::from(std::env::var("CT_MACOS_LOCK_READY").unwrap());
            let _lease = Lease::acquire_named(&lock).unwrap();
            fs::write(&ready, b"ready").unwrap();
            loop {
                thread::park();
            }
        }
        let fixture = Fixture::new();
        let ready = fixture.path("ready");
        let lock = format!("copilot-controller-ct-test-lock-{}", uuid::Uuid::new_v4());

        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env("CT_MACOS_NATIVE_ISOLATED", "lease-holder")
            .env("CT_MACOS_LOCK_NAME", &lock)
            .env("CT_MACOS_LOCK_READY", &ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut holder = HolderProcess::new(child, lock.clone());

        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if !ready.exists() {
            panic!("isolated holder never acknowledged lock acquisition");
        }

        let contender = Lease::acquire_named(&lock);
        if !matches!(
            contender,
            Err(TransportError {
                kind: ProbeErrorKind::BusyOrDenied,
                ..
            })
        ) {
            panic!("contender did not observe held flock");
        }

        let pid = holder.child.as_ref().unwrap().id();
        assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGKILL) }, 0);
        let status = holder.child.as_mut().unwrap().wait().unwrap();
        holder.child = None;
        assert!(!status.success());

        let reclaimed =
            Lease::acquire_named(&lock).expect("failed to reacquire after killed holder");
        drop(reclaimed);
        remove_named_lockfile(&lock);
    }

    #[test]
    fn lock_directory_and_file_validation_fail_closed() {
        let fixture = Fixture::new();
        let good_dir = fixture.path("good-dir");
        fs::DirBuilder::new().mode(0o700).create(&good_dir).unwrap();
        let bad_dir = fixture.path("bad-dir");
        fs::DirBuilder::new().mode(0o700).create(&bad_dir).unwrap();
        fs::set_permissions(&bad_dir, fs::Permissions::from_mode(0o755)).unwrap();

        let open_dir = |path: &Path| -> OwnedFd {
            let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
            let fd = unsafe {
                libc::open(
                    c_path.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            assert!(fd >= 0);
            unsafe { OwnedFd::from_raw_fd(fd) }
        };

        let good = open_dir(&good_dir);
        assert!(verify_lock_directory(&good).is_ok());

        let bad = open_dir(&bad_dir);
        assert_eq!(
            verify_lock_directory(&bad).unwrap_err().kind,
            ProbeErrorKind::DiscoveryIncomplete
        );

        let lock_path = fixture.path("lockfile");
        fs::write(&lock_path, []).unwrap();
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).unwrap();
        let lock_c = CString::new(lock_path.as_os_str().as_bytes()).unwrap();
        let fd = unsafe { libc::open(lock_c.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        assert!(fd >= 0);
        let file = unsafe { OwnedFd::from_raw_fd(fd) };
        assert!(verify_lock_file(&file).is_ok());

        let linked = fixture.path("lockfile.link");
        fs::hard_link(&lock_path, &linked).unwrap();
        assert_eq!(
            verify_lock_file(&file).unwrap_err().kind,
            ProbeErrorKind::DiscoveryIncomplete
        );
    }

    #[test]
    fn open_callout_rejects_non_dev_cu_prefix_paths() {
        let fixture = Fixture::new();
        let regular = fixture.path("regular");
        fs::write(&regular, b"x").unwrap();
        let regular = regular.to_string_lossy();
        assert_eq!(
            open_callout(&regular).unwrap_err().kind,
            ProbeErrorKind::DiscoveryIncomplete
        );

        let fake = fixture.path("cu.fake");
        let target = fixture.path("target");
        fs::write(&target, b"x").unwrap();
        std::os::unix::fs::symlink(&target, &fake).unwrap();
        let fake = fake.to_string_lossy();
        assert_eq!(
            open_callout(&fake).unwrap_err().kind,
            ProbeErrorKind::DiscoveryIncomplete
        );
    }

    #[test]
    fn poll_fd_and_nonblocking_io_helpers_handle_idle_data_and_hangup() {
        let cancelled = AtomicBool::new(false);

        let (read_fd, write_fd) = pipe_pair();
        set_nonblocking(read_fd.as_raw_fd());
        set_nonblocking(write_fd.as_raw_fd());
        let mut buffer = [0u8; 8];
        assert_eq!(
            read_nonblocking(read_fd.as_raw_fd(), &mut buffer, 100, &cancelled, || 0).unwrap(),
            ReadResult::Idle
        );
        assert_eq!(
            unsafe { libc::write(write_fd.as_raw_fd(), b"abc".as_ptr().cast(), 3,) },
            3
        );
        assert_eq!(
            read_nonblocking(read_fd.as_raw_fd(), &mut buffer, 100, &cancelled, || 0).unwrap(),
            ReadResult::Data(3)
        );
        assert_eq!(&buffer[..3], b"abc");
        drop(write_fd);
        assert_eq!(
            read_nonblocking(read_fd.as_raw_fd(), &mut buffer, 100, &cancelled, || 0).unwrap(),
            ReadResult::Disconnected
        );

        let (read_fd, write_fd) = pipe_pair();
        set_nonblocking(read_fd.as_raw_fd());
        set_nonblocking(write_fd.as_raw_fd());
        assert_eq!(
            write_nonblocking(write_fd.as_raw_fd(), b"xyz", 100, &cancelled, || 0).unwrap(),
            3
        );
        let mut read_back = [0u8; 8];
        assert_eq!(
            read_nonblocking(read_fd.as_raw_fd(), &mut read_back, 100, &cancelled, || 0).unwrap(),
            ReadResult::Data(3)
        );
        assert_eq!(&read_back[..3], b"xyz");
        drop(read_fd);
        assert_eq!(
            poll_fd(write_fd.as_raw_fd(), libc::POLLOUT, 20, &cancelled)
                .unwrap_err()
                .kind,
            ProbeErrorKind::Disconnected
        );
    }

    #[test]
    fn pty_nonblocking_respects_vmin_and_reports_idle_data_disconnect_deadline_and_cancel() {
        let cancelled = AtomicBool::new(false);
        let (master, slave) = pty_pair();
        set_nonblocking(slave.as_raw_fd());

        let mut seeded = std::mem::MaybeUninit::<libc::termios>::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), seeded.as_mut_ptr()) },
            0
        );
        let mut seeded = unsafe { seeded.assume_init() };
        seeded.c_cc[libc::VMIN] = 0;
        seeded.c_cc[libc::VTIME] = 0;
        assert_eq!(
            unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &seeded) },
            0
        );

        apply_raw_serial_termios(&mut seeded);
        assert_eq!(
            unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &seeded) },
            0
        );
        verify_raw_serial_termios(slave.as_raw_fd()).unwrap();

        let idle_start = Instant::now();
        let mut buffer = [0u8; 16];
        assert_eq!(
            read_nonblocking(slave.as_raw_fd(), &mut buffer, 120, &cancelled, || {
                idle_start.elapsed().as_millis() as u64
            })
            .unwrap(),
            ReadResult::Idle
        );

        let writer_fd = unsafe { libc::dup(master.as_raw_fd()) };
        assert!(writer_fd >= 0);
        let writer_fd = unsafe { OwnedFd::from_raw_fd(writer_fd) };
        let delayed = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            assert_eq!(
                unsafe { libc::write(writer_fd.as_raw_fd(), b"pty".as_ptr().cast(), 3) },
                3
            );
        });
        let read_start = Instant::now();
        assert_eq!(
            read_nonblocking(slave.as_raw_fd(), &mut buffer, 300, &cancelled, || {
                read_start.elapsed().as_millis() as u64
            })
            .unwrap(),
            ReadResult::Data(3)
        );
        assert_eq!(&buffer[..3], b"pty");
        delayed.join().unwrap();

        let write_cancelled = AtomicBool::new(true);
        assert_eq!(
            write_nonblocking(slave.as_raw_fd(), b"x", 50, &write_cancelled, || 0)
                .unwrap_err()
                .kind,
            ProbeErrorKind::Cancelled
        );
        let write_active = AtomicBool::new(false);
        assert_eq!(
            write_nonblocking(slave.as_raw_fd(), b"x", 0, &write_active, || 0)
                .unwrap_err()
                .kind,
            ProbeErrorKind::Timeout
        );

        let read_cancelled = AtomicBool::new(true);
        assert_eq!(
            read_nonblocking(slave.as_raw_fd(), &mut buffer, 50, &read_cancelled, || 0)
                .unwrap_err()
                .kind,
            ProbeErrorKind::Cancelled
        );
        let read_start = Instant::now();
        assert_eq!(
            read_nonblocking(slave.as_raw_fd(), &mut buffer, 1, &cancelled, || {
                read_start.elapsed().as_millis() as u64 + 1
            })
            .unwrap_err()
            .kind,
            ProbeErrorKind::Timeout
        );

        drop(master);
        let closed = read_nonblocking(slave.as_raw_fd(), &mut buffer, 200, &cancelled, || 0);
        assert!(matches!(
            closed,
            Ok(ReadResult::Disconnected)
                | Err(TransportError {
                    kind: ProbeErrorKind::Disconnected,
                    ..
                })
        ));
    }

    #[test]
    fn errno_mapping_is_typed_for_permission_and_disconnect_codes() {
        assert_eq!(
            map_io_error(libc::EACCES as u32).kind,
            ProbeErrorKind::BusyOrDenied
        );
        assert_eq!(
            map_io_error(libc::EBUSY as u32).kind,
            ProbeErrorKind::BusyOrDenied
        );
        assert_eq!(
            map_io_error(libc::ENODEV as u32).kind,
            ProbeErrorKind::Disconnected
        );
        assert_eq!(
            map_io_error(libc::EIO as u32).kind,
            ProbeErrorKind::Disconnected
        );
        assert_eq!(map_io_error(123_456).kind, ProbeErrorKind::NativeFailure);
    }

    #[test]
    fn native_entry_refuses_unsupervised_access_before_discovery() {
        let failure = run_native(
            Options {
                capture_ms: 0,
                ..Options::default()
            },
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(failure.kind, ProbeErrorKind::NativeFailure);
        assert_eq!(failure.stage, Stage::Discovery);
    }
}
