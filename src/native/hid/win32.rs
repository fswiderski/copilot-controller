//! Windows HID backend over SetupAPI, hid.dll, and overlapped file I/O.
//! All raw FFI is private to this file.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_Parent, CR_NO_SUCH_VALUE, CR_SUCCESS, DIGCF_DEVICEINTERFACE,
    DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    SP_DEVINFO_DATA, SetupDiCreateDeviceInfoList, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
    SetupDiOpenDeviceInterfaceW,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HIDD_ATTRIBUTES, HIDP_CAPS, HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetHidGuid,
    HidD_GetPreparsedData, HidD_GetProductString, HidD_GetSerialNumberString, HidP_GetCaps,
};
use windows_sys::Win32::Devices::Properties::DEVPKEY_Device_BusTypeGuid;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
    WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use super::platform::parse_vid_pid;
use super::win32_evidence::{
    BusApi, BusEvidence, WindowsEvidence, collect_candidates, revalidate_open, scan_interfaces,
};
use super::*;
use crate::native::windows_worker::{HidWorkerCapability, hid_worker_capability};

const HIDP_STATUS_SUCCESS: i32 = 0x0011_0000;
/// Fixed write deadline; cancellation is best effort and unvalidated on hardware.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const STRING_BYTES: usize = 256;

pub(super) struct WindowsHid {
    capability: HidWorkerCapability,
    enumerated: RefCell<Vec<WindowsEvidence>>,
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by CreateFileW/CreateEventW and is owned here.
        unsafe { CloseHandle(self.0) };
    }
}

struct DevInfo(HDEVINFO);

impl Drop for DevInfo {
    fn drop(&mut self) {
        // SAFETY: owned set returned by SetupDiGetClassDevsW.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

fn os_err(op: HidOp) -> HidError {
    // SAFETY: trivially safe thread-local error read.
    HidError::Os {
        op,
        code: i64::from(unsafe { GetLastError() }),
    }
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn open_handle(path: &str, access: u32, flags: u32, op: HidOp) -> Result<Handle, HidError> {
    let w = wide(path);
    // SAFETY: `w` is NUL-terminated and outlives the call.
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            flags,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        Err(os_err(op))
    } else {
        Ok(Handle(h))
    }
}

fn attributes(h: &Handle, op: HidOp) -> Result<HidIdentity, HidError> {
    let mut a = HIDD_ATTRIBUTES {
        Size: size_of::<HIDD_ATTRIBUTES>() as u32,
        VendorID: 0,
        ProductID: 0,
        VersionNumber: 0,
    };
    // SAFETY: `a` is a valid, correctly sized out-structure.
    if !unsafe { HidD_GetAttributes(h.0, &mut a) } {
        return Err(os_err(op));
    }
    Ok(HidIdentity {
        vendor_id: a.VendorID,
        product_id: a.ProductID,
    })
}

fn caps(h: &Handle, op: HidOp) -> Result<HIDP_CAPS, HidError> {
    let mut pre = 0isize;
    // SAFETY: out-pointer is valid; the preparsed data is freed below on every path.
    if !unsafe { HidD_GetPreparsedData(h.0, &mut pre) } {
        return Err(os_err(op));
    }
    let mut c = HIDP_CAPS::default();
    // SAFETY: `pre` is valid preparsed data and `c` a valid out-structure.
    let st = unsafe { HidP_GetCaps(pre, &mut c) };
    // SAFETY: `pre` came from HidD_GetPreparsedData and is not used afterwards.
    unsafe { HidD_FreePreparsedData(pre) };
    if st == HIDP_STATUS_SUCCESS {
        Ok(c)
    } else {
        Err(HidError::Os {
            op,
            code: i64::from(st),
        })
    }
}

fn string_attr(
    h: &Handle,
    f: unsafe extern "system" fn(HANDLE, *mut core::ffi::c_void, u32) -> bool,
) -> Option<String> {
    let mut buf = [0u16; STRING_BYTES / 2];
    // SAFETY: buffer pointer/length describe `buf` exactly.
    if !unsafe { f(h.0, buf.as_mut_ptr().cast(), STRING_BYTES as u32) } {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..end]);
    (!s.is_empty()).then_some(s)
}

struct NativeBusApi {
    op: HidOp,
}

impl BusApi for NativeBusApi {
    fn property(
        &mut self,
        node: u32,
        buffer: &mut [u8; 16],
    ) -> Result<Option<(u32, usize)>, HidError> {
        let mut ty = 0;
        let mut len = buffer.len() as u32;
        // SAFETY: node came from SetupAPI/CM_Get_Parent and is local. The buffer is the
        // entire writable extent. No retry/allocation follows a larger size.
        let status = unsafe {
            CM_Get_DevNode_PropertyW(
                node,
                &DEVPKEY_Device_BusTypeGuid,
                &mut ty,
                buffer.as_mut_ptr(),
                &mut len,
                0,
            )
        };
        match status {
            CR_SUCCESS => Ok(Some((ty, len as usize))),
            CR_NO_SUCH_VALUE => Ok(None),
            code => Err(HidError::Os {
                op: self.op,
                code: i64::from(code),
            }),
        }
    }

