//! macOS ARM64 HID backend over IOKit's IOHIDManager and CoreFoundation.
//! All raw FFI is private to this file. Not validated against hardware.
//!
//! Input reports are delivered through a run-loop callback on the thread that
//! opened the device (the transport is therefore `!Send`). Every live operation
//! requires a private, thread-bound worker capability. Synchronous output has no
//! individual deadline; the supervised parent can kill and reap the child.
//! Stopped/finished run loops fail with a bounded static error, never a spin.

use std::collections::VecDeque;
use std::ffi::{CString, c_char, c_void};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use super::*;
use crate::devices::micro::{
    enumeration::{APPLICATION_PID, APPLICATION_VID, REPORT_SIZE, USAGE, USAGE_PAGE},
    probe::verify_micro_descriptor,
};

type CFTypeRef = *const c_void;
type CFIndex = isize;
type IOReturn = i32;

const UTF8: u32 = 0x0800_0100;
const SINT32: CFIndex = 3;
const MAX_QUEUED: usize = 64;
const MAX_SCAN: CFIndex = 4096;
const MAX_INTERFACE_ANCESTRY_NODES: usize = 16;
const K_IO_HID_OPTIONS_TYPE_SEIZE_DEVICE: u32 = 0x1;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFAllocatorDefault: CFTypeRef;
    static kCFRunLoopDefaultMode: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    fn CFRelease(cf: CFTypeRef);
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFDataGetLength(data: CFTypeRef) -> CFIndex;
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    fn CFNumberGetValue(n: CFTypeRef, ty: CFIndex, out: *mut c_void) -> u8;
    fn CFNumberCreate(a: CFTypeRef, ty: CFIndex, v: *const c_void) -> CFTypeRef;
    fn CFStringCreateWithCString(a: CFTypeRef, s: *const c_char, enc: u32) -> CFTypeRef;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: CFIndex, enc: u32) -> u8;
    fn CFDictionaryCreate(
        a: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        n: CFIndex,
        kcb: *const u8,
        vcb: *const u8,
    ) -> CFTypeRef;
    fn CFSetGetCount(s: CFTypeRef) -> CFIndex;
    fn CFSetGetValues(s: CFTypeRef, out: *mut CFTypeRef);
    fn CFRunLoopGetCurrent() -> CFTypeRef;
    fn CFRunLoopRunInMode(mode: CFTypeRef, secs: f64, return_after_source: u8) -> i32;
}

type InputCb = extern "C" fn(*mut c_void, IOReturn, *mut c_void, u32, u32, *mut u8, CFIndex);

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDManagerCreate(a: CFTypeRef, options: u32) -> CFTypeRef;
    fn IOHIDManagerSetDeviceMatching(m: CFTypeRef, matching: CFTypeRef);
    fn IOHIDManagerCopyDevices(m: CFTypeRef) -> CFTypeRef;
    fn IOHIDDeviceGetProperty(d: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn IOHIDDeviceGetService(d: CFTypeRef) -> u32;
    fn IOHIDDeviceOpen(d: CFTypeRef, options: u32) -> IOReturn;
    fn IOHIDDeviceClose(d: CFTypeRef, options: u32) -> IOReturn;
    fn IOHIDDeviceSetReport(
        d: CFTypeRef,
        ty: u32,
        id: CFIndex,
        report: *const u8,
        len: CFIndex,
    ) -> IOReturn;
    fn IOHIDDeviceRegisterInputReportCallback(
        d: CFTypeRef,
        report: *mut u8,
        len: CFIndex,
        cb: Option<InputCb>,
        ctx: *mut c_void,
    );
    fn IOHIDDeviceScheduleWithRunLoop(d: CFTypeRef, rl: CFTypeRef, mode: CFTypeRef);
    fn IOHIDDeviceUnscheduleFromRunLoop(d: CFTypeRef, rl: CFTypeRef, mode: CFTypeRef);
    fn IORegistryEntryGetRegistryEntryID(service: u32, id: *mut u64) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CFTypeRef,
        allocator: CFTypeRef,
        options: u32,
    ) -> CFTypeRef;
    fn IORegistryEntryGetParentEntry(entry: u32, plane: *const c_char, parent: *mut u32) -> i32;
    fn IOObjectConformsTo(object: u32, class_name: *const c_char) -> i32;
    fn IOObjectRelease(object: u32) -> i32;
}

/// Owned CoreFoundation object released on drop.
struct Cf(CFTypeRef);

impl Cf {
    fn new(p: CFTypeRef) -> Option<Self> {
        (!p.is_null()).then(|| Self(p))
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        // SAFETY: owned +1 reference.
        unsafe { CFRelease(self.0) };
    }
}

fn cfstr(s: &str) -> Option<Cf> {
    let c = CString::new(s).ok()?;
    // SAFETY: valid NUL-terminated string.
    Cf::new(unsafe { CFStringCreateWithCString(kCFAllocatorDefault, c.as_ptr(), UTF8) })
}

fn cfnum(v: i32) -> Option<Cf> {
    // SAFETY: `v` is a valid SInt32 source.
    Cf::new(unsafe { CFNumberCreate(kCFAllocatorDefault, SINT32, (&raw const v).cast()) })
}

fn prop_i32(dev: CFTypeRef, key: &str) -> Option<i32> {
    let k = cfstr(key)?;
    // SAFETY: borrowed (get-rule) property; type verified before use.
    let v = unsafe { IOHIDDeviceGetProperty(dev, k.0) };
    if v.is_null() || unsafe { CFGetTypeID(v) != CFNumberGetTypeID() } {
        return None;
    }
    let mut out = 0i32;
    // SAFETY: `v` is a CFNumber and `out` a valid SInt32 destination.
    (unsafe { CFNumberGetValue(v, SINT32, (&raw mut out).cast()) } != 0).then_some(out)
}

