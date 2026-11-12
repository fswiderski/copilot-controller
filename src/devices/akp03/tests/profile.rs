use super::super::{
    Profile,
    profile::{
        HidIdentity, REV2_REPORT_DESCRIPTOR, match_identity, validate_rev2_report_descriptor,
    },
};

#[test]
fn identity_matching_is_exact_and_does_not_generalize_related_products() {
    assert_eq!(Profile::default(), Profile::Original);
    for profile in [Profile::Original, Profile::Rev2] {
        assert_eq!(match_identity(profile.identity()), Some(profile));
        let json = serde_json::to_string(&profile).unwrap();
        assert_eq!(json, format!("\"{}\"", profile.id()));
        assert_eq!(serde_json::from_str::<Profile>(&json).unwrap(), profile);
    }
    let original = Profile::Original.identity();
    let rev2 = Profile::Rev2.identity();
    for identity in [
        HidIdentity {
            product_id: 0x1002,
            ..original
        },
        HidIdentity {
            product_id: 0x3003,
            ..rev2
        },
        HidIdentity {
            vendor_id: 0x0301,
            ..rev2
        },
        HidIdentity {
            usage_page: 1,
            usage: 6,
            ..rev2
        },
        HidIdentity { usage: 2, ..rev2 },
    ] {
        assert_eq!(match_identity(identity), None);
    }
}

#[test]
fn revision_two_descriptor_requires_byte_for_byte_equality() {
    assert!(validate_rev2_report_descriptor(&REV2_REPORT_DESCRIPTOR).is_ok());
    let mut changed = REV2_REPORT_DESCRIPTOR;
    changed[48] ^= 1;
    assert!(validate_rev2_report_descriptor(&changed).is_err());
    assert!(validate_rev2_report_descriptor(&[]).is_err());
    assert!(validate_rev2_report_descriptor(&[0x05, 0x01, 0x09, 0x06]).is_err());
    assert!(validate_rev2_report_descriptor(&REV2_REPORT_DESCRIPTOR[..53]).is_err());
    let mut extra = REV2_REPORT_DESCRIPTOR.to_vec();
    extra.push(0);
    assert!(validate_rev2_report_descriptor(&extra).is_err());
}
