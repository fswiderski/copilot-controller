use super::super::{
    ImageFileEvidence, PlatformResult,
    qualification::{ImageFileBackend, ImageFileRecord, collect_image_file},
};
use super::{
    Handle, last_error,
    process::{HeldProcess, check_running, image_path},
};
use std::ptr;
use windows_sys::Win32::{
    Foundation::{FILETIME, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
        GetFileInformationByHandle, OPEN_EXISTING,
    },
};

struct NativeImage;

impl ImageFileBackend for NativeImage {
    type Process = HeldProcess;
    type File = Handle;

    fn check_running(&mut self, process: &Self::Process) -> PlatformResult<()> {
        check_running(process)
    }

    fn image_path(&mut self, process: &Self::Process) -> PlatformResult<Vec<u16>> {
        image_path(process)
    }

    fn open_attributes(&mut self, path: &[u16]) -> PlatformResult<Self::File> {
        let mut terminated = path.to_vec();
        terminated.push(0);
        let handle = unsafe {
            CreateFileW(
                terminated.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ,
                ptr::null(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_error("CreateFileW_image_attributes"));
        }
        Ok(Handle(handle))
    }

    fn information(&mut self, file: &Self::File) -> PlatformResult<ImageFileRecord> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.0, &mut info) } == 0 {
            return Err(last_error("GetFileInformationByHandle"));
        }
        fn time(value: FILETIME) -> u64 {
            (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
        }
        Ok(ImageFileRecord {
            attributes: info.dwFileAttributes,
            creation_time: time(info.ftCreationTime),
            last_access_time: time(info.ftLastAccessTime),
            last_write_time: time(info.ftLastWriteTime),
            volume_serial: info.dwVolumeSerialNumber,
            size_high: info.nFileSizeHigh,
            size_low: info.nFileSizeLow,
            link_count: info.nNumberOfLinks,
            index_high: info.nFileIndexHigh,
            index_low: info.nFileIndexLow,
        })
    }
}

pub(super) fn observe_image_file(
    observer: &HeldProcess,
    target: &HeldProcess,
) -> PlatformResult<ImageFileEvidence> {
    collect_image_file(&mut NativeImage, observer, target)
}