fn prop_string(dev: CFTypeRef, key: &str) -> Option<String> {
    let k = cfstr(key)?;
    // SAFETY: borrowed property; type verified before use.
    let v = unsafe { IOHIDDeviceGetProperty(dev, k.0) };
    if v.is_null() || unsafe { CFGetTypeID(v) != CFStringGetTypeID() } {
        return None;
    }
    let mut buf = [0 as c_char; 256];
    // SAFETY: buffer pointer/length describe `buf`.
    if unsafe { CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as CFIndex, UTF8) } == 0 {
        return None;
    }
    // SAFETY: CFStringGetCString NUL-terminates on success.
    let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    (!s.is_empty()).then_some(s)
}

fn prop_u32(dev: CFTypeRef, key: &str) -> Option<u32> {
    prop_i32(dev, key)
        .and_then(|value| u32::try_from(value).ok())
        .or_else(|| {
            let value = prop_i32(dev, key)?;
            Some(u32::from_ne_bytes(value.to_ne_bytes()))
        })
}

fn entry_id(dev: CFTypeRef) -> Option<u64> {
    // SAFETY: borrowed device; out-pointer is valid.
    let svc = unsafe { IOHIDDeviceGetService(dev) };
    if svc == 0 {
        return None;
    }
    let mut id = 0u64;
    (unsafe { IORegistryEntryGetRegistryEntryID(svc, &mut id) } == 0).then_some(id)
}

fn classify_transport(transport: &str) -> HidBus {
    if transport.eq_ignore_ascii_case("usb") {
        HidBus::Usb
    } else if transport.eq_ignore_ascii_case("bluetooth")
        || transport.eq_ignore_ascii_case("Bluetooth Low Energy")
    {
        HidBus::Bluetooth
    } else {
        HidBus::Unknown
    }
}

struct IoOwned(u32);

impl IoOwned {
    fn new(value: u32) -> Option<Self> {
        (value != 0).then(|| Self(value))
    }
}

impl Drop for IoOwned {
    fn drop(&mut self) {
        let _ = unsafe { IOObjectRelease(self.0) };
    }
}

fn entry_property_i32(entry: u32, key: &str) -> Option<i32> {
    let key = cfstr(key)?;
    let value =
        Cf::new(unsafe { IORegistryEntryCreateCFProperty(entry, key.0, kCFAllocatorDefault, 0) })?;
    if unsafe { CFGetTypeID(value.0) != CFNumberGetTypeID() } {
        return None;
    }
    let mut out = 0i32;
    (unsafe { CFNumberGetValue(value.0, SINT32, (&raw mut out).cast()) } != 0).then_some(out)
}

fn parent_entry(entry: u32) -> Option<IoOwned> {
    let plane = c"IOService";
    let mut parent = 0u32;
    (unsafe { IORegistryEntryGetParentEntry(entry, plane.as_ptr(), &mut parent) == 0 })
        .then(|| IoOwned::new(parent))
        .flatten()
}

fn conforms_to(entry: u32, class_name: &std::ffi::CStr) -> bool {
    unsafe { IOObjectConformsTo(entry, class_name.as_ptr()) != 0 }
}

trait HeldNode {
    fn raw(&self) -> u32;
}

impl HeldNode for IoOwned {
    fn raw(&self) -> u32 {
        self.0
    }
}

struct AncestorEvidence<P> {
    registry_id: Option<u64>,
    is_usb_interface: bool,
    is_usb_device: bool,
    interface_number: Option<i32>,
    vendor_id: Option<i32>,
    product_id: Option<i32>,
    parent: Option<P>,
}

fn interface_number_from_ancestry<P: HeldNode>(
    service: u32,
    identity: HidIdentity,
    mut read_node: impl FnMut(u32) -> Option<AncestorEvidence<P>>,
) -> Option<i32> {
    let mut current_raw = Some(service);
    let mut current_parent: Option<P> = None;
    let mut seen = Vec::with_capacity(MAX_INTERFACE_ANCESTRY_NODES);
    for _ in 0..MAX_INTERFACE_ANCESTRY_NODES {
        let node = if let Some(parent) = current_parent.as_ref() {
            parent.raw()
        } else {
            current_raw?
        };
        let evidence = read_node(node)?;
        let id = evidence.registry_id?;
        if seen.contains(&id) {
            return None;
        }
        seen.push(id);
        if evidence.is_usb_interface {
            let interface_number = evidence.interface_number?;
            if !(0..=255).contains(&interface_number) {
                return None;
            }
            if evidence.vendor_id == Some(i32::from(identity.vendor_id))
                && evidence.product_id == Some(i32::from(identity.product_id))
            {
                return Some(interface_number);
            }
            return None;
        }
        if evidence.is_usb_device {
            return None;
        }
        current_raw = None;
        current_parent = evidence.parent;
    }
    None
}

fn resolve_interface_number<P: HeldNode>(
    direct: Option<i32>,
    service: u32,
    identity: HidIdentity,
    read_node: impl FnMut(u32) -> Option<AncestorEvidence<P>>,
) -> Option<i32> {
    if let Some(number) = direct {
        return Some(number);
    }
    interface_number_from_ancestry(service, identity, read_node)
}

fn interface_number(dev: CFTypeRef, identity: HidIdentity) -> Option<i32> {
    let direct = prop_i32(dev, "InterfaceNumber");
    let service = unsafe { IOHIDDeviceGetService(dev) };
    if service == 0 {
        return direct;
    }
    let interface_class = c"IOUSBHostInterface";
    let device_class = c"IOUSBHostDevice";
    resolve_interface_number(direct, service, identity, |entry| {
        Some(AncestorEvidence {
            registry_id: {
                let mut id = 0u64;
                (unsafe { IORegistryEntryGetRegistryEntryID(entry, &mut id) } == 0).then_some(id)
            },
            is_usb_interface: conforms_to(entry, interface_class),
            is_usb_device: conforms_to(entry, device_class),
            interface_number: entry_property_i32(entry, "bInterfaceNumber")
                .or_else(|| entry_property_i32(entry, "InterfaceNumber")),
            vendor_id: entry_property_i32(entry, "idVendor"),
            product_id: entry_property_i32(entry, "idProduct"),
            parent: parent_entry(entry),
        })
    })
}

