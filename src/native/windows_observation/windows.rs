mod desktop;
mod image;
mod process;
mod qualification;
mod security;
mod serial;
mod ui;

pub use desktop::observe_desktop;
pub use process::{
    observe_current_process, observe_foreground, observe_process, observe_process_inventory,
};
pub use qualification::{observe_console_membership, observe_exact_process, observe_ui_structure};
pub use serial::observe_serial;

use super::{PlatformError, windows_native_error};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn native_error(operation: &'static str, code: u32) -> PlatformError {
    windows_native_error(operation, code)
}

fn last_error(operation: &'static str) -> PlatformError {
    native_error(operation, unsafe { GetLastError() })
}
