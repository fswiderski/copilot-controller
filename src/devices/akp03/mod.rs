//! Pure AKP03 input, profile, image, and HID-output preparation.
//!
//! These APIs never discover or open a device, authorize an action, or assert
//! that a report was displayed. Shared presentation and gesture admission are
//! deliberately left to their respective integration layers.

pub mod controls;
pub mod image;
pub mod input;
pub mod layout;
pub mod output;
pub mod probe;
pub mod profile;

pub use profile::Profile;

#[cfg(test)]
mod tests;
