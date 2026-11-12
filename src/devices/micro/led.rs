use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::error::MicroError;
use super::input::{PROFILE_ID, Slot};
use super::json::repr_float;

pub const DEFAULT_BRIGHTNESS: f64 = 0.25;
pub const LEGACY_EFFECT: u32 = 1;
pub const LEGACY_SPEED: f64 = 0.2;
pub const MAX_OBSERVATION_REFERENCE_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedStatus {
    Off,
    Idle,
    Working,
    Complete,
    Waiting,
    Error,
    Unknown,
}

impl LedStatus {
    pub const fn color(self) -> u32 {
        match self {
            Self::Off => 0x000000,
            Self::Idle => 0xffffff,
            Self::Working => 0x2070ff,
            Self::Complete => 0x00d060,
            Self::Waiting => 0xffaa00,
            Self::Error => 0xff3030,
            Self::Unknown => 0x607d8b,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "off" => Some(Self::Off),
            "idle" => Some(Self::Idle),
            "working" => Some(Self::Working),
            "complete" => Some(Self::Complete),
            "waiting" => Some(Self::Waiting),
            "error" => Some(Self::Error),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LedEntry {
    pub id: u8,
    pub c: u32,
    pub b: f64,
    pub e: u32,
    pub s: f64,
}

/// This legacy constructor intentionally does not validate brightness.
pub fn legacy_frame(
    statuses: [LedStatus; 6],
    selected: Option<Slot>,
    brightness: f64,
) -> [LedEntry; 6] {
    std::array::from_fn(|index| LedEntry {
        id: index as u8,
        c: statuses[index].color(),
        b: if selected.is_some_and(|slot| usize::from(slot.get()) != index + 1) {
            brightness / 2.0
        } else {
            brightness
        },
        e: LEGACY_EFFECT,
        s: LEGACY_SPEED,
    })
}

pub fn shutdown_frame() -> [LedEntry; 6] {
    legacy_frame([LedStatus::Off; 6], None, DEFAULT_BRIGHTNESS)
}

pub fn diagnostic_frame() -> [LedEntry; 6] {
    legacy_frame(
        [
            LedStatus::Idle,
            LedStatus::Working,
            LedStatus::Complete,
            LedStatus::Waiting,
            LedStatus::Error,
            LedStatus::Off,
        ],
        Some(Slot::ALL[0]),
        DEFAULT_BRIGHTNESS,
    )
}

pub fn active_slots(statuses: &[LedStatus; 6]) -> Vec<Slot> {
    statuses
        .iter()
        .zip(Slot::ALL)
        .filter_map(|(status, slot)| (*status != LedStatus::Off).then_some(slot))
        .collect()
}

pub(crate) fn serialize_entries(entries: &[LedEntry; 6]) -> Result<String, MicroError> {
    let mut json = String::from("[");
    for (index, entry) in entries.iter().enumerate() {
        if !entry.b.is_finite() || !entry.s.is_finite() {
            return Err(MicroError::Serialization);
        }
        if index != 0 {
            json.push(',');
        }
        write!(
            json,
            "{{\"id\":{},\"c\":{},\"b\":{},\"e\":{},\"s\":{}}}",
            entry.id,
            entry.c,
            repr_float(entry.b),
            entry.e,
            repr_float(entry.s)
        )
        .expect("writing to a string cannot fail");
    }
    json.push(']');
    Ok(json)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Micro,
    Akp03,
    Ct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Usb,
    Bluetooth,
    Serial,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityScope {
    pub device: DeviceKind,
    pub profile: String,
    pub firmware: String,
    pub transport: Transport,
    pub mode: String,
    pub platform: String,
    pub architecture: String,
}

impl CapabilityScope {
    pub fn is_complete(&self) -> bool {
        self.transport != Transport::Unknown
            && [
                &self.profile,
                &self.firmware,
                &self.mode,
                &self.platform,
                &self.architecture,
            ]
            .into_iter()
            .all(|text| {
                !text.trim().is_empty() && text.len() <= 128 && !text.chars().any(char::is_control)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    NotImplemented,
    Inferred,
    SourceReported,
    VendorClaim,
    OfflineDecoded,
    MockTested,
    NativeObserved,
    PhysicallyAccepted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectFeature {
    Steady,
    SlowPulse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticToken {
    Off,
    Idle,
    Working,
    Complete,
    Waiting,
    Error,
    Unknown,
    Empty,
    Stale,
    Exited,
    PidReused,
    Unresolved,
    Busy,
    Refused,
    Neutral,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleCapability {
    pub scope: CapabilityScope,
    pub feature: EffectFeature,
    pub evidence: Evidence,
    pub observation_reference: Option<String>,
}

impl VisibleCapability {
    fn authorizes(&self, scope: &CapabilityScope, feature: EffectFeature) -> bool {
        self.scope == *scope
            && scope.is_complete()
            && self.feature == feature
            && self.evidence == Evidence::PhysicallyAccepted
            && self
                .observation_reference
                .as_ref()
                .is_some_and(|reference| {
                    !reference.is_empty() && reference.len() <= MAX_OBSERVATION_REFERENCE_BYTES
                })
    }
}

/// This is caller-supplied evidence, not an observation made by this module.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectQualification {
    pub capability: VisibleCapability,
    pub token: SemanticToken,
    pub native_id: u32,
    pub native_speed: f64,
    pub smooth_cycle_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EffectRealization {
    Native { id: u32, speed: f64 },
    NonNative,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticLed {
    pub scope: CapabilityScope,
    pub id: u8,
    pub token: SemanticToken,
    pub rgb: [u8; 3],
    pub brightness: f64,
    pub realization: EffectRealization,
}

/// Returns no entry unless externally supplied physical evidence matches exactly.
pub fn encode_semantic(
    realized: &SemanticLed,
    qualification: Option<&EffectQualification>,
) -> Option<LedEntry> {
    if realized.scope.device != DeviceKind::Micro
        || realized.scope.profile != PROFILE_ID
        || realized.id >= 6
        || !realized.brightness.is_finite()
        || !(0.0..=1.0).contains(&realized.brightness)
    {
        return None;
    }
    let qualification = qualification?;
    if qualification.token != realized.token {
        return None;
    }
    let (effect, speed) = match realized.realization {
        EffectRealization::Native { id, speed } => {
            if !qualification
                .capability
                .authorizes(&realized.scope, EffectFeature::SlowPulse)
                || !speed.is_finite()
                || speed <= 0.0
                || qualification.native_id != id
                || qualification.native_speed != speed
                || qualification
                    .smooth_cycle_ms
                    .is_none_or(|period| period < 1000)
            {
                return None;
            }
            (id, speed)
        }
        EffectRealization::NonNative => {
            if !qualification
                .capability
                .authorizes(&realized.scope, EffectFeature::Steady)
                || !qualification.native_speed.is_finite()
                || qualification.native_speed < 0.0
            {
                return None;
            }
            (qualification.native_id, qualification.native_speed)
        }
    };
    let [red, green, blue] = realized.rgb;
    Some(LedEntry {
        id: realized.id,
        c: (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue),
        b: realized.brightness,
        e: effect,
        s: speed,
    })
}
