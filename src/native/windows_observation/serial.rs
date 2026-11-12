use super::{ErrorKind, PlatformError, PlatformResult};
use serde::Serialize;

const REG_SZ: u32 = 1;
const REG_MULTI_SZ: u32 = 7;

pub fn matches_serial_hardware_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    const BASE: &[u8] = b"USB\\VID_2EC2&PID_0003";
    if bytes.len() < BASE.len() || !bytes[..BASE.len()].eq_ignore_ascii_case(BASE) {
        return false;
    }
    let mut rest = &bytes[BASE.len()..];
    let mut rev = false;
    let mut mi = false;
    while !rest.is_empty() {
        if rest.len() >= 9 && rest[..5].eq_ignore_ascii_case(b"&REV_") {
            if rev || !rest[5..9].iter().all(u8::is_ascii_hexdigit) {
                return false;
            }
            rev = true;
            rest = &rest[9..];
        } else if rest.len() >= 6 && rest[..4].eq_ignore_ascii_case(b"&MI_") {
            if mi || !rest[4..6].iter().all(u8::is_ascii_hexdigit) {
                return false;
            }
            mi = true;
            rest = &rest[6..];
        } else {
            return false;
        }
    }
    true
}

pub fn normalize_com_port(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    if !(4..=8).contains(&bytes.len())
        || !bytes[..3].eq_ignore_ascii_case(b"COM")
        || bytes[3] == b'0'
        || !bytes[3..].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let mut number = 0u32;
    for digit in &bytes[3..] {
        number = number * 10 + u32::from(*digit - b'0');
    }
    if number == 0 || number > 65_535 {
        return None;
    }
    Some(format!("COM{number}"))
}

/// Raw facts are deliberately not serializable: ports may be invalid or sensitive.
pub struct SerialFact {
    pub present: bool,
    pub active: bool,
    pub hardware_associated: bool,
    pub port: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialSelectionState {
    Incomplete,
    NotFound,
    Unique,
    Ambiguous,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SerialSelection {
    pub state: SerialSelectionState,
    pub present_count: usize,
    pub matching_count: usize,
    pub ports: Vec<String>,
    pub selected_port: Option<String>,
}

pub fn select_serial(facts: &[SerialFact], complete: bool) -> SerialSelection {
    let present_count = facts.iter().filter(|fact| fact.present).count();
    let matching: Vec<_> = facts
        .iter()
        .filter(|fact| fact.present && fact.active && fact.hardware_associated)
        .collect();
    let ports: Vec<_> = matching
        .iter()
        .filter_map(|fact| fact.port.as_deref().and_then(normalize_com_port))
        .collect();
    let state = if !complete || ports.len() != matching.len() {
        SerialSelectionState::Incomplete
    } else {
        match matching.len() {
            0 => SerialSelectionState::NotFound,
            1 => SerialSelectionState::Unique,
            _ => SerialSelectionState::Ambiguous,
        }
    };
    let selected_port = if state == SerialSelectionState::Unique {
        ports.first().cloned()
    } else {
        None
    };
    SerialSelection {
        state,
        present_count,
        matching_count: matching.len(),
        ports,
        selected_port,
    }
}

fn registry_units(
    bytes: &[u8],
    max_units: usize,
    operation: &'static str,
) -> PlatformResult<Vec<u16>> {
    if bytes.len() > max_units * 2 {
        return Err(PlatformError::new(ErrorKind::LimitExceeded, operation));
    }
    if !bytes.len().is_multiple_of(2) {
        return Err(PlatformError::new(ErrorKind::InvalidData, operation));
    }
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect())
}

pub fn decode_registry_sz(value_type: u32, bytes: &[u8]) -> PlatformResult<String> {
    const OP: &str = "registry_port_name";
    if value_type != REG_SZ {
        return Err(PlatformError::new(ErrorKind::InvalidData, OP));
    }
    let units = registry_units(bytes, 64, OP)?;
    let Some(end) = units.iter().position(|unit| *unit == 0) else {
        return Err(PlatformError::new(ErrorKind::InvalidData, OP));
    };
    if units[end..].iter().any(|unit| *unit != 0) {
        return Err(PlatformError::new(ErrorKind::InvalidData, OP));
    }
    String::from_utf16(&units[..end]).map_err(|_| PlatformError::new(ErrorKind::InvalidData, OP))
}

pub fn decode_registry_multi_sz(value_type: u32, bytes: &[u8]) -> PlatformResult<Vec<String>> {
    const OP: &str = "registry_hardware_ids";
    if value_type != REG_MULTI_SZ {
        return Err(PlatformError::new(ErrorKind::InvalidData, OP));
    }
    let units = registry_units(bytes, 32_768, OP)?;
    if units.len() < 2 || units[units.len() - 2..] != [0, 0] {
        return Err(PlatformError::new(ErrorKind::InvalidData, OP));
    }
    let mut result = Vec::new();
    let mut start = 0;
    loop {
        if units[start] == 0 {
            if units[start..].iter().any(|unit| *unit != 0) {
                return Err(PlatformError::new(ErrorKind::InvalidData, OP));
            }
            return Ok(result);
        }
        let Some(relative_end) = units[start..].iter().position(|unit| *unit == 0) else {
            return Err(PlatformError::new(ErrorKind::InvalidData, OP));
        };
        let end = start + relative_end;
        result.push(
            String::from_utf16(&units[start..end])
                .map_err(|_| PlatformError::new(ErrorKind::InvalidData, OP))?,
        );
        start = end + 1;
    }
}

/// Native implementations must not retrieve interface-path text or open ports.
pub trait SerialBackend {
    type Interface;
    type Associated;
    type Node: Copy + Eq;