/// Manager restricted to one vendor/product, plus the (owned) matching devices.
struct Matched {
    _manager: Cf,
    set: Cf,
}

impl Matched {
    fn new(id: HidIdentity) -> Result<Self, HidError> {
        let err = HidError::Os {
            op: HidOp::Enumerate,
            code: 0,
        };
        let (kv, kp) = (
            cfstr("VendorID").ok_or(err.clone())?,
            cfstr("ProductID").ok_or(err.clone())?,
        );
        let (vv, vp) = (
            cfnum(i32::from(id.vendor_id)).ok_or(err.clone())?,
            cfnum(i32::from(id.product_id)).ok_or(err.clone())?,
        );
        let keys = [kv.0, kp.0];
        let vals = [vv.0, vp.0];
        // SAFETY: arrays hold 2 valid CF objects; callbacks are the CF-provided statics.
        let dict = Cf::new(unsafe {
            CFDictionaryCreate(
                kCFAllocatorDefault,
                keys.as_ptr(),
                vals.as_ptr(),
                2,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            )
        })
        .ok_or(err.clone())?;
        // SAFETY: default allocator, no options. The manager is deliberately never opened.
        let manager =
            Cf::new(unsafe { IOHIDManagerCreate(kCFAllocatorDefault, 0) }).ok_or(err.clone())?;
        // SAFETY: valid manager and dictionary.
        unsafe { IOHIDManagerSetDeviceMatching(manager.0, dict.0) };
        // SAFETY: valid manager; result follows the copy rule (owned) and may be NULL.
        let set = Cf::new(unsafe { IOHIDManagerCopyDevices(manager.0) });
        match set {
            Some(set) => Ok(Self {
                _manager: manager,
                set,
            }),
            None => Err(HidError::NotFound),
        }
    }

    fn devices(&self) -> Result<Vec<CFTypeRef>, HidError> {
        // SAFETY: valid set.
        let n = unsafe { CFSetGetCount(self.set.0) };
        if !(0..=MAX_SCAN).contains(&n) {
            return Err(HidError::Protocol("implausible device count"));
        }
        let mut v = vec![null(); n as usize];
        // SAFETY: `v` has room for `n` values.
        unsafe { CFSetGetValues(self.set.0, v.as_mut_ptr()) };
        Ok(v)
    }
}

pub(super) struct IoKitHid {
    capability: crate::native::windows_worker::HidWorkerCapability,
}

fn path_for(id: u64) -> String {
    format!("iokit:{id}")
}

fn describe(dev: CFTypeRef, identity: HidIdentity) -> Option<HidDescriptor> {
    let actual = HidIdentity {
        vendor_id: u16::try_from(prop_i32(dev, "VendorID")?).ok()?,
        product_id: u16::try_from(prop_i32(dev, "ProductID")?).ok()?,
    };
    if actual != identity {
        return None;
    }
    let id = entry_id(dev)?;
    let size = |k| {
        prop_i32(dev, k)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0)
    };
    let page = |k| {
        prop_i32(dev, k)
            .and_then(|v| u16::try_from(v).ok())
            .unwrap_or(0)
    };
    Some(HidDescriptor {
        identity,
        usage_page: page("PrimaryUsagePage"),
        usage: page("PrimaryUsage"),
        product: prop_string(dev, "Product"),
        serial: prop_string(dev, "SerialNumber"),
        max_input_report_len: size("MaxInputReportSize"),
        max_output_report_len: size("MaxOutputReportSize"),
        interface_number: interface_number(dev, identity),
        bus: prop_string(dev, "Transport").map(|transport| classify_transport(&transport)),
        location_id: prop_u32(dev, "LocationID"),
        path: HidPath::new(path_for(id)).ok()?,
    })
}

fn same_open_identity(
    reopened: &HidDescriptor,
    selected: &HidDescriptor,
    allow_usage_change: bool,
) -> bool {
    let usage_matches = allow_usage_change
        || (reopened.usage_page == selected.usage_page && reopened.usage == selected.usage);
    reopened.identity == selected.identity
        && reopened.interface_number == selected.interface_number
        && reopened.bus == selected.bus
        && reopened.location_id == selected.location_id
        && reopened.max_input_report_len == selected.max_input_report_len
        && reopened.max_output_report_len == selected.max_output_report_len
        && reopened.product == selected.product
        && reopened.path == selected.path
        && usage_matches
}

