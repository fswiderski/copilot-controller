use super::super::*;
use super::{native_error, qualification::Backend};
use ::windows::{
    Win32::{
        Foundation::HWND,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        UI::Accessibility::{
            CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTreeWalker,
        },
    },
    core::Interface,
};
use std::ffi::c_void;
use windows_sys::Win32::{
    System::Threading::GetCurrentProcessId,
    UI::WindowsAndMessaging::{
        GA_ROOT, GetAncestor, GetForegroundWindow, GetWindowThreadProcessId, IsWindow,
    },
};

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct Ui<'a> {
    process: Backend<'a>,
    // COM references must drop before the apartment guard.
    automation: Option<IUIAutomation>,
    walker: Option<IUIAutomationTreeWalker>,
    apartment: Option<Apartment>,
}

fn com_error(operation: &'static str, error: ::windows::core::Error) -> PlatformError {
    native_error(operation, error.code().0 as u32)
}

impl ProcessProbeBackend for Ui<'_> {
    type Held = <Backend<'static> as ProcessProbeBackend>::Held;

    fn stamp(&mut self) -> PlatformResult<MonotonicStamp> {
        self.process.stamp()
    }
    fn current_pid(&mut self) -> u32 {
        self.process.current_pid()
    }
    fn hold(&mut self, pid: u32) -> PlatformResult<Self::Held> {
        self.process.hold(pid)
    }
    fn evidence(&mut self, process: &Self::Held) -> PlatformResult<ProcessEvidence> {
        self.process.evidence(process)
    }
    fn check_running(&mut self, process: &Self::Held) -> PlatformResult<()> {
        self.process.check_running(process)
    }
    fn security(
        &mut self,
        observer: &Self::Held,
        target: &Self::Held,
    ) -> PlatformResult<SecurityEvidence> {
        self.process.security(observer, target)
    }
}

impl Ui<'_> {
    fn initialize(&mut self) -> PlatformResult<()> {
        if self.apartment.is_some() {
            return Ok(());
        }
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.is_err() {
            return Err(native_error("CoInitializeEx", result.0 as u32));
        }
        self.apartment = Some(Apartment);
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                .map_err(|error| com_error("CoCreateInstance_UIAutomation", error))?;
        let walker = unsafe { automation.ControlViewWalker() }
            .map_err(|error| com_error("UIAutomation_ControlViewWalker", error))?;
        self.automation = Some(automation);
        self.walker = Some(walker);
        Ok(())
    }

    fn traverse(
        &self,
        node: &IUIAutomationElement,
        child: bool,
    ) -> PlatformResult<Option<IUIAutomationElement>> {
        let walker = self
            .walker
            .as_ref()
            .ok_or_else(|| PlatformError::new(ErrorKind::InvalidData, "uia_walker"))?;
        // Successful null pointers mean traversal end; the typed wrapper rejects them.
        let mut result: *mut c_void = std::ptr::null_mut();
        let status = unsafe {
            if child {
                (walker.vtable().GetFirstChildElement)(walker.as_raw(), node.as_raw(), &mut result)
            } else {
                (walker.vtable().GetNextSiblingElement)(walker.as_raw(), node.as_raw(), &mut result)
            }
        };
        if status.is_err() {
            return Err(native_error("UIAutomation_tree_walk", status.0 as u32));
        }
        if result.is_null() {
            return Ok(None);
        }
        Ok(Some(unsafe { IUIAutomationElement::from_raw(result) }))
    }
}

impl UiBackend for Ui<'_> {
    type Element = IUIAutomationElement;

    fn is_root_window(&mut self, window: u64) -> PlatformResult<bool> {
        let hwnd = window as usize as *mut c_void;
        Ok(unsafe { IsWindow(hwnd) != 0 && GetAncestor(hwnd, GA_ROOT) == hwnd })
    }

    fn window_owner(&mut self, window: u64) -> PlatformResult<(u32, u32)> {
        let mut pid = 0;
        let thread = unsafe { GetWindowThreadProcessId(window as usize as *mut c_void, &mut pid) };
        Ok((thread, pid))
    }

    fn is_foreground(&mut self, window: u64) -> PlatformResult<bool> {
        Ok(unsafe { GetForegroundWindow() == window as usize as *mut c_void })
    }

    fn root_element(&mut self, window: u64) -> PlatformResult<Self::Element> {
        self.initialize()?;
        let automation = self
            .automation
            .as_ref()
            .ok_or_else(|| PlatformError::new(ErrorKind::InvalidData, "uia_automation"))?;
        unsafe { automation.ElementFromHandle(HWND(window as usize as *mut c_void)) }
            .map_err(|error| com_error("UIAutomation_ElementFromHandle", error))
    }

    fn root_native_window(&mut self, root: &Self::Element) -> PlatformResult<u64> {
        let window = unsafe { root.CurrentNativeWindowHandle() }
            .map_err(|error| com_error("UIAutomation_NativeWindowHandle", error))?;
        Ok(window.0 as usize as u64)
    }

    fn process_id(&mut self, node: &Self::Element) -> PlatformResult<i32> {
        unsafe { node.CurrentProcessId() }
            .map_err(|error| com_error("UIAutomation_ProcessId", error))
    }

    fn metadata(&mut self, node: &Self::Element) -> PlatformResult<UiNodeMetadata> {
        let control_type = unsafe { node.CurrentControlType() }
            .map_err(|error| com_error("UIAutomation_ControlType", error))?
            .0;
        let keyboard_focus = unsafe { node.CurrentHasKeyboardFocus() }
            .map_err(|error| com_error("UIAutomation_KeyboardFocus", error))?
            .0
            != 0;
        let offscreen = unsafe { node.CurrentIsOffscreen() }
            .map_err(|error| com_error("UIAutomation_Offscreen", error))?
            .0
            != 0;
        Ok(UiNodeMetadata {
            control_type,
            keyboard_focus,
            offscreen,
        })
    }

    fn first_child(&mut self, node: &Self::Element) -> PlatformResult<Option<Self::Element>> {
        self.traverse(node, true)
    }

    fn next_sibling(&mut self, node: &Self::Element) -> PlatformResult<Option<Self::Element>> {
        self.traverse(node, false)
    }
}

pub(super) fn observe(
    request: &UiRequest,
    names: ExecutableNames<'_>,
) -> Observation<UiStructureReport> {
    if request.process.target.pid == unsafe { GetCurrentProcessId() } {
        return Observation::Unavailable(PlatformError::new(
            ErrorKind::InvalidData,
            "helper_must_not_be_target",
        ));
    }
    let request = request.clone();
    let cli = names.cli_basename().to_owned();
    let native_host = names.native_host_basename().to_owned();
    let thread = std::thread::Builder::new()
        .name("windows-uia-observation".into())
        .spawn(move || {
            let names = ExecutableNames::new(&cli, &native_host)?;
            let mut backend = Ui {
                process: Backend {
                    epoch: request.process.requested_at.epoch,
                    names,
                },
                automation: None,
                walker: None,
                apartment: None,
            };
            collect_ui_structure(&mut backend, &request)
        });
    match thread {
        Ok(thread) => match thread.join() {
            Ok(result) => result.into(),
            Err(_) => Observation::Unavailable(PlatformError::new(
                ErrorKind::NativeFailure,
                "uia_thread_panic",
            )),
        },
        Err(_) => Observation::Unavailable(PlatformError::new(
            ErrorKind::NativeFailure,
            "uia_thread_create",
        )),
    }
}