    fn next_interface(&mut self) -> PlatformResult<Option<Self::Interface>>;
    fn flags(&self, interface: &Self::Interface) -> (bool, bool);
    fn associated_device(
        &mut self,
        interface: &Self::Interface,
    ) -> PlatformResult<Self::Associated>;
    fn node(&self, device: &Self::Associated) -> Self::Node;
    /// `None` means that `node` is the root, whose properties must not be read.
    fn parent(&mut self, node: Self::Node) -> PlatformResult<Option<Self::Node>>;
    fn hardware_ids(&mut self, node: Self::Node) -> PlatformResult<Vec<String>>;
    fn port_name(&mut self, device: &Self::Associated) -> PlatformResult<Option<String>>;
}

pub fn collect_serial<B: SerialBackend>(backend: &mut B) -> PlatformResult<SerialSelection> {
    let mut facts = Vec::new();
    while let Some(interface) = backend.next_interface()? {
        if facts.len() == 512 {
            return Err(PlatformError::new(
                ErrorKind::LimitExceeded,
                "serial_interface_enumeration",
            ));
        }
        let (present, active) = backend.flags(&interface);
        let mut fact = SerialFact {
            present,
            active,
            hardware_associated: false,
            port: None,
        };
        if present && active {
            let device = backend.associated_device(&interface)?;
            let mut node = backend.node(&device);
            let mut visited = Vec::new();
            loop {
                if visited.contains(&node) {
                    return Err(PlatformError::new(
                        ErrorKind::InvalidData,
                        "serial_ancestor_cycle",
                    ));
                }
                visited.push(node);
                let Some(parent) = backend.parent(node)? else {
                    break;
                };
                if visited.len() > 16 {
                    return Err(PlatformError::new(
                        ErrorKind::LimitExceeded,
                        "serial_ancestor_depth",
                    ));
                }
                if backend
                    .hardware_ids(node)?
                    .iter()
                    .any(|id| matches_serial_hardware_id(id))
                {
                    fact.hardware_associated = true;
                }
                node = parent;
            }
            if fact.hardware_associated {
                fact.port = backend.port_name(&device)?;
            }
        }
        facts.push(fact);
    }
    Ok(select_serial(&facts, true))
}
