use super::super::{
    ErrorKind, Observation, PlatformError, PlatformResult, SerialBackend, SerialSelection,
    collect_serial, configuration_manager_error, decode_registry_multi_sz, decode_registry_sz,
};
use super::{last_error, native_error};
use std::{mem, ptr};
use windows_sys::{
    Win32::{
        Devices::DeviceAndDriverInstallation::{
            CM_DRP_HARDWAREID, CM_Get_DevNode_Registry_PropertyW, CM_Get_Parent,
            CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CR_NO_SUCH_VALUE, CR_SUCCESS,
            DICS_FLAG_GLOBAL, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, DIREG_DEV, HDEVINFO,
            SP_DEVICE_INTERFACE_DATA, SP_DEVINFO_DATA, SPINT_ACTIVE, SPINT_REMOVED,
            SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
            SetupDiGetDeviceInterfaceDetailW, SetupDiOpenDevRegKey,
        },
        Foundation::{
            ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS,
            ERROR_SUCCESS, GetLastError, INVALID_HANDLE_VALUE,
        },
        System::Registry::{HKEY, KEY_QUERY_VALUE, RegCloseKey, RegQueryValueExW},
    },
    core::GUID,
};

const COM_INTERFACE: GUID = GUID {
    data1: 0x86e0d1e0,
    data2: 0x8089,
    data3: 0x11d0,
    data4: [0x9c, 0xe4, 0x08, 0x00, 0x3e, 0x30, 0x1f, 0x73],
};

struct DeviceSet(HDEVINFO);
impl Drop for DeviceSet {
    fn drop(&mut self) {
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

struct RegistryKey(HKEY);
impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

fn config_error(operation: &'static str, code: u32) -> PlatformError {
    configuration_manager_error(operation, code)
}

struct Discovery {
    set: DeviceSet,
    index: u32,
    root: u32,
}

impl SerialBackend for Discovery {
    type Interface = SP_DEVICE_INTERFACE_DATA;
    type Associated = SP_DEVINFO_DATA;
    type Node = u32;

    fn next_interface(&mut self) -> PlatformResult<Option<Self::Interface>> {
        let mut interface: SP_DEVICE_INTERFACE_DATA = unsafe { mem::zeroed() };
        interface.cbSize = mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        if unsafe {
            SetupDiEnumDeviceInterfaces(
                self.set.0,
                ptr::null(),
                &COM_INTERFACE,
                self.index,
                &mut interface,
            )
        } == 0
        {
            let code = unsafe { GetLastError() };
            if code == ERROR_NO_MORE_ITEMS {
                return Ok(None);
            }
            return Err(native_error("SetupDiEnumDeviceInterfaces", code));
        }
        self.index += 1;
        Ok(Some(interface))
    }

    fn flags(&self, interface: &Self::Interface) -> (bool, bool) {
        (
            interface.Flags & SPINT_REMOVED == 0,
            interface.Flags & SPINT_ACTIVE != 0,
        )
    }

    fn associated_device(
        &mut self,
        interface: &Self::Interface,
    ) -> PlatformResult<Self::Associated> {
        let mut device: SP_DEVINFO_DATA = unsafe { mem::zeroed() };
        device.cbSize = mem::size_of::<SP_DEVINFO_DATA>() as u32;
        let mut required = 0;
        let result = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                self.set.0,
                interface,
                ptr::null_mut(),
                0,
                &mut required,
                &mut device,
            )
        };
        if result != 0 {
            return Err(PlatformError::new(
                ErrorKind::InvalidData,
                "serial_interface_detail",
            ));
        }
        let code = unsafe { GetLastError() };
        if code != ERROR_INSUFFICIENT_BUFFER {
            return Err(native_error("SetupDiGetDeviceInterfaceDetailW", code));
        }
        if required == 0 {
            return Err(PlatformError::new(
                ErrorKind::InvalidData,
                "serial_interface_detail",
            ));
        }
        Ok(device)
    }

