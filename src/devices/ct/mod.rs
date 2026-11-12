//! Portable, offline CT protocol and display preparation.
//!
//! These helpers never open a device, deliver an action, or establish physical
//! qualification. A framing fault requires the caller to invalidate its input
//! generation. Rasterization uses caller-supplied text and icon coverage.
//!
//! Integration requires `unicode-general-category = "=1.1.0"` (Unicode 16.0).
//! Map these CT-specific slot, status, selection, and event types into the
//! package's shared contracts; recommendations and historical mapping evidence
//! are not grants. Touch contacts are only named, never normalized into input.
//! The caller supplies text/action-icon rasterization. Native text/PNG, artwork,
//! serial I/O, touch recognition, delivery completion, and scheduling are not
//! implemented here. Prepared buffers and correlated replies do not prove
//! physical visibility, and no display frame-rate limit is invented.

pub mod controls;
pub mod layout;
pub mod pixels;
pub mod probe;
pub mod protocol;
pub mod scene;
pub mod text;

pub const USB_VENDOR_ID: u16 = 0x2ec2;
pub const USB_PRODUCT_ID: u16 = 0x0003;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub protocol_decoding: bool,
    pub pixel_conversion: bool,
    pub offline_composition: bool,
    pub live_transport: bool,
    pub hardware_qualified: bool,
    pub firmware_update: bool,
    pub command_ready: bool,
}

pub const CAPABILITIES: Capabilities = Capabilities {
    protocol_decoding: true,
    pixel_conversion: true,
    offline_composition: true,
    live_transport: false,
    hardware_qualified: false,
    firmware_update: false,
    command_ready: false,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveSupportError {
    UnsupportedIdentity,
    HardwareUnqualified,
}

/// No identity is admitted as production live support by this offline module.
pub fn require_live_support(vendor: u16, product: u16) -> Result<(), LiveSupportError> {
    if vendor != USB_VENDOR_ID || product != USB_PRODUCT_ID {
        Err(LiveSupportError::UnsupportedIdentity)
    } else {
        Err(LiveSupportError::HardwareUnqualified)
    }
}

macro_rules! impl_error {
    ($($error:ty),+ $(,)?) => {
        $(
            impl std::fmt::Display for $error {
                fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    write!(formatter, "{self:?}")
                }
            }

            impl std::error::Error for $error {}
        )+
    };
}

impl_error!(
    LiveSupportError,
    protocol::FrameError,
    protocol::InputError,
    pixels::PixelError,
    layout::LayoutError,
    scene::SceneError,
);

impl<E: std::fmt::Debug> std::fmt::Display for scene::RenderError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl<E: std::error::Error + 'static> std::error::Error for scene::RenderError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidScene(error) => Some(error),
            Self::Backend(error) => Some(error),
            Self::Pixels(error) => Some(error),
            Self::Coverage { .. } | Self::Allocation => None,
        }
    }
}

/// A validated, one-based CT session slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Slot(u8);

impl Slot {
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number <= 6 {
            Some(Self(number))
        } else {
            None
        }
    }

    pub const fn number(self) -> u8 {
        self.0
    }

    pub const fn index(self) -> usize {
        (self.0 - 1) as usize
    }
}

#[cfg(test)]
mod tests;
