use super::super::*;
use super::{
    desktop::Desktop,
    image::observe_image_file,
    last_error,
    process::{Foreground, HeldProcess, check_running, hold_process, inspect_held},
    security::observe_security,
};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::GetLastError,
    System::{
        Console::{AttachConsole, FreeConsole, GetConsoleProcessList, GetConsoleWindow},
        Threading::GetCurrentProcessId,
    },
};

pub(super) struct Backend<'a> {
    pub(super) epoch: Uuid,
    pub(super) names: ExecutableNames<'a>,
}

impl ProcessProbeBackend for Backend<'_> {
    type Held = HeldProcess;

    fn stamp(&mut self) -> PlatformResult<MonotonicStamp> {
        Ok(observe_clock(self.epoch)?.stamp)
    }

    fn current_pid(&mut self) -> u32 {
        unsafe { GetCurrentProcessId() }
    }

    fn hold(&mut self, pid: u32) -> PlatformResult<Self::Held> {
        hold_process(pid, self.epoch)
    }

    fn evidence(&mut self, process: &Self::Held) -> PlatformResult<ProcessEvidence> {
        inspect_held(process, self.names)
    }

    fn check_running(&mut self, process: &Self::Held) -> PlatformResult<()> {
        check_running(process)
    }

    fn security(
        &mut self,
        observer: &Self::Held,
        target: &Self::Held,
    ) -> PlatformResult<SecurityEvidence> {
        observe_security(observer, target)
    }
}

impl ExactProcessBackend for Backend<'_> {
    fn image_file(
        &mut self,
        observer: &Self::Held,
        target: &Self::Held,
    ) -> PlatformResult<ImageFileEvidence> {
        observe_image_file(observer, target)
    }

    fn foreground(&mut self) -> PlatformResult<ForegroundEvidence> {
        collect_foreground(&mut Foreground {
            epoch: self.epoch,
            names: self.names,
        })
    }

    fn desktop(&mut self) -> PlatformResult<DesktopContext> {
        collect_desktop(&mut Desktop { epoch: self.epoch })
    }
}

pub fn observe_exact_process(
    request: &ProcessRequest,
    names: ExecutableNames<'_>,
) -> Observation<ExactProcessReport> {
    collect_exact_process(
        &mut Backend {
            epoch: request.requested_at.epoch,
            names,
        },
        request,
    )
    .into()
}

pub fn observe_ui_structure(
    request: &UiRequest,
    names: ExecutableNames<'_>,
) -> Observation<UiStructureReport> {
    super::ui::observe(request, names)
}

pub(super) struct OwnedConsole<'a> {
    epoch: Uuid,
    names: ExecutableNames<'a>,
    attached: bool,
}

impl Drop for OwnedConsole<'_> {
    fn drop(&mut self) {
        if self.attached {
            unsafe { FreeConsole() };
        }
    }
}

impl ConsoleAttachment for OwnedConsole<'_> {
    type Held = HeldProcess;

    fn process_ids(&mut self) -> PlatformResult<Vec<u32>> {
        let mut ids = [0u32; 128];
        let count = unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) };
        if count == 0 {
            return Err(PlatformError::with_code(
                ErrorKind::NativeFailure,
                "GetConsoleProcessList",
                unsafe { GetLastError() },
            ));
        }
        if count as usize > ids.len() {
            return Err(PlatformError::new(
                ErrorKind::LimitExceeded,
                "console_process_list",
            ));
        }
        Ok(ids[..count as usize].to_vec())
    }

    fn console_window(&mut self) -> PlatformResult<Option<u64>> {
        let window = unsafe { GetConsoleWindow() };
        Ok((!window.is_null()).then_some(window as usize as u64))
    }

    fn hold(&mut self, pid: u32) -> PlatformResult<Self::Held> {
        hold_process(pid, self.epoch)
    }

    fn evidence(&mut self, process: &Self::Held) -> PlatformResult<ProcessEvidence> {
        inspect_held(process, self.names)
    }

    fn check_running(&mut self, process: &Self::Held) -> PlatformResult<()> {
        check_running(process)
    }

    fn detach(mut self) -> PlatformResult<()> {
        if unsafe { FreeConsole() } == 0 {
            return Err(last_error("FreeConsole"));
        }
        self.attached = false;
        Ok(())
    }
}

impl<'a> ConsoleBackend for Backend<'a> {
    type Attachment = OwnedConsole<'a>;

    fn existing_console_window(&mut self) -> PlatformResult<Option<u64>> {
        let window = unsafe { GetConsoleWindow() };
        Ok((!window.is_null()).then_some(window as usize as u64))
    }

    fn attach(&mut self, target_pid: u32) -> PlatformResult<Self::Attachment> {
        if unsafe { AttachConsole(target_pid) } == 0 {
            return Err(last_error("AttachConsole"));
        }
        Ok(OwnedConsole {
            epoch: self.epoch,
            names: self.names,
            attached: true,
        })
    }
}

pub fn observe_console_membership(
    request: &ProcessRequest,
    names: ExecutableNames<'_>,
) -> Observation<ConsoleMembershipReport> {
    collect_console_membership(
        &mut Backend {
            epoch: request.requested_at.epoch,
            names,
        },
        request,
    )
    .into()
}
