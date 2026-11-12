//! Pure, fail-closed platform gating.

use super::UnavailableReason;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HidFeature {
    WindowsHid,
    MacosIoKit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformSupport {
    Supported(HidFeature),
    Unavailable(UnavailableReason),
}

/// Windows x64/ARM64 and macOS ARM64 only, and only with a HID feature selected.
pub fn platform_support(feature_enabled: bool, os: &str, arch: &str) -> PlatformSupport {
    if !feature_enabled {
        return PlatformSupport::Unavailable(UnavailableReason::FeatureDisabled);
    }
    match (os, arch) {
        ("windows", "x86_64" | "aarch64") => PlatformSupport::Supported(HidFeature::WindowsHid),
        ("macos", "aarch64") => PlatformSupport::Supported(HidFeature::MacosIoKit),
        _ => PlatformSupport::Unavailable(UnavailableReason::UnsupportedPlatform),
    }
}

/// Extracts `vid_XXXX` / `pid_XXXX` from a Windows interface path so unrelated
/// devices are never opened during metadata enumeration.
#[cfg(any(all(windows, any(feature = "akp03-hid", feature = "micro-hid")), test))]
pub(super) fn parse_vid_pid(path: &str) -> Option<super::HidIdentity> {
    let lower = path.to_ascii_lowercase();
    let field = |tag: &str| -> Option<u16> {
        let start = lower.find(tag)? + tag.len();
        let hex = lower.get(start..start + 4)?;
        let next = lower.as_bytes().get(start + 4);
        if next.is_some_and(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u16::from_str_radix(hex, 16).ok()
    };
    Some(super::HidIdentity {
        vendor_id: field("vid_")?,
        product_id: field("pid_")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_windows_paths() {
        let p =
            r"\\?\hid#vid_1A2B&pid_00ff&mi_00#7&abc&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        let id = parse_vid_pid(p).unwrap();
        assert_eq!((id.vendor_id, id.product_id), (0x1a2b, 0xff));
        assert!(parse_vid_pid(r"\\?\hid#vid_12&pid_0001").is_none());
        assert!(parse_vid_pid(r"\\?\hid#vid_12345&pid_0001").is_none());
        assert!(parse_vid_pid(r"\\?\hid#vid_zzzz&pid_0001").is_none());
        assert!(parse_vid_pid("").is_none());
    }
}