    fn parent(&mut self, node: u32) -> Result<u32, HidError> {
        let mut parent = 0;
        // SAFETY: node is a local DEVINST; valid out-pointer, reserved flags zero.
        let status = unsafe { CM_Get_Parent(&mut parent, node, 0) };
        if status != CR_SUCCESS {
            return Err(HidError::Os {
                op: self.op,
                code: i64::from(status),
            });
        }
        Ok(parent)
    }
}

fn bus_evidence(device: &SP_DEVINFO_DATA, op: HidOp) -> Result<BusEvidence, HidError> {
    BusEvidence::read(device.DevInst, &mut NativeBusApi { op })
}

fn interface_detail(
    set: &DevInfo,
    interface: &SP_DEVICE_INTERFACE_DATA,
    op: HidOp,
) -> Result<(HidPath, SP_DEVINFO_DATA), HidError> {
    let mut needed = 0;
    // SAFETY: size query with a null buffer.
    let ok = unsafe {
        SetupDiGetDeviceInterfaceDetailW(set.0, interface, null_mut(), 0, &mut needed, null_mut())
    };
    if ok != 0 {
        return Err(HidError::Protocol(
            "HID detail size query unexpectedly succeeded",
        ));
    }
    if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
        return Err(os_err(op));
    }
    let needed = needed as usize;
    if !(size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>()..=4096).contains(&needed) {
        return Err(HidError::Protocol("HID detail size outside bounds"));
    }
    let mut buf = vec![0u32; needed.div_ceil(4)];
    let detail = buf.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
    // SAFETY: zero is valid for SP_DEVINFO_DATA, with cbSize set before use.
    let mut device: SP_DEVINFO_DATA = unsafe { core::mem::zeroed() };
    device.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
    let mut returned = 0;
    // SAFETY: buffer has the required alignment and capacity; device is a
    // correctly sized out-structure belonging to this interface's set.
    let ok = unsafe {
        (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        SetupDiGetDeviceInterfaceDetailW(
            set.0,
            interface,
            detail,
            needed as u32,
            &mut returned,
            &mut device,
        )
    };
    let returned = returned as usize;
    let offset = core::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
    if ok == 0 {
        return Err(os_err(op));
    }
    if returned > needed || returned < offset + 2 || !(returned - offset).is_multiple_of(2) {
        return Err(HidError::Protocol("invalid HID detail length"));
    }
    // SAFETY: the slice stays inside the successfully returned buffer extent.
    let path_ptr = unsafe { core::ptr::addr_of!((*detail).DevicePath) }.cast::<u16>();
    let slice = unsafe { core::slice::from_raw_parts(path_ptr, (returned - offset) / 2) };
    let end = slice
        .iter()
        .position(|&c| c == 0)
        .ok_or(HidError::Protocol("unterminated HID interface path"))?;
    let path = HidPath::new(
        String::from_utf16(&slice[..end])
            .map_err(|_| HidError::Protocol("invalid HID interface path UTF-16"))?,
    )?;
    Ok((path, device))
}

struct Interfaces {
    _set: DevInfo,
    entries: Vec<(HidPath, SP_DEVINFO_DATA)>,
}

fn interface_paths() -> Result<Interfaces, HidError> {
    let mut guid = windows_sys::core::GUID {
        data1: 0,
        data2: 0,
        data3: 0,
        data4: [0; 8],
    };
    // SAFETY: valid out-pointer.
    unsafe { HidD_GetHidGuid(&mut guid) };
    // SAFETY: GUID pointer is valid; null enumerator/parent are permitted.
    let set = unsafe {
        SetupDiGetClassDevsW(
            &guid,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if set as isize == -1 {
        return Err(os_err(HidOp::Enumerate));
    }
    let set = DevInfo(set);
    let entries = scan_interfaces(
        |index| {
            let mut ifd: SP_DEVICE_INTERFACE_DATA = unsafe { core::mem::zeroed() };
            ifd.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            // SAFETY: `ifd` is initialised with cbSize as required.
            if unsafe { SetupDiEnumDeviceInterfaces(set.0, null(), &guid, index, &mut ifd) } == 0 {
                return Err(unsafe { GetLastError() });
            }
            Ok(ifd)
        },
        |ifd| interface_detail(&set, &ifd, HidOp::Enumerate),
    )?;
    Ok(Interfaces { _set: set, entries })
}

fn reopened_bus(path: &HidPath) -> Result<BusEvidence, HidError> {
    // SAFETY: null class/parent creates an empty information set.
    let set = unsafe { SetupDiCreateDeviceInfoList(null(), null_mut()) };
    if set as isize == -1 {
        return Err(os_err(HidOp::Open));
    }
    let set = DevInfo(set);
    let path_wide = wide(path.as_str());
    // SAFETY: zero is valid, with cbSize set before use.
    let mut interface: SP_DEVICE_INTERFACE_DATA = unsafe { core::mem::zeroed() };
    interface.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
    // SAFETY: bounded, NUL-terminated path; valid set and out-structure.
    if unsafe { SetupDiOpenDeviceInterfaceW(set.0, path_wide.as_ptr(), 0, &mut interface) } == 0 {
        return Err(os_err(HidOp::Open));
    }
    let (actual_path, device) = interface_detail(&set, &interface, HidOp::Open)?;
    if actual_path != *path {
        return Err(HidError::NotFound);
    }
    bus_evidence(&device, HidOp::Open)
}

fn describe(
    h: &Handle,
    path: HidPath,
    bus: BusEvidence,
    identity: HidIdentity,
    op: HidOp,
) -> Result<WindowsEvidence, HidError> {
    let c = caps(h, op)?;
    Ok(WindowsEvidence {
        descriptor: HidDescriptor {
            identity,
            usage_page: c.UsagePage,
            usage: c.Usage,
            product: string_attr(h, HidD_GetProductString),
            serial: string_attr(h, HidD_GetSerialNumberString),
            max_input_report_len: usize::from(c.InputReportByteLength),
            max_output_report_len: usize::from(c.OutputReportByteLength),
            // No documented exact collection-handle source for bInterfaceNumber.
            interface_number: None,
            bus: bus.classification(),
            path,
        },
        bus,
    })
}

impl WindowsHid {
    pub(super) fn new() -> Result<Self, HidError> {
        let capability = hid_worker_capability()
            .ok_or(HidError::Unavailable(UnavailableReason::UnsupervisedAccess))?;
        Ok(Self {
            capability,
            enumerated: RefCell::new(Vec::new()),
        })
    }

    fn open_with_policy(
        &self,
        d: &HidDescriptor,
        allow_usage_change: bool,
    ) -> Result<Box<dyn HidTransport>, HidError> {
        self.capability.authorize()?;
        let expected = self
            .enumerated
            .borrow()
            .iter()
            .find(|entry| entry.descriptor == *d)
            .cloned()
            .ok_or(HidError::NotFound)?;
        let h = open_handle(
            d.path.as_str(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_FLAG_OVERLAPPED,
            HidOp::Open,
        )?;
        revalidate_open(&expected, allow_usage_change, || {
            describe(
                &h,
                d.path.clone(),
                reopened_bus(&d.path)?,
                attributes(&h, HidOp::Open)?,
                HidOp::Open,
            )
        })?;
        Ok(Box::new(WinTransport {
            h,
            in_len: d.max_input_report_len,
            out_len: d.max_output_report_len,
            capability: self.capability.clone(),
        }))
    }
}

impl HidBackend for WindowsHid {
    fn enumerate(&self, m: &HidMatcher, limit: usize) -> Result<Vec<HidDescriptor>, HidError> {
        self.capability.authorize()?;
        self.enumerated.borrow_mut().clear();
        if limit == 0 || limit > MAX_ENUMERATED_DEVICES {
            return Err(HidError::InvalidArgument(ArgumentError::LimitInvalid));
        }
        let interfaces = interface_paths()?;
        let evidence = collect_candidates(interfaces.entries, m, limit, |(path, device)| {
            // Only paths whose name carries the requested identity are touched.
            if parse_vid_pid(path.as_str()) != Some(m.identity) {
                return Ok(None);
            }
            // Access mask 0 queries attributes without read/write access.
            let h = open_handle(path.as_str(), 0, 0, HidOp::Enumerate)?;
            let identity = attributes(&h, HidOp::Enumerate)?;
            if identity != m.identity {
                return Ok(None);
            }
            describe(
                &h,
                path,
                bus_evidence(&device, HidOp::Enumerate)?,
                identity,
                HidOp::Enumerate,
            )
            .map(Some)
        })?;
        let found = evidence
            .iter()
            .map(|entry| entry.descriptor.clone())
            .collect();
        *self.enumerated.borrow_mut() = evidence;
        Ok(found)
    }

    fn open(&self, d: &HidDescriptor) -> Result<Box<dyn HidTransport>, HidError> {
        self.open_with_policy(d, false)
    }

    fn open_allowing_usage_change(
        &self,
        d: &HidDescriptor,
    ) -> Result<Box<dyn HidTransport>, HidError> {
        self.open_with_policy(d, true)
    }
}

struct WinTransport {
    h: Handle,
    in_len: usize,
    out_len: usize,
    capability: HidWorkerCapability,
}

impl WinTransport {
    /// Runs one overlapped transfer, cancelling it if `timeout` elapses.
    fn transfer(
        &self,
        op: HidOp,
        buf: *mut u8,
        len: usize,
        timeout: Duration,
    ) -> Result<usize, HidError> {
        self.capability.authorize()?;
        // SAFETY: manual-reset, unsignalled, unnamed event.
        let ev = unsafe { CreateEventW(null(), 1, 0, null()) };
        if ev.is_null() {
            return Err(os_err(op));
        }
        let ev = Handle(ev);
        let mut ov: OVERLAPPED = unsafe { core::mem::zeroed() };
        ov.hEvent = ev.0;
        let mut n = 0u32;
        // SAFETY: `buf` is valid for `len` bytes and, with `ov`, stays alive until the
        // operation completes: we always wait for completion after cancelling.
        let ok = unsafe {
            match op {
                HidOp::Write => WriteFile(self.h.0, buf, len as u32, null_mut(), &mut ov),
                _ => ReadFile(self.h.0, buf, len as u32, null_mut(), &mut ov),
            }
        };
        if ok == 0 {
            if unsafe { GetLastError() } != ERROR_IO_PENDING {
                return Err(os_err(op));
            }
            let ms = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
            // SAFETY: valid event handle.
            if unsafe { WaitForSingleObject(ev.0, ms) } != WAIT_OBJECT_0 {
                // SAFETY: cancel then block until the kernel is done with buf/ov.
                unsafe {
                    CancelIoEx(self.h.0, &ov);
                    GetOverlappedResult(self.h.0, &ov, &mut n, 1);
                }
                return Err(HidError::Timeout);
            }
        }
        // SAFETY: operation has completed.
        if unsafe { GetOverlappedResult(self.h.0, &ov, &mut n, 0) } == 0 {
            return Err(os_err(op));
        }
        Ok(n as usize)
    }
}

impl HidTransport for WinTransport {
    fn supports_write(&self) -> bool {
        self.capability.valid()
    }

    fn write(&mut self, report: &[u8]) -> Result<usize, HidError> {
        self.capability.authorize()?;
        // Windows requires exactly the device's output report length; zero-pad.
        let total = if self.out_len == 0 {
            report.len()
        } else {
            self.out_len
        };
        if report.len() > total {
            return Err(HidError::InvalidArgument(
                ArgumentError::ReportExceedsDevice,
            ));
        }
        let mut buf = vec![0u8; total];
        buf[..report.len()].copy_from_slice(report);
        let n = self.transfer(HidOp::Write, buf.as_mut_ptr(), total, WRITE_TIMEOUT)?;
        Ok(n.min(report.len()))
    }

    fn read(&mut self, out: &mut [u8], timeout: Duration) -> Result<usize, HidError> {
        self.capability.authorize()?;
        // Windows requires a buffer of the full input report length.
        let total = if self.in_len == 0 {
            out.len()
        } else {
            self.in_len
        };
        if total > MAX_REPORT_LEN {
            return Err(HidError::Protocol("input report too large"));
        }
        let mut buf = vec![0u8; total];
        let n = self
            .transfer(HidOp::Read, buf.as_mut_ptr(), total, timeout)?
            .min(total);
        let n = n.min(out.len());
        out[..n].copy_from_slice(&buf[..n]);
        Ok(n)
    }

    fn report_descriptor(&mut self) -> Result<Vec<u8>, HidError> {
        self.capability.authorize()?;
        // IOCTL_HID_GET_REPORT_DESCRIPTOR is a HID minidriver request, not a
        // supported user-mode collection API. Preparsed data is not raw bytes.
        Err(HidError::EvidenceUnavailable(
            EvidenceKind::RawReportDescriptor,
        ))
    }
}
