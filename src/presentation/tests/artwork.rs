use crate::presentation::artwork::{ARTWORK, MANIFEST, verify_artifacts, verify_bundle};

#[test]
fn status_artwork_is_original_self_contained_and_matches_manifest_bytes() {
    verify_bundle().unwrap();
    for asset in ARTWORK {
        assert!(asset.svg.starts_with("<svg "));
        assert!(asset.svg.contains(&format!("id=\"{}\"", asset.id)));
        assert!(asset.svg.contains("viewBox=\"0 0 64 64\""));
        assert!(!asset.svg.contains("<script"));
        assert!(!asset.svg.contains("href="));
        assert!(!asset.svg.contains("<image"));
        assert!(!asset.svg.contains("<text"));
    }
}

#[test]
fn status_manifest_rejects_missing_duplicate_and_altered_files() {
    let artifacts: Vec<_> = ARTWORK
        .iter()
        .map(|asset| (asset.filename, asset.svg.as_bytes()))
        .collect();
    assert!(verify_artifacts(MANIFEST, &artifacts[..5]).is_err());
    let mut duplicate = artifacts.clone();
    duplicate[1] = duplicate[0];
    assert!(verify_artifacts(MANIFEST, &duplicate).is_err());
    let mut altered = artifacts;
    altered[0].1 = b"<svg id=\"state-idle\">altered</svg>";
    assert!(verify_artifacts(MANIFEST, &altered).is_err());
    let mut manifest: serde_json::Value = serde_json::from_slice(MANIFEST).unwrap();
    manifest["artifacts"][1] = manifest["artifacts"][0].clone();
    assert!(verify_artifacts(&serde_json::to_vec(&manifest).unwrap(), &altered).is_err());
}
