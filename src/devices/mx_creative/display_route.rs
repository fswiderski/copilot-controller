use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::probe::{
    KEYPAD_DESCRIPTOR_HEX, KEYPAD_DESCRIPTOR_SHA256, KEYPAD_LOCATION_ID, KEYPAD_PID, KEYPAD_USAGE,
    VENDOR_PAGE, VID,
};
use crate::native::hid::{
    HidBus, HidDescriptor, HidIdentity, HidMatcher, MxCreativeDisplayRouteBackend,
};

pub const OUTPUT_REPORT_ID: u8 = 0x14;
pub const OUTPUT_REPORT_LEN: usize = 4095;
const DISPLAY_USAGE: u16 = 0x1a10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsagePair {
    pub usage_page: u16,
    pub usage: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub vendor_id: u16,
    pub product_id: u16,
    pub transport: String,
    pub primary_usage_page: u16,
    pub primary_usage: u16,
    pub vendor_collection_usages: Vec<UsagePair>,
    pub max_input_report_len: usize,
    pub max_output_report_len: usize,
    pub report_descriptor_sha256: String,
    pub route: String,
    pub display_collection_present: bool,
    pub output_report_id: u8,
    pub output_report_len: usize,
    pub opened_without_io: bool,
    pub closed_after_open: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Result {
    pub schema: u8,
    pub probe_succeeded: bool,
    pub read_only: bool,
    pub no_paint: bool,
    pub command_ready: bool,
    pub hardware_qualified: bool,
    pub candidates: Vec<Candidate>,
    pub hid_output_reports_sent: u8,
    pub image_payloads_sent: u8,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

pub(crate) fn run(backend: &MxCreativeDisplayRouteBackend) -> Result {
    let started = std::time::Instant::now();
    let selected = match select(backend) {
        Ok(selected) => selected,
        Err(error) => return failure(error, elapsed(started)),
    };
    let proof = match backend.open_route_probe(&selected.descriptor) {
        Ok(proof) => proof,
        Err(error) => return failure(error.to_string(), elapsed(started)),
    };
    if !proof.opened
        || !proof.closed
        || proof.descriptor != selected.descriptor
        || proof.report_descriptor != decode_hex(KEYPAD_DESCRIPTOR_HEX)
    {
        return failure("keypad reopened route proof mismatch", elapsed(started));
    }
    let candidate = Candidate {
        vendor_id: selected.descriptor.identity.vendor_id,
        product_id: selected.descriptor.identity.product_id,
        transport: "usb".into(),
        primary_usage_page: selected.descriptor.usage_page,
        primary_usage: selected.descriptor.usage,
        vendor_collection_usages: selected.shape.vendor_collection_usages,
        max_input_report_len: selected.descriptor.max_input_report_len,
        max_output_report_len: selected.descriptor.max_output_report_len,
        report_descriptor_sha256: KEYPAD_DESCRIPTOR_SHA256.into(),
        route: "single_iokit_device".into(),
        display_collection_present: true,
        output_report_id: OUTPUT_REPORT_ID,
        output_report_len: OUTPUT_REPORT_LEN,
        opened_without_io: true,
        closed_after_open: true,
    };
    Result {
        schema: 1,
        probe_succeeded: true,
        read_only: true,
        no_paint: true,
        candidates: vec![candidate],
        elapsed_ms: elapsed(started),
        ..Result::default()
    }
}

struct Selected {
    descriptor: HidDescriptor,
    shape: DescriptorShape,
}

fn select(backend: &MxCreativeDisplayRouteBackend) -> std::result::Result<Selected, String> {
    let matcher = HidMatcher::new(HidIdentity {
        vendor_id: VID,
        product_id: KEYPAD_PID,
    });
    let found = backend
        .enumerate(&matcher, 16)
        .map_err(|error| error.to_string())?;
    let display_specific = found
        .iter()
        .filter(|descriptor| {
            descriptor.usage_page == VENDOR_PAGE && descriptor.usage == DISPLAY_USAGE
        })
        .collect::<Vec<_>>();
    let selected = if display_specific.is_empty() {
        found
            .iter()
            .filter(|descriptor| {
                descriptor.usage_page == VENDOR_PAGE && descriptor.usage == KEYPAD_USAGE
            })
            .collect::<Vec<_>>()
    } else {
        display_specific
    };
    if selected.len() != 1 {
        return Err(format!(
            "keypad output route requires exactly one qualifying collection; found {}",
            selected.len()
        ));
    }
    let descriptor = selected[0].clone();
    verify_descriptor(&descriptor)?;
    let evidence = backend
        .original_report_descriptor_evidence(&descriptor)
        .map_err(|error| error.to_string())?;
    if evidence.source_path != descriptor.path.as_str()
        || digest(&evidence.bytes) != KEYPAD_DESCRIPTOR_SHA256
        || evidence.bytes != decode_hex(KEYPAD_DESCRIPTOR_HEX)
    {
        return Err("keypad output route descriptor mismatch".into());
    }
    let shape = parse_descriptor(&evidence.bytes)
        .map_err(|_| "keypad output route descriptor parse failed".to_owned())?;
    if !shape.vendor_collection_usages.contains(&UsagePair {
        usage_page: VENDOR_PAGE,
        usage: DISPLAY_USAGE,
    }) || !shape.supports_output_report(OUTPUT_REPORT_ID, OUTPUT_REPORT_LEN)
    {
        return Err("keypad display output route absent".into());
    }
    Ok(Selected { descriptor, shape })
}

fn verify_descriptor(descriptor: &HidDescriptor) -> std::result::Result<(), String> {
    let exact_primary = matches!(
        (descriptor.usage_page, descriptor.usage),
        (VENDOR_PAGE, KEYPAD_USAGE) | (VENDOR_PAGE, DISPLAY_USAGE)
    );
    if descriptor.identity.vendor_id != VID
        || descriptor.identity.product_id != KEYPAD_PID
        || !exact_primary
        || descriptor.max_input_report_len != 32
        || descriptor.max_output_report_len != OUTPUT_REPORT_LEN
        || descriptor.bus != Some(HidBus::Usb)
        || descriptor.product.as_deref() != Some("MX Creative Keypad")
        || descriptor.location_id != Some(KEYPAD_LOCATION_ID)
    {
        return Err("keypad output route identity or transport mismatch".into());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DescriptorShape {
    vendor_collection_usages: Vec<UsagePair>,
    output_reports: Vec<(u8, usize)>,
}

impl DescriptorShape {
    fn supports_output_report(&self, id: u8, len: usize) -> bool {
        self.output_reports
            .iter()
            .any(|report| report == &(id, len))
    }
}

fn parse_descriptor(raw: &[u8]) -> std::result::Result<DescriptorShape, ()> {
    let mut cursor = 0;
    let mut usage_page = 0u32;
    let mut report_id = 0u8;
    let mut report_size = 0u32;
    let mut report_count = 0u32;
    let mut local_usages = Vec::new();
    let mut collections = Vec::new();
    let mut vendor_collection_usages = Vec::new();
    let mut outputs: std::collections::BTreeMap<u8, u32> = std::collections::BTreeMap::new();
    while cursor < raw.len() {
        let item = raw[cursor];
        cursor += 1;
        if item == 0xfe {
            return Err(());
        }
        let size_code = item & 3;
        let size = match size_code {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 4,
            _ => unreachable!(),
        };
        if cursor + size > raw.len() {
            return Err(());
        }
        let value = raw[cursor..cursor + size]
            .iter()
            .rev()
            .fold(0u32, |value, byte| (value << 8) | u32::from(*byte));
        cursor += size;
        match item & 0xfc {
            0x04 => usage_page = value,
            0x74 => report_size = value,
            0x84 => {
                report_id = u8::try_from(value).map_err(|_| ())?;
                if report_id == 0 {
                    return Err(());
                }
            }
            0x94 => report_count = value,
            0x08 => {
                if usage_page > u32::from(u16::MAX) || value > u32::from(u16::MAX) {
                    return Err(());
                }
                local_usages.push(UsagePair {
                    usage_page: usage_page as u16,
                    usage: value as u16,
                });
            }
            0xa0 => {
                let usage = *local_usages.last().ok_or(())?;
                collections.push(usage);
                if usage.usage_page == VENDOR_PAGE && !vendor_collection_usages.contains(&usage) {
                    vendor_collection_usages.push(usage);
                }
                local_usages.clear();
            }
            0xc0 => {
                collections.pop().ok_or(())?;
                local_usages.clear();
            }
            0x90 => {
                if collections.is_empty()
                    || !collections.last().is_some_and(|pair| {
                        pair.usage_page == VENDOR_PAGE && pair.usage == DISPLAY_USAGE
                    })
                {
                    local_usages.clear();
                    continue;
                }
                let bits = report_size.checked_mul(report_count).ok_or(())?;
                *outputs.entry(report_id).or_default() = outputs
                    .get(&report_id)
                    .copied()
                    .unwrap_or(0)
                    .checked_add(bits)
                    .ok_or(())?;
                local_usages.clear();
            }
            0x80 => local_usages.clear(),
            0x18 | 0x28 | 0x38 => {
                return Err(());
            }
            _ => {}
        }
    }
    if cursor != raw.len() || !collections.is_empty() {
        return Err(());
    }
    let output_reports = outputs
        .into_iter()
        .map(|(id, bits)| {
            let bytes = bits.div_ceil(8);
            let bytes = usize::try_from(bytes).map_err(|_| ())?;
            Ok::<_, ()>((id, bytes + 1))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(DescriptorShape {
        vendor_collection_usages,
        output_reports,
    })
}

fn failure(error: impl Into<String>, elapsed_ms: u64) -> Result {
    Result {
        schema: 1,
        read_only: true,
        no_paint: true,
        hid_output_reports_sent: 0,
        image_payloads_sent: 0,
        elapsed_ms,
        error: Some(error.into()),
        ..Result::default()
    }
}

fn decode_hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let nibble = |byte: u8| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => unreachable!(),
            };
            (nibble(pair[0]) << 4) | nibble(pair[1])
        })
        .collect()
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn elapsed(started: std::time::Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
pub(crate) fn pinned_fake_result() -> Result {
    let descriptor = HidDescriptor {
        identity: HidIdentity {
            vendor_id: VID,
            product_id: KEYPAD_PID,
        },
        usage_page: VENDOR_PAGE,
        usage: KEYPAD_USAGE,
        product: Some("MX Creative Keypad".into()),
        serial: Some("private".into()),
        max_input_report_len: 32,
        max_output_report_len: OUTPUT_REPORT_LEN,
        interface_number: None,
        bus: Some(HidBus::Usb),
        location_id: Some(KEYPAD_LOCATION_ID),
        path: crate::native::hid::HidPath::new("iokit:42").unwrap(),
    };
    let bytes = decode_hex(KEYPAD_DESCRIPTOR_HEX);
    let fake = crate::native::hid::fake::FakeBackend::new(vec![descriptor.clone()]);
    fake.state.borrow_mut().report_descriptor = Some(bytes.clone());
    fake.state.borrow_mut().route_open_result = Some(crate::native::hid::OutputRouteOpenProof {
        descriptor,
        report_descriptor: bytes,
        opened: true,
        closed: true,
    });
    run(&MxCreativeDisplayRouteBackend::from_test_backend(Box::new(
        fake,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::hid::{HidPath, OutputRouteOpenProof, fake::FakeBackend};

    fn descriptor() -> HidDescriptor {
        HidDescriptor {
            identity: HidIdentity {
                vendor_id: VID,
                product_id: KEYPAD_PID,
            },
            usage_page: VENDOR_PAGE,
            usage: KEYPAD_USAGE,
            product: Some("MX Creative Keypad".into()),
            serial: Some("private".into()),
            max_input_report_len: 32,
            max_output_report_len: 4095,
            interface_number: None,
            bus: Some(HidBus::Usb),
            location_id: Some(KEYPAD_LOCATION_ID),
            path: HidPath::new("iokit:42").unwrap(),
        }
    }

    fn backend() -> (FakeBackend, MxCreativeDisplayRouteBackend) {
        let descriptor = descriptor();
        let bytes = decode_hex(KEYPAD_DESCRIPTOR_HEX);
        let fake = FakeBackend::new(vec![descriptor.clone()]);
        fake.state.borrow_mut().report_descriptor = Some(bytes.clone());
        fake.state.borrow_mut().route_open_result = Some(OutputRouteOpenProof {
            descriptor,
            report_descriptor: bytes,
            opened: true,
            closed: true,
        });
        let state = fake.state.clone();
        let typed = MxCreativeDisplayRouteBackend::from_test_backend(Box::new(fake));
        let _ = state;
        (FakeBackend::new(Vec::new()), typed)
    }

    #[test]
    fn proves_exact_route_without_report_or_image_io() {
        let (_, backend) = backend();
        let result = run(&backend);
        assert!(result.probe_succeeded, "{result:?}");
        assert!(result.read_only && result.no_paint);
        assert_eq!(result.hid_output_reports_sent, 0);
        assert_eq!(result.image_payloads_sent, 0);
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].output_report_id, 20);
        assert_eq!(result.candidates[0].output_report_len, 4095);
        assert_eq!(result.candidates[0].route, "single_iokit_device");
        assert!(result.candidates[0].display_collection_present);
        assert_eq!(
            result.candidates[0].vendor_collection_usages,
            [
                UsagePair {
                    usage_page: VENDOR_PAGE,
                    usage: KEYPAD_USAGE,
                },
                UsagePair {
                    usage_page: VENDOR_PAGE,
                    usage: 0x1a08,
                },
                UsagePair {
                    usage_page: VENDOR_PAGE,
                    usage: DISPLAY_USAGE,
                },
            ]
        );
    }

    #[test]
    fn rejects_wrong_primary_usage_even_with_matching_descriptor() {
        let mut candidate = descriptor();
        candidate.usage = 0x1a09;
        let fake = FakeBackend::new(vec![candidate]);
        fake.state.borrow_mut().report_descriptor = Some(decode_hex(KEYPAD_DESCRIPTOR_HEX));
        let backend = MxCreativeDisplayRouteBackend::from_test_backend(Box::new(fake));
        let result = run(&backend);
        assert!(!result.probe_succeeded);
        assert_eq!(
            result.error.as_deref(),
            Some("keypad output route requires exactly one qualifying collection; found 0")
        );
    }

    #[test]
    fn rejects_forged_post_open_descriptor() {
        let descriptor = descriptor();
        let fake = FakeBackend::new(vec![descriptor.clone()]);
        fake.state.borrow_mut().report_descriptor = Some(decode_hex(KEYPAD_DESCRIPTOR_HEX));
        fake.state.borrow_mut().route_open_result = Some(OutputRouteOpenProof {
            descriptor,
            report_descriptor: vec![0],
            opened: true,
            closed: true,
        });
        let state = fake.state.clone();
        let backend = MxCreativeDisplayRouteBackend::from_test_backend(Box::new(fake));
        let result = run(&backend);
        assert_eq!(
            result.error.as_deref(),
            Some("keypad reopened route proof mismatch")
        );
        assert_eq!(state.borrow().route_open_calls, 1);
        assert!(state.borrow().written.is_empty());
        assert!(state.borrow().inputs.is_empty());
    }

    #[test]
    fn prefers_separate_display_service_without_opening_primary_input() {
        let primary = descriptor();
        let mut display = descriptor();
        display.usage = DISPLAY_USAGE;
        display.path = HidPath::new("iokit:43").unwrap();
        let fake = FakeBackend::new(vec![primary, display.clone()]);
        fake.state.borrow_mut().report_descriptor = Some(decode_hex(KEYPAD_DESCRIPTOR_HEX));
        fake.state.borrow_mut().route_open_result = Some(OutputRouteOpenProof {
            descriptor: display,
            report_descriptor: decode_hex(KEYPAD_DESCRIPTOR_HEX),
            opened: true,
            closed: true,
        });
        let state = fake.state.clone();
        let backend = MxCreativeDisplayRouteBackend::from_test_backend(Box::new(fake));
        let result = run(&backend);
        assert!(result.probe_succeeded, "{result:?}");
        assert_eq!(result.candidates[0].primary_usage, DISPLAY_USAGE);
        assert_eq!(state.borrow().route_open_calls, 1);
        assert_eq!(state.borrow().open_calls, 0);
        assert!(state.borrow().written.is_empty());
    }
}