impl HidBackend for IoKitHid {
    fn enumerate(&self, m: &HidMatcher, limit: usize) -> Result<Vec<HidDescriptor>, HidError> {
        self.authorize()?;
        let matched = match Matched::new(m.identity) {
            Ok(x) => x,
            Err(HidError::NotFound) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut found = Vec::new();
        for dev in matched.devices()? {
            let Some(d) = describe(dev, m.identity) else {
                continue;
            };
            if m.matches(&d) {
                if found.len() == limit {
                    return Err(HidError::TooManyDevices);
                }
                found.push(d);
            }
        }
        Ok(found)
    }

    fn open(&self, d: &HidDescriptor) -> Result<Box<dyn HidTransport>, HidError> {
        self.open_with_policy(d, false, None)
    }

    fn open_filtered(
        &self,
        d: &HidDescriptor,
        reports: &[(u8, usize)],
    ) -> Result<Box<dyn HidTransport>, HidError> {
        self.open_with_policy(d, false, Some(reports.to_vec()))
    }

    fn open_allowing_usage_change(
        &self,
        d: &HidDescriptor,
    ) -> Result<Box<dyn HidTransport>, HidError> {
        self.open_with_policy(d, true, None)
    }

    fn original_report_descriptor(&self, d: &HidDescriptor) -> Result<Vec<u8>, HidError> {
        self.authorize()?;
        let matched = Matched::new(d.identity)?;
        let dev = self.find_device(&matched, d, false)?;
        original_report_descriptor(dev).ok_or(HidError::EvidenceUnavailable(
            EvidenceKind::RawReportDescriptor,
        ))
    }

    fn original_report_descriptor_evidence(
        &self,
        d: &HidDescriptor,
    ) -> Result<ReportDescriptorEvidence, HidError> {
        self.authorize()?;
        let matched = Matched::new(d.identity)?;
        let dev = self.find_device(&matched, d, false)?;
        let (property_type, bytes) = original_report_descriptor_property(dev);
        Ok(ReportDescriptorEvidence {
            bytes: bytes.unwrap_or_default(),
            source_path: path_for(entry_id(dev).ok_or(HidError::NotFound)?),
            property_type: property_type.into(),
        })
    }

    fn open_exclusive_probe(&self, d: &HidDescriptor) -> Result<ExclusiveOpenProbe, HidError> {
        self.authorize()?;
        if !micro_exclusive_candidate(d) {
            return Err(HidError::Protocol("unqualified Micro transport/interface"));
        }
        let report_descriptor = self.original_report_descriptor(d)?;
        verify_micro_descriptor(d, &report_descriptor)?;
        let matched = Matched::new(d.identity)?;
        let dev = self.find_device(&matched, d, false)?;
        // SAFETY: `dev` is a live element of the matched set; retained for close.
        let dev = Cf(unsafe { CFRetain(dev) });
        exclusive_open_probe_sequence(
            dev.0,
            d,
            |device, options| {
                // SAFETY: valid device; options are caller-selected constants.
                unsafe { IOHIDDeviceOpen(device, options) }
            },
            |device, options| {
                // SAFETY: valid opened device; close has no seize option.
                unsafe { IOHIDDeviceClose(device, options) }
            },
            |device| describe(device, d.identity),
        )
    }
}

impl IoKitHid {
    pub(super) fn new() -> Result<Self, HidError> {
        let capability = crate::native::windows_worker::hid_worker_capability()
            .ok_or(HidError::Unavailable(UnavailableReason::UnsupervisedAccess))?;
        Ok(Self { capability })
    }

    #[cfg(feature = "mx-creative-hid")]
    pub(super) fn new_for_mx_creative_review() -> Result<Self, HidError> {
        let capability = crate::native::windows_worker::review_hid_worker_capability()
            .ok_or(HidError::Unavailable(UnavailableReason::UnsupervisedAccess))?;
        Ok(Self { capability })
    }

    fn authorize(&self) -> Result<(), HidError> {
        if self.capability.valid() {
            Ok(())
        } else {
            Err(HidError::Unavailable(UnavailableReason::UnsupervisedAccess))
        }
    }

