//! Validation of exact Windows native evidence; no native calls.

use super::{HidBus, HidDescriptor, HidError, HidMatcher, HidOp, validate_descriptor};

#[cfg(windows)]
use windows_sys::Win32::Foundation::ERROR_NO_MORE_ITEMS;
#[cfg(not(windows))]
const ERROR_NO_MORE_ITEMS: u32 = 259;

const GUID_LEN: usize = 16;
const DEVPROP_TYPE_GUID: u32 = 13;
pub(super) const MAX_INTERFACE_SCAN: u32 = 4096;
const MAX_BUS_NODES: usize = 16;
// GUID_BUS_TYPE_USB from Wdmguid.h, in the native GUID memory layout.
const USB_GUID: [u8; GUID_LEN] = [
    0xbc, 0xeb, 0x7d, 0x9d, 0x5d, 0xc8, 0xd1, 0x11, 0x9e, 0xb4, 0x00, 0x60, 0x08, 0xc3, 0xa1, 0x9a,
];
const HID_GUID: [u8; GUID_LEN] = [
    0xd0, 0x37, 0xaf, 0xee, 0x63, 0x19, 0xc4, 0x47, 0xaa, 0x48, 0x72, 0x47, 0x6d, 0xb7, 0xcf, 0x49,
];

pub(super) trait BusApi {
    fn property(
        &mut self,
        node: u32,
        buffer: &mut [u8; GUID_LEN],
    ) -> Result<Option<(u32, usize)>, HidError>;
    fn parent(&mut self, node: u32) -> Result<u32, HidError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BusNode {
    node: u32,
    guid: Option<[u8; GUID_LEN]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BusEvidence(Vec<BusNode>);

impl BusEvidence {
    pub(super) fn read(mut node: u32, api: &mut impl BusApi) -> Result<Self, HidError> {
        let mut nodes: Vec<BusNode> = Vec::new();
        loop {
            if nodes.iter().any(|seen| seen.node == node) {
                return Err(HidError::Protocol("HID bus ancestry cycle"));
            }
            let mut bytes = [0; GUID_LEN];
            let guid = match api.property(node, &mut bytes)? {
                Some((ty, len)) if ty == DEVPROP_TYPE_GUID && len == GUID_LEN => Some(bytes),
                Some(_) => {
                    return Err(HidError::Protocol("invalid bus GUID property type or size"));
                }
                None => None,
            };
            nodes.push(BusNode { node, guid });
            // Only an exact HID logical bus permits traversal. Do not cross an
            // unknown/missing transport to find (e.g.) a USB Bluetooth adapter.
            if guid != Some(HID_GUID) {
                return Ok(Self(nodes));
            }
            if nodes.len() == MAX_BUS_NODES {
                return Err(HidError::Protocol("HID bus ancestry limit reached"));
            }
            node = api.parent(node)?;
        }
    }

    pub(super) fn classification(&self) -> Option<HidBus> {
        self.0.last()?.guid.map(|guid| {
            if guid == USB_GUID {
                HidBus::Usb
            } else {
                HidBus::Unknown
            }
        })
    }
}

/// Only the enumeration call's ERROR_NO_MORE_ITEMS completes the inventory;
/// even that code from a detail call is a failure, not end-of-scan.
pub(super) fn scan_interfaces<T, U>(
    mut next: impl FnMut(u32) -> Result<T, u32>,
    mut detail: impl FnMut(T) -> Result<U, HidError>,
) -> Result<Vec<U>, HidError> {
    let mut interfaces = Vec::new();
    for index in 0..MAX_INTERFACE_SCAN {
        match next(index) {
            Ok(interface) => interfaces.push(detail(interface)?),
            Err(ERROR_NO_MORE_ITEMS) => return Ok(interfaces),
            Err(code) => {
                return Err(HidError::Os {
                    op: HidOp::Enumerate,
                    code: i64::from(code),
                });
            }
        }
    }
    Err(HidError::Protocol("HID interface scan limit reached"))
}

pub(super) fn collect_candidates<T>(
    interfaces: impl IntoIterator<Item = T>,
    matcher: &HidMatcher,
    limit: usize,
    mut resolve: impl FnMut(T) -> Result<Option<WindowsEvidence>, HidError>,
) -> Result<Vec<WindowsEvidence>, HidError> {
    let mut candidates = Vec::new();
    for interface in interfaces {
        if let Some(entry) = resolve(interface)?
            && matcher.matches(&entry.descriptor)
        {
            validate_descriptor(&entry.descriptor)?;
            if candidates.len() == limit {
                return Err(HidError::TooManyDevices);
            }
            candidates.push(entry);
        }
    }
    Ok(candidates)
}

#[derive(Clone)]
pub(super) struct WindowsEvidence {
    pub(super) descriptor: HidDescriptor,
    pub(super) bus: BusEvidence,
}

/// The same property/handle readers run again after CreateFileW. Preserve the
/// raw GUID too: two different unsupported buses must not compare as Unknown.
pub(super) fn revalidate_open(
    expected: &WindowsEvidence,
    allow_usage_change: bool,
    read: impl FnOnce() -> Result<WindowsEvidence, HidError>,
) -> Result<(), HidError> {
    let mut actual = read()?;
    if allow_usage_change {
        actual.descriptor.usage_page = expected.descriptor.usage_page;
        actual.descriptor.usage = expected.descriptor.usage;
    }
    if actual.descriptor != expected.descriptor || actual.bus != expected.bus {
        return Err(HidError::NotFound);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::hid::{HidIdentity, HidOp, HidPath};

    type Property = Result<Option<([u8; 16], u32, usize)>, HidError>;

    struct FakeBusApi {
        properties: Vec<Property>,
        parents: Vec<Result<u32, HidError>>,
        property_calls: usize,
        parent_calls: usize,
    }

    impl FakeBusApi {
        fn new(properties: Vec<Property>) -> Self {
            Self {
                properties,
                parents: Vec::new(),
                property_calls: 0,
                parent_calls: 0,
            }
        }
    }

    impl BusApi for FakeBusApi {
        fn property(
            &mut self,
            node: u32,
            buffer: &mut [u8; 16],
        ) -> Result<Option<(u32, usize)>, HidError> {
            self.property_calls += 1;
            self.properties[node as usize].clone().map(|property| {
                property.map(|(guid, ty, len)| {
                    buffer.copy_from_slice(&guid);
                    (ty, len)
                })
            })
        }

        fn parent(&mut self, node: u32) -> Result<u32, HidError> {
            self.parent_calls += 1;
            self.parents
                .get(node as usize)
                .cloned()
                .unwrap_or(Ok(node + 1))
        }
    }

    fn guid_property(guid: [u8; 16]) -> Property {
        Ok(Some((guid, DEVPROP_TYPE_GUID, 16)))
    }

    fn property(guid: [u8; 16], ty: u32, len: usize) -> Result<BusEvidence, HidError> {
        BusEvidence::read(0, &mut FakeBusApi::new(vec![Ok(Some((guid, ty, len)))]))
    }

    fn missing_property() -> BusEvidence {
        BusEvidence::read(0, &mut FakeBusApi::new(vec![Ok(None)])).unwrap()
    }

    fn evidence(guid: [u8; 16]) -> WindowsEvidence {
        let bus = property(guid, DEVPROP_TYPE_GUID, 16).unwrap();
        WindowsEvidence {
            descriptor: HidDescriptor {
                identity: HidIdentity {
                    vendor_id: 0x0300,
                    product_id: 0x3010,
                },
                usage_page: 0xff00,
                usage: 1,
                product: Some("USB Bluetooth mi_00".into()),
                serial: None,
                max_input_report_len: 64,
                max_output_report_len: 64,
                interface_number: None,
                bus: bus.classification(),
                path: HidPath::new(r"\\?\hid#vid_0300&pid_3010&mi_00").unwrap(),
            },
            bus,
        }
    }

    #[test]
    fn bus_requires_success_exact_guid_type_and_size() {
        assert_eq!(
            property(USB_GUID, DEVPROP_TYPE_GUID, 16)
                .unwrap()
                .classification(),
            Some(HidBus::Usb)
        );
        for (ty, len) in [
            (0, 16),
            (18, 16),                         // DEVPROP_TYPE_STRING
            (0x1000 | DEVPROP_TYPE_GUID, 16), // An array is not a GUID.
            (DEVPROP_TYPE_GUID, 0),
            (DEVPROP_TYPE_GUID, 15),
            (DEVPROP_TYPE_GUID, 17),
            (DEVPROP_TYPE_GUID, usize::MAX),
        ] {
            assert!(matches!(
                property(USB_GUID, ty, len),
                Err(HidError::Protocol(_))
            ));
        }
        assert_eq!(missing_property().classification(), None);
    }

    #[test]
    fn unknown_buses_and_bluetooth_class_guid_are_not_transport_evidence() {
        // GUID_DEVCLASS_BLUETOOTH is a setup class, not a documented bus GUID.
        let bluetooth_class = [
            0x6c, 0xf0, 0xcb, 0xe0, 0x8b, 0xcd, 0x47, 0x46, 0xbb, 0x8a, 0x26, 0x3b, 0x43, 0xf0,
            0xf9, 0x74,
        ];
        for guid in [[0; 16], [0xff; 16], bluetooth_class] {
            assert_eq!(evidence(guid).descriptor.bus, Some(HidBus::Unknown));
        }
    }

    #[test]
    fn revalidation_rejects_identity_metadata_and_raw_bus_changes() {
        let expected = evidence(USB_GUID);
        revalidate_open(&expected, false, || Ok(expected.clone())).unwrap();
        let mut changes = Vec::new();
        let mut changed = expected.clone();
        changed.descriptor.identity.product_id += 1;
        changes.push(changed);
        let mut changed = expected.clone();
        changed.descriptor.serial = Some("replacement".into());
        changes.push(changed);
        let mut changed = expected.clone();
        changed.descriptor.max_output_report_len += 1;
        changes.push(changed);
        let mut changed = expected.clone();
        changed.descriptor.interface_number = Some(0);
        changes.push(changed);
        let mut changed = expected.clone();
        changed.bus = missing_property();
        changed.descriptor.bus = None;
        changes.push(changed);
        changes.push(evidence([0xff; 16]));
        for changed in changes {
            assert_eq!(
                revalidate_open(&expected, true, || Ok(changed)),
                Err(HidError::NotFound)
            );
        }
        assert_eq!(
            revalidate_open(&evidence([0; 16]), false, || Ok(evidence([0xff; 16]))),
            Err(HidError::NotFound)
        );
        assert_eq!(
            revalidate_open(&expected, false, || Err(HidError::NotFound)),
            Err(HidError::NotFound)
        );
    }

    #[test]
    fn composite_reopen_may_change_only_usage() {
        let expected = evidence(USB_GUID);
        let mut actual = expected.clone();
        actual.descriptor.usage_page = 1;
        actual.descriptor.usage = 2;
        assert_eq!(
            revalidate_open(&expected, false, || Ok(actual.clone())),
            Err(HidError::NotFound)
        );
        revalidate_open(&expected, true, || Ok(actual)).unwrap();
    }

    #[test]
    fn hid_collection_walks_exact_hid_parents_to_usb() {
        let mut api = FakeBusApi::new(vec![
            guid_property(HID_GUID),
            guid_property(HID_GUID),
            guid_property(USB_GUID),
        ]);
        assert_eq!(
            BusEvidence::read(0, &mut api).unwrap().classification(),
            Some(HidBus::Usb)
        );
        assert_eq!((api.property_calls, api.parent_calls), (3, 2));
        let mut properties = vec![guid_property(HID_GUID); MAX_BUS_NODES];
        properties[MAX_BUS_NODES - 1] = guid_property(USB_GUID);
        let mut api = FakeBusApi::new(properties);
        assert_eq!(
            BusEvidence::read(0, &mut api).unwrap().classification(),
            Some(HidBus::Usb)
        );
        assert_eq!(
            (api.property_calls, api.parent_calls),
            (MAX_BUS_NODES, MAX_BUS_NODES - 1)
        );
    }

    #[test]
    fn unknown_and_missing_properties_stop_before_usb_adapter_ancestors() {
        for property in [guid_property([0xff; 16]), Ok(None)] {
            let expected = if property.as_ref().unwrap().is_some() {
                Some(HidBus::Unknown)
            } else {
                None
            };
            let mut api = FakeBusApi::new(vec![
                guid_property(HID_GUID),
                property,
                guid_property(USB_GUID),
            ]);
            assert_eq!(
                BusEvidence::read(0, &mut api).unwrap().classification(),
                expected
            );
            assert_eq!((api.property_calls, api.parent_calls), (2, 1));
        }
    }

    #[test]
    fn ancestry_native_errors_cycles_and_cap_fail_closed() {
        let error = HidError::Os {
            op: HidOp::Enumerate,
            code: 51,
        };
        let mut api = FakeBusApi::new(vec![guid_property(HID_GUID), Err(error.clone())]);
        assert_eq!(BusEvidence::read(0, &mut api), Err(error.clone()));
        let mut api = FakeBusApi::new(vec![guid_property(HID_GUID)]);
        api.parents.push(Err(error.clone()));
        assert_eq!(BusEvidence::read(0, &mut api), Err(error));
        let mut api = FakeBusApi::new(vec![guid_property(HID_GUID)]);
        api.parents.push(Ok(0));
        assert!(matches!(
            BusEvidence::read(0, &mut api),
            Err(HidError::Protocol(_))
        ));
        assert_eq!((api.property_calls, api.parent_calls), (1, 1));
        let mut api = FakeBusApi::new(vec![guid_property(HID_GUID); MAX_BUS_NODES]);
        assert!(matches!(
            BusEvidence::read(0, &mut api),
            Err(HidError::Protocol(_))
        ));
        assert_eq!(
            (api.property_calls, api.parent_calls),
            (MAX_BUS_NODES, MAX_BUS_NODES - 1)
        );
    }

    #[test]
    fn revalidation_binds_ancestry_and_preserves_native_error() {
        let mut expected = evidence(USB_GUID);
        expected.bus = BusEvidence::read(
            0,
            &mut FakeBusApi::new(vec![
                guid_property(HID_GUID),
                guid_property(USB_GUID),
                guid_property(USB_GUID),
            ]),
        )
        .unwrap();
        let mut actual = expected.clone();
        let mut changed_parent = FakeBusApi::new(vec![
            guid_property(HID_GUID),
            guid_property(USB_GUID),
            guid_property(USB_GUID),
        ]);
        changed_parent.parents.push(Ok(2));
        actual.bus = BusEvidence::read(0, &mut changed_parent).unwrap();
        assert_eq!(
            revalidate_open(&expected, false, || Ok(actual)),
            Err(HidError::NotFound)
        );
        let error = HidError::Os {
            op: HidOp::Open,
            code: 51,
        };
        let result = revalidate_open(&expected, false, || {
            let mut actual = expected.clone();
            actual.bus = BusEvidence::read(0, &mut FakeBusApi::new(vec![Err(error.clone())]))?;
            Ok(actual)
        });
        assert_eq!(result, Err(error));
    }

    #[test]
    fn incomplete_scan_never_returns_a_false_unique_inventory() {
        let mut calls = 0;
        let error = HidError::Os {
            op: HidOp::Enumerate,
            code: 31,
        };
        assert_eq!(
            scan_interfaces(
                |index| {
                    calls += 1;
                    if index == 0 { Ok(0) } else { Err(31) }
                },
                Ok
            ),
            Err(error)
        );
        assert_eq!(calls, 2);
        assert_eq!(
            scan_interfaces(
                |index| if index == 0 {
                    Ok(index)
                } else {
                    Err(ERROR_NO_MORE_ITEMS)
                },
                Ok
            ),
            Ok(vec![0])
        );
        let completed = scan_interfaces(
            |index| {
                if index < MAX_INTERFACE_SCAN - 1 {
                    Ok(index)
                } else {
                    Err(ERROR_NO_MORE_ITEMS)
                }
            },
            Ok,
        )
        .unwrap();
        assert_eq!(completed.len(), (MAX_INTERFACE_SCAN - 1) as usize);
        let mut calls = 0;
        assert!(matches!(
            scan_interfaces(
                |index| {
                    calls += 1;
                    Ok(index)
                },
                Ok
            ),
            Err(HidError::Protocol(_))
        ));
        assert_eq!(calls, MAX_INTERFACE_SCAN);
        let detail_error = HidError::Os {
            op: HidOp::Enumerate,
            code: i64::from(ERROR_NO_MORE_ITEMS),
        };
        assert_eq!(
            scan_interfaces(Ok, |_| Err::<(), _>(detail_error.clone())),
            Err(detail_error)
        );
    }

    #[test]
    fn matching_candidate_resolution_failure_never_hides_a_second_device() {
        let first = evidence(USB_GUID);
        let matcher = HidMatcher::new(first.descriptor.identity);
        for code in [5, 31, i64::from(0xc0110001u32 as i32), 51] {
            // Open, attributes, caps, CM property failures.
            let error = HidError::Os {
                op: HidOp::Enumerate,
                code,
            };
            let result = collect_candidates([0, 1], &matcher, 64, |index| {
                if index == 0 {
                    Ok(Some(first.clone()))
                } else {
                    Err(error.clone())
                }
            });
            assert!(matches!(result, Err(actual) if actual == error));
        }
        let mut second = first.clone();
        second.descriptor.path = HidPath::new("second").unwrap();
        second.bus = missing_property();
        second.descriptor.bus = None;
        let candidates =
            collect_candidates([first, second], &matcher, 64, |entry| Ok(Some(entry))).unwrap();
        assert_eq!(candidates.len(), 2);
    }
}
