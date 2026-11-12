use serde::{Deserialize, Serialize};
use std::fmt;

/// A wire profile, not evidence that a particular device is qualified.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Profile {
    #[default]
    #[serde(rename = "akp03-original")]
    Original,
    #[serde(rename = "akp03e-rev2")]
    Rev2,
}

/// Only the identity fields relevant to the offline profile match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HidIdentity {
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
}

impl Profile {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Original => "akp03-original",
            Self::Rev2 => "akp03e-rev2",
        }
    }

    pub const fn identity(self) -> HidIdentity {
        HidIdentity {
            vendor_id: 0x0300,
            product_id: match self {
                Self::Original => 0x1001,
                Self::Rev2 => 0x3002,
            },
            usage_page: 0xffa0,
            usage: 1,
        }
    }

    pub const fn key_size(self) -> u16 {
        match self {
            Self::Original => 60,
            Self::Rev2 => 64,
        }
    }

    pub const fn minimum_input_report_len(self) -> usize {
        match self {
            Self::Original => 10,
            Self::Rev2 => 11,
        }
    }
}

/// Match all four fields; this does not enumerate, select, or open a device.
pub fn match_identity(identity: HidIdentity) -> Option<Profile> {
    [Profile::Original, Profile::Rev2]
        .into_iter()
        .find(|profile| profile.identity() == identity)
}

/// The exact revision-2 descriptor accepted by a future bounded probe.
pub const REV2_REPORT_DESCRIPTOR: [u8; 54] = [
    0x06, 0xa0, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x09, 0x02, 0xa1, 0x00, 0x06, 0xa1, 0xff, 0x09, 0x03,
    0x09, 0x04, 0x15, 0x80, 0x25, 0x7f, 0x35, 0x00, 0x45, 0xff, 0x75, 0x08, 0x96, 0x00, 0x02, 0x81,
    0x02, 0x09, 0x05, 0x09, 0x06, 0x15, 0x80, 0x25, 0x7f, 0x35, 0x00, 0x45, 0xff, 0x75, 0x08, 0x96,
    0x00, 0x04, 0x91, 0x02, 0xc0, 0xc0,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DescriptorMismatch;

impl fmt::Display for DescriptorMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("revision-2 HID report descriptor does not match exactly")
    }
}

impl std::error::Error for DescriptorMismatch {}

/// Compare bytes, not just equivalent HID semantics.
pub fn validate_rev2_report_descriptor(descriptor: &[u8]) -> Result<(), DescriptorMismatch> {
    if descriptor == REV2_REPORT_DESCRIPTOR {
        Ok(())
    } else {
        Err(DescriptorMismatch)
    }
}