    fn node(&self, device: &Self::Associated) -> Self::Node {
        device.DevInst
    }

    fn parent(&mut self, node: Self::Node) -> PlatformResult<Option<Self::Node>> {
        if node == self.root {
            return Ok(None);
        }
        let mut parent = 0;
        match unsafe { CM_Get_Parent(&mut parent, node, 0) } {
            CR_SUCCESS => Ok(Some(parent)),
            code => Err(config_error("CM_Get_Parent", code)),
        }
    }

    fn hardware_ids(&mut self, node: Self::Node) -> PlatformResult<Vec<String>> {
        let mut units = vec![0u16; 32_768];
        let mut length = (units.len() * 2) as u32;
        let mut value_type = 0;
        match unsafe {
            CM_Get_DevNode_Registry_PropertyW(
                node,
                CM_DRP_HARDWAREID,
                &mut value_type,
                units.as_mut_ptr().cast(),
                &mut length,
                0,
            )
        } {
            CR_NO_SUCH_VALUE => Ok(Vec::new()),
            CR_SUCCESS => {
                if length as usize > units.len() * 2 {
                    return Err(PlatformError::new(
                        ErrorKind::LimitExceeded,
                        "registry_hardware_ids",
                    ));
                }
                let bytes =
                    unsafe { std::slice::from_raw_parts(units.as_ptr().cast(), length as usize) };
                decode_registry_multi_sz(value_type, bytes)
            }
            code => Err(config_error("CM_Get_DevNode_Registry_PropertyW", code)),
        }
    }

    fn port_name(&mut self, device: &Self::Associated) -> PlatformResult<Option<String>> {
        let raw = unsafe {
            SetupDiOpenDevRegKey(
                self.set.0,
                device,
                DICS_FLAG_GLOBAL,
                0,
                DIREG_DEV,
                KEY_QUERY_VALUE,
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(last_error("SetupDiOpenDevRegKey"));
        }
        let key = RegistryKey(raw);
        let mut units = [0u16; 64];
        let mut length = (units.len() * 2) as u32;
        let mut value_type = 0;
        const PORT_NAME: [u16; 9] = [80, 111, 114, 116, 78, 97, 109, 101, 0];
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PORT_NAME.as_ptr(),
                ptr::null(),
                &mut value_type,
                units.as_mut_ptr().cast(),
                &mut length,
            )
        };
        match status {
            ERROR_FILE_NOT_FOUND => Ok(None),
            ERROR_MORE_DATA => Err(PlatformError::with_code(
                ErrorKind::LimitExceeded,
                "registry_port_name",
                status,
            )),
            ERROR_SUCCESS => {
                if length as usize > units.len() * 2 {
                    return Err(PlatformError::new(
                        ErrorKind::LimitExceeded,
                        "registry_port_name",
                    ));
                }
                let bytes =
                    unsafe { std::slice::from_raw_parts(units.as_ptr().cast(), length as usize) };
                decode_registry_sz(value_type, bytes).map(Some)
            }
            code => Err(native_error("RegQueryValueExW", code)),
        }
    }
}

pub fn observe_serial() -> Observation<SerialSelection> {
    (|| {
        let mut root = 0;
        let status =
            unsafe { CM_Locate_DevNodeW(&mut root, ptr::null(), CM_LOCATE_DEVNODE_NORMAL) };
        if status != CR_SUCCESS {
            return Err(config_error("CM_Locate_DevNodeW", status));
        }
        let raw = unsafe {
            SetupDiGetClassDevsW(
                &COM_INTERFACE,
                ptr::null(),
                ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
        };
        if raw == INVALID_HANDLE_VALUE as isize {
            return Err(last_error("SetupDiGetClassDevsW"));
        }
        collect_serial(&mut Discovery {
            set: DeviceSet(raw),
            index: 0,
            root,
        })
    })()
    .into()
}