    fn open_with_policy(
        &self,
        d: &HidDescriptor,
        allow_usage_change: bool,
        filter: Option<Vec<(u8, usize)>>,
    ) -> Result<Box<dyn HidTransport>, HidError> {
        self.authorize()?;
        let matched = Matched::new(d.identity)?;
        let dev = self.find_device(&matched, d, allow_usage_change)?;
        // SAFETY: `dev` is a live element of the matched set; retained for our lifetime.
        let dev = unsafe { CFRetain(dev) };
        let dev = Cf(dev);
        // SAFETY: valid device; no seize option.
        let r = unsafe { IOHIDDeviceOpen(dev.0, 0) };
        if r != 0 {
            return Err(HidError::Os {
                op: HidOp::Open,
                code: i64::from(r),
            });
        }
        if !describe(dev.0, d.identity)
            .is_some_and(|reopened| same_open_identity(&reopened, d, allow_usage_change))
        {
            unsafe { IOHIDDeviceClose(dev.0, 0) };
            return Err(HidError::NotFound);
        }
        let mut state = Box::new(InputState {
            buf: vec![0u8; MAX_REPORT_LEN],
            queue: VecDeque::new(),
            filter,
        });
        let ctx = (&raw mut *state).cast::<c_void>();
        // SAFETY: `state` (and its fixed-size buffer) outlives the registration: Drop
        // unregisters before freeing. The callback only touches `*ctx`.
        unsafe {
            IOHIDDeviceRegisterInputReportCallback(
                dev.0,
                state.buf.as_mut_ptr(),
                MAX_REPORT_LEN as CFIndex,
                Some(on_input),
                ctx,
            );
            IOHIDDeviceScheduleWithRunLoop(dev.0, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
        }
        let report_descriptor = original_report_descriptor(dev.0);
        Ok(Box::new(MacTransport {
            dev,
            _matched: matched,
            run_loop: unsafe { CFRunLoopGetCurrent() },
            state,
            report_descriptor,
            capability: self.capability.clone(),
        }))
    }

    fn find_device(
        &self,
        matched: &Matched,
        descriptor: &HidDescriptor,
        allow_usage_change: bool,
    ) -> Result<CFTypeRef, HidError> {
        matched
            .devices()?
            .into_iter()
            .find(|&device| {
                describe(device, descriptor.identity).is_some_and(|candidate| {
                    same_open_identity(&candidate, descriptor, allow_usage_change)
                })
            })
            .ok_or(HidError::NotFound)
    }
}

fn micro_exclusive_candidate(descriptor: &HidDescriptor) -> bool {
    descriptor.identity
        == HidIdentity {
            vendor_id: APPLICATION_VID,
            product_id: APPLICATION_PID,
        }
        && descriptor.usage_page == USAGE_PAGE
        && descriptor.usage == USAGE
        && descriptor.interface_number == Some(2)
        && descriptor.bus == Some(HidBus::Usb)
        && descriptor.max_input_report_len == REPORT_SIZE
        && descriptor.max_output_report_len == REPORT_SIZE
}

fn exclusive_open_probe_sequence(
    device: CFTypeRef,
    selected: &HidDescriptor,
    open: impl FnOnce(CFTypeRef, u32) -> IOReturn,
    close: impl FnOnce(CFTypeRef, u32) -> IOReturn,
    describe_opened: impl FnOnce(CFTypeRef) -> Option<HidDescriptor>,
) -> Result<ExclusiveOpenProbe, HidError> {
    let open_code = open(device, K_IO_HID_OPTIONS_TYPE_SEIZE_DEVICE);
    if open_code != 0 {
        return Ok(ExclusiveOpenProbe {
            requested: true,
            acquired: false,
            released: false,
            open_code: i64::from(open_code),
            close_code: None,
        });
    }
    let reopened_ok = describe_opened(device)
        .is_some_and(|reopened| same_open_identity(&reopened, selected, false));
    let close_code = close(device, 0);
    if close_code != 0 {
        return Err(HidError::Os {
            op: HidOp::Close,
            code: i64::from(close_code),
        });
    }
    if !reopened_ok {
        return Err(HidError::NotFound);
    }
    Ok(ExclusiveOpenProbe {
        requested: true,
        acquired: true,
        released: true,
        open_code: 0,
        close_code: Some(0),
    })
}

struct InputState {
    buf: Vec<u8>,
    queue: VecDeque<Vec<u8>>,
    filter: Option<Vec<(u8, usize)>>,
}

extern "C" fn on_input(
    ctx: *mut c_void,
    result: IOReturn,
    _sender: *mut c_void,
    _ty: u32,
    id: u32,
    report: *mut u8,
    len: CFIndex,
) {
    if result != 0 || ctx.is_null() || report.is_null() || len <= 0 || len as usize > MAX_REPORT_LEN
    {
        return;
    }
    // SAFETY: ctx is the InputState registered at open and alive until unregistered;
    // IOKit guarantees `report` is valid for `len` bytes during the callback.
    let (state, data) = unsafe {
        (
            &mut *ctx.cast::<InputState>(),
            std::slice::from_raw_parts(report, len as usize),
        )
    };
    if state.queue.len() >= MAX_QUEUED {
        return;
    }
    let mut out = Vec::with_capacity(data.len() + 1);
    // Normalise to Windows-style framing: report-ID byte first (0 if unnumbered).
    if data.first().copied().map(u32::from) != Some(id) || id == 0 {
        out.push(id as u8);
    }
    out.extend_from_slice(data);
    if out.len() <= MAX_REPORT_LEN
        && state
            .filter
            .as_ref()
            .is_none_or(|filter| filter.contains(&(id as u8, out.len())))
    {
        state.queue.push_back(out);
    }
}

struct MacTransport {
    dev: Cf,
    _matched: Matched,
    run_loop: CFTypeRef,
    state: Box<InputState>,
    report_descriptor: Option<Vec<u8>>,
    capability: crate::native::windows_worker::HidWorkerCapability,
}

fn original_report_descriptor(dev: CFTypeRef) -> Option<Vec<u8>> {
    original_report_descriptor_property(dev).1
}

fn original_report_descriptor_property(dev: CFTypeRef) -> (&'static str, Option<Vec<u8>>) {
    let Some(key) = cfstr("ReportDescriptor") else {
        return ("key_creation_failed", None);
    };
    // SAFETY: borrowed property from a live IOHIDDevice.
    let data = unsafe { IOHIDDeviceGetProperty(dev, key.0) };
    if data.is_null() {
        return ("missing", None);
    }
    if unsafe { CFGetTypeID(data) != CFDataGetTypeID() } {
        let property_type = if unsafe { CFGetTypeID(data) == CFStringGetTypeID() } {
            "cfstring"
        } else if unsafe { CFGetTypeID(data) == CFNumberGetTypeID() } {
            "cfnumber"
        } else {
            "other"
        };
        return (property_type, None);
    }
    let len = unsafe { CFDataGetLength(data) };
    if !(1..=1024).contains(&len) {
        return ("cfdata_out_of_bounds", None);
    }
    let ptr = unsafe { CFDataGetBytePtr(data) };
    if ptr.is_null() {
        return ("cfdata_null", None);
    }
    // SAFETY: CFData owns `len` contiguous bytes and remains alive for this copy.
    (
        "cfdata",
        Some(unsafe { std::slice::from_raw_parts(ptr, len as usize) }.to_vec()),
    )
}

impl Drop for MacTransport {
    fn drop(&mut self) {
        // SAFETY: unregister callbacks before `state` is freed, then unschedule and close.
        unsafe {
            IOHIDDeviceRegisterInputReportCallback(
                self.dev.0,
                self.state.buf.as_mut_ptr(),
                MAX_REPORT_LEN as CFIndex,
                None,
                null_mut(),
            );
            IOHIDDeviceUnscheduleFromRunLoop(self.dev.0, self.run_loop, kCFRunLoopDefaultMode);
            IOHIDDeviceClose(self.dev.0, 0);
        }
    }
}

impl HidTransport for MacTransport {
    fn supports_write(&self) -> bool {
        self.capability.valid()
    }

    fn write(&mut self, report: &[u8]) -> Result<usize, HidError> {
        output_report(&self.capability, report, |id, bytes| {
            // The capability is checked immediately before this synchronous FFI.
            unsafe {
                IOHIDDeviceSetReport(self.dev.0, 1, id, bytes.as_ptr(), bytes.len() as CFIndex)
            }
        })
    }

    fn read(&mut self, out: &mut [u8], timeout: Duration) -> Result<usize, HidError> {
        if !self.capability.valid() {
            return Err(HidError::Unavailable(UnavailableReason::UnsupervisedAccess));
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(r) = self.state.queue.pop_front() {
                let n = r.len().min(out.len());
                out[..n].copy_from_slice(&r[..n]);
                return Ok(n);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(HidError::Timeout);
            }
            run_loop_slice(left, |seconds| {
                // SAFETY: services this thread's run loop for at most `left`.
                unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 1) }
            })?;
        }
    }

    fn report_descriptor(&mut self) -> Result<Vec<u8>, HidError> {
        if !self.capability.valid() {
            return Err(HidError::Unavailable(UnavailableReason::UnsupervisedAccess));
        }
        self.report_descriptor
            .clone()
            .ok_or(HidError::EvidenceUnavailable(
                EvidenceKind::RawReportDescriptor,
            ))
    }
}

fn run_loop_slice(timeout: Duration, run: impl FnOnce(f64) -> i32) -> Result<(), HidError> {
    match run(timeout.as_secs_f64()) {
        1 | 2 => Err(HidError::Protocol("HID run loop stopped or finished")),
        3 => Err(HidError::Timeout),
        4 => Ok(()),
        _ => Err(HidError::Protocol("unexpected HID run loop result")),
    }
}

fn output_report(
    capability: &crate::native::windows_worker::HidWorkerCapability,
    report: &[u8],
    set_report: impl FnOnce(CFIndex, &[u8]) -> IOReturn,
) -> Result<usize, HidError> {
    if !capability.valid() {
        return Err(HidError::Unavailable(UnavailableReason::UnsupervisedWrite));
    }
    let Some(&id) = report.first() else {
        return Err(HidError::InvalidArgument(ArgumentError::EmptyReport));
    };
    if report.len() > MAX_REPORT_LEN {
        return Err(HidError::InvalidArgument(ArgumentError::ReportTooLarge));
    }
    // IOKit includes numbered IDs in the report but not the synthetic zero ID.
    let bytes = if id == 0 { &report[1..] } else { report };
    let result = set_report(CFIndex::from(id), bytes);
    if result != 0 {
        return Err(HidError::Os {
            op: HidOp::Write,
            code: i64::from(result),
        });
    }
    Ok(report.len())
}

#[cfg(test)]
pub(crate) fn fixture_output(
    capability: &crate::native::windows_worker::HidWorkerCapability,
    report: &[u8],
    set_report: impl FnOnce(isize, &[u8]) -> i32,
) -> Result<usize, HidError> {
    output_report(capability, report, set_report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::HashMap, rc::Rc};

    #[test]
    fn transport_classification_accepts_only_documented_exact_names() {
        for (transport, expected) in [
            ("USB", HidBus::Usb),
            ("usb", HidBus::Usb),
            ("Bluetooth", HidBus::Bluetooth),
            ("bluetooth", HidBus::Bluetooth),
            ("Bluetooth Low Energy", HidBus::Bluetooth),
            ("BLUETOOTH LOW ENERGY", HidBus::Bluetooth),
            ("Bluetooth Low Energy 2", HidBus::Unknown),
            ("Low Energy", HidBus::Unknown),
            ("USB Low Energy", HidBus::Unknown),
        ] {
            assert_eq!(classify_transport(transport), expected, "{transport}");
        }
    }

    #[test]
    fn input_callback_retains_only_exact_vendor_frames_with_a_bounded_queue() {
        let mut state = Box::new(InputState {
            buf: vec![0; MAX_REPORT_LEN],
            queue: VecDeque::new(),
            filter: Some(vec![(17, 20)]),
        });
        let context = (&raw mut *state).cast::<c_void>();
        let mut mouse = [2, 0, 0];
        on_input(
            context,
            0,
            null_mut(),
            0,
            2,
            mouse.as_mut_ptr(),
            mouse.len() as CFIndex,
        );
        assert!(state.queue.is_empty());

        let mut vendor = [17; 20];
        for index in 0..65 {
            vendor[1] = index;
            on_input(
                context,
                0,
                null_mut(),
                0,
                17,
                vendor.as_mut_ptr(),
                vendor.len() as CFIndex,
            );
        }
        assert_eq!(state.queue.len(), MAX_QUEUED);
        assert_eq!(state.queue.front().unwrap()[0], 17);
        assert_eq!(state.queue.back().unwrap()[1], 63);
    }

    #[test]
    fn stopped_or_finished_run_loop_fails_without_spinning_or_hardware() {
        for status in [1, 2, 99] {
            let mut calls = 0;
            let error = run_loop_slice(Duration::from_millis(50), |_| {
                calls += 1;
                status
            })
            .unwrap_err();
            assert!(matches!(error, HidError::Protocol(_)));
            assert_eq!(calls, 1);
        }
        assert_eq!(
            run_loop_slice(Duration::from_millis(50), |_| 3),
            Err(HidError::Timeout),
        );
        assert_eq!(run_loop_slice(Duration::from_millis(50), |_| 4), Ok(()));
    }

    #[test]
    fn null_wrappers_do_not_construct_releasing_drops() {
        assert!(Cf::new(null()).is_none());
        assert!(IoOwned::new(0).is_none());
    }

    #[derive(Clone, Copy)]
    struct TestNode {
        registry_id: Option<u64>,
        is_usb_interface: bool,
        is_usb_device: bool,
        interface_number: Option<i32>,
        vendor_id: Option<i32>,
        product_id: Option<i32>,
        parent: Option<u32>,
    }

    fn test_node(
        registry_id: Option<u64>,
        is_usb_interface: bool,
        is_usb_device: bool,
        interface_number: Option<i32>,
        vendor_id: Option<i32>,
        product_id: Option<i32>,
        parent: Option<u32>,
    ) -> TestNode {
        TestNode {
            registry_id,
            is_usb_interface,
            is_usb_device,
            interface_number,
            vendor_id,
            product_id,
            parent,
        }
    }

    #[derive(Clone, Copy)]
    struct TestParent(u32);

    impl HeldNode for TestParent {
        fn raw(&self) -> u32 {
            self.0
        }
    }

    fn find_interface(
        start: u32,
        identity: HidIdentity,
        nodes: &HashMap<u32, TestNode>,
    ) -> Option<i32> {
        interface_number_from_ancestry(start, identity, |entry| {
            nodes.get(&entry).map(|n| AncestorEvidence {
                registry_id: n.registry_id,
                is_usb_interface: n.is_usb_interface,
                is_usb_device: n.is_usb_device,
                interface_number: n.interface_number,
                vendor_id: n.vendor_id,
                product_id: n.product_id,
                parent: n.parent.map(TestParent),
            })
        })
    }

    #[test]
    fn ancestor_usb_interface_number_is_used_when_identity_matches() {
        let identity = HidIdentity {
            vendor_id: 0x303a,
            product_id: 0x8298,
        };
        let mut nodes = HashMap::new();
        nodes.insert(
            1,
            test_node(Some(11), false, false, None, None, None, Some(2)),
        );
        nodes.insert(
            2,
            test_node(Some(12), false, false, None, None, None, Some(3)),
        );
        nodes.insert(
            3,
            test_node(
                Some(13),
                true,
                false,
                Some(2),
                Some(i32::from(identity.vendor_id)),
                Some(i32::from(identity.product_id)),
                Some(4),
            ),
        );
        assert_eq!(find_interface(1, identity, &nodes), Some(2));
    }

    #[test]
    fn usb_interface_bounds_are_enforced() {
        let identity = HidIdentity {
            vendor_id: 0x303a,
            product_id: 0x8298,
        };
        let mut nodes = HashMap::new();
        nodes.insert(
            1,
            test_node(
                Some(1),
                true,
                false,
                Some(2),
                Some(i32::from(identity.vendor_id)),
                Some(i32::from(identity.product_id)),
                None,
            ),
        );
        assert_eq!(find_interface(1, identity, &nodes), Some(2));
        nodes.insert(
            1,
            test_node(
                Some(1),
                true,
                false,
                Some(256),
                Some(i32::from(identity.vendor_id)),
                Some(i32::from(identity.product_id)),
                None,
            ),
        );
        assert_eq!(find_interface(1, identity, &nodes), None);
    }

    #[test]
    fn ancestor_usb_interface_mismatch_or_missing_identity_is_rejected() {
        let identity = HidIdentity {
            vendor_id: 0x303a,
            product_id: 0x8298,
        };
        let mut mismatched = HashMap::new();
        mismatched.insert(
            1,
            test_node(
                Some(1),
                true,
                false,
                Some(2),
                Some(i32::from(identity.vendor_id)),
                Some(0x1234),
                None,
            ),
        );
        assert_eq!(find_interface(1, identity, &mismatched), None);

        let mut missing = HashMap::new();
        missing.insert(
            1,
            test_node(
                Some(1),
                true,
                false,
                Some(2),
                Some(i32::from(identity.vendor_id)),
                None,
                None,
            ),
        );
        assert_eq!(find_interface(1, identity, &missing), None);
    }

    #[test]
    fn malformed_or_unrelated_ancestry_is_unavailable() {
        let identity = HidIdentity {
            vendor_id: 0x303a,
            product_id: 0x8298,
        };
        let mut cycle = HashMap::new();
        cycle.insert(
            1,
            test_node(Some(7), false, false, None, None, None, Some(2)),
        );
        cycle.insert(
            2,
            test_node(Some(7), false, false, None, None, None, Some(1)),
        );
        assert_eq!(find_interface(1, identity, &cycle), None);

        let mut unrelated = HashMap::new();
        unrelated.insert(
            1,
            test_node(
                Some(10),
                false,
                false,
                Some(9),
                Some(i32::from(identity.vendor_id)),
                Some(i32::from(identity.product_id)),
                None,
            ),
        );
        assert_eq!(find_interface(1, identity, &unrelated), None);

        let mut too_deep = HashMap::new();
        for id in 1..=17 {
            let parent = (id < 17).then_some(id + 1);
            too_deep.insert(
                id,
                test_node(Some(id as u64), false, false, None, None, None, parent),
            );
        }
        too_deep.insert(
            17,
            test_node(
                Some(17),
                true,
                false,
                Some(2),
                Some(i32::from(identity.vendor_id)),
                Some(i32::from(identity.product_id)),
                None,
            ),
        );
        assert_eq!(find_interface(1, identity, &too_deep), None);

        let mut missing_registry_id = HashMap::new();
        missing_registry_id.insert(1, test_node(None, false, false, None, None, None, Some(2)));
        missing_registry_id.insert(
            2,
            test_node(
                Some(2),
                true,
                false,
                Some(2),
                Some(i32::from(identity.vendor_id)),
                Some(i32::from(identity.product_id)),
                None,
            ),
        );
        assert_eq!(find_interface(1, identity, &missing_registry_id), None);
    }

    #[test]
    fn traversal_stops_at_usb_device_boundary() {
        let identity = HidIdentity {
            vendor_id: 0x303a,
            product_id: 0x8298,
        };
        let mut nodes = HashMap::new();
        nodes.insert(
            1,
            test_node(Some(1), false, false, None, None, None, Some(2)),
        );
        nodes.insert(
            2,
            test_node(
                Some(2),
                false,
                true,
                None,
                Some(i32::from(identity.vendor_id)),
                Some(0x1234),
                Some(3),
            ),
        );
        nodes.insert(
            3,
            test_node(
                Some(3),
                true,
                false,
                Some(2),
                Some(i32::from(identity.vendor_id)),
                Some(i32::from(identity.product_id)),
                None,
            ),
        );
        assert_eq!(find_interface(1, identity, &nodes), None);
    }

    #[derive(Clone)]
    struct DroppableParent {
        raw: u32,
        released: Rc<RefCell<Vec<u32>>>,
    }

    impl HeldNode for DroppableParent {
        fn raw(&self) -> u32 {
            self.raw
        }
    }

    impl Drop for DroppableParent {
        fn drop(&mut self) {
            self.released.borrow_mut().push(self.raw);
        }
    }

    #[test]
    fn parent_handle_lives_through_node_visit() {
        let identity = HidIdentity {
            vendor_id: 0x303a,
            product_id: 0x8298,
        };
        let released = Rc::new(RefCell::new(Vec::<u32>::new()));
        let nodes = HashMap::from([
            (
                1u32,
                test_node(Some(1), false, false, None, None, None, Some(2)),
            ),
            (
                2u32,
                test_node(Some(2), false, false, None, None, None, Some(3)),
            ),
            (
                3u32,
                test_node(
                    Some(3),
                    true,
                    false,
                    Some(2),
                    Some(i32::from(identity.vendor_id)),
                    Some(i32::from(identity.product_id)),
                    None,
                ),
            ),
        ]);
        let interface = interface_number_from_ancestry(1, identity, |entry| {
            assert!(
                !released.borrow().contains(&entry),
                "current node handle was released before visit"
            );
            nodes.get(&entry).map(|n| AncestorEvidence {
                registry_id: n.registry_id,
                is_usb_interface: n.is_usb_interface,
                is_usb_device: n.is_usb_device,
                interface_number: n.interface_number,
                vendor_id: n.vendor_id,
                product_id: n.product_id,
                parent: n.parent.map(|raw| DroppableParent {
                    raw,
                    released: Rc::clone(&released),
                }),
            })
        });
        assert_eq!(interface, Some(2));
        let released = released.borrow();
        assert!(released.contains(&2));
        assert!(released.contains(&3));
    }

    #[test]
    fn direct_property_is_preferred_and_preserves_bluetooth_minus_one() {
        let identity = HidIdentity {
            vendor_id: 0x303a,
            product_id: 0x8298,
        };
        let mut called = false;
        let direct = resolve_interface_number::<TestParent>(Some(-1), 1, identity, |_| {
            called = true;
            None
        });
        assert_eq!(direct, Some(-1));
        assert!(!called);
    }

    fn micro_descriptor() -> HidDescriptor {
        HidDescriptor {
            identity: HidIdentity {
                vendor_id: APPLICATION_VID,
                product_id: APPLICATION_PID,
            },
            usage_page: USAGE_PAGE,
            usage: USAGE,
            product: None,
            serial: None,
            max_input_report_len: REPORT_SIZE,
            max_output_report_len: REPORT_SIZE,
            interface_number: Some(2),
            bus: Some(HidBus::Usb),
            location_id: None,
            path: HidPath::new("iokit-test").unwrap(),
        }
    }

    #[test]
    fn exclusive_entry_gate_rejects_non_micro_targets() {
        let descriptor = micro_descriptor();
        assert!(micro_exclusive_candidate(&descriptor));

        let mut wrong_interface = descriptor.clone();
        wrong_interface.interface_number = Some(0);
        assert!(!micro_exclusive_candidate(&wrong_interface));

        let mut wrong_bus = descriptor.clone();
        wrong_bus.bus = Some(HidBus::Bluetooth);
        assert!(!micro_exclusive_candidate(&wrong_bus));

        let mut wrong_usage = descriptor.clone();
        wrong_usage.usage_page = 0x01;
        assert!(!micro_exclusive_candidate(&wrong_usage));
    }

    #[test]
    fn exclusive_sequence_uses_seize_option_without_fallback_and_checks_close() {
        let descriptor = micro_descriptor();
        let mut open_calls = Vec::new();
        let mut close_calls = Vec::new();
        let denied = exclusive_open_probe_sequence(
            null(),
            &descriptor,
            |_device, options| {
                open_calls.push(options);
                -536_870_174
            },
            |_device, options| {
                close_calls.push(options);
                0
            },
            |_device| Some(descriptor.clone()),
        )
        .unwrap();
        assert_eq!(open_calls, vec![K_IO_HID_OPTIONS_TYPE_SEIZE_DEVICE]);
        assert!(close_calls.is_empty());
        assert_eq!(
            denied,
            ExclusiveOpenProbe {
                requested: true,
                acquired: false,
                released: false,
                open_code: -536_870_174,
                close_code: None,
            }
        );

        open_calls.clear();
        close_calls.clear();
        let close_error = exclusive_open_probe_sequence(
            null(),
            &descriptor,
            |_device, options| {
                open_calls.push(options);
                0
            },
            |_device, options| {
                close_calls.push(options);
                -536_870_160
            },
            |_device| Some(descriptor.clone()),
        )
        .unwrap_err();
        assert_eq!(open_calls, vec![K_IO_HID_OPTIONS_TYPE_SEIZE_DEVICE]);
        assert_eq!(close_calls, vec![0]);
        assert_eq!(
            close_error,
            HidError::Os {
                op: HidOp::Close,
                code: -536_870_160,
            }
        );
    }

    #[test]
    fn exclusive_sequence_revalidates_identity_before_reporting_success() {
        let descriptor = micro_descriptor();
        let mismatch = HidDescriptor {
            usage: USAGE + 1,
            ..descriptor.clone()
        };
        let error = exclusive_open_probe_sequence(
            null(),
            &descriptor,
            |_device, options| {
                assert_eq!(options, K_IO_HID_OPTIONS_TYPE_SEIZE_DEVICE);
                0
            },
            |_device, options| {
                assert_eq!(options, 0);
                0
            },
            |_device| Some(mismatch),
        )
        .unwrap_err();
        assert_eq!(error, HidError::NotFound);
    }
}
