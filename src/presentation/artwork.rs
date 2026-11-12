//! Original, self-contained status SVGs and their bundled SHA-256 manifest.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::semantic::Token;

#[derive(Clone, Copy, Debug)]
pub struct Artwork {
    pub token: Token,
    pub status: &'static str,
    pub id: &'static str,
    pub filename: &'static str,
    pub svg: &'static str,
}

pub const ARTWORK: [Artwork; 6] = [
    Artwork {
        token: Token::Idle,
        status: "idle",
        id: "state-idle",
        filename: "idle.svg",
        svg: include_str!("../../assets/status/idle.svg"),
    },
    Artwork {
        token: Token::Working,
        status: "working",
        id: "state-working",
        filename: "working.svg",
        svg: include_str!("../../assets/status/working.svg"),
    },
    Artwork {
        token: Token::Complete,
        status: "complete",
        id: "state-complete",
        filename: "complete.svg",
        svg: include_str!("../../assets/status/complete.svg"),
    },
    Artwork {
        token: Token::Waiting,
        status: "waiting",
        id: "state-waiting",
        filename: "waiting.svg",
        svg: include_str!("../../assets/status/waiting.svg"),
    },
    Artwork {
        token: Token::Error,
        status: "error",
        id: "state-error",
        filename: "error.svg",
        svg: include_str!("../../assets/status/error.svg"),
    },
    Artwork {
        token: Token::Unknown,
        status: "unknown",
        id: "state-unknown",
        filename: "unknown.svg",
        svg: include_str!("../../assets/status/unknown.svg"),
    },
];

pub const MANIFEST: &[u8] = include_bytes!("../../assets/status/manifest.json");

pub fn for_token(token: Token) -> Option<&'static Artwork> {
    ARTWORK.iter().find(|artwork| artwork.token == token)
}

#[derive(Debug)]
pub enum ArtworkError {
    InvalidManifest(serde_json::Error),
    UnsupportedVersion,
    WrongCount,
    Duplicate,
    UnexpectedArtifact,
    MissingArtifact,
    WrongSemanticId,
    DigestMismatch,
}

impl std::fmt::Display for ArtworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidManifest(error) => write!(f, "invalid artwork manifest: {error}"),
            Self::UnsupportedVersion => f.write_str("unsupported artwork manifest version"),
            Self::WrongCount => f.write_str("artwork manifest must name exactly six artifacts"),
            Self::Duplicate => f.write_str("duplicate artwork status or filename"),
            Self::UnexpectedArtifact => f.write_str("unexpected artwork status or filename"),
            Self::MissingArtifact => f.write_str("missing artwork artifact"),
            Self::WrongSemanticId => f.write_str("artwork semantic ID does not match"),
            Self::DigestMismatch => f.write_str("artwork SHA-256 does not match"),
        }
    }
}

impl std::error::Error for ArtworkError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u8,
    artifacts: Vec<ManifestEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestEntry {
    status: String,
    id: String,
    file: String,
    sha256: String,
}

/// Verify an explicitly supplied set; production uses the compiled-in set below.
pub fn verify_artifacts(manifest: &[u8], artifacts: &[(&str, &[u8])]) -> Result<(), ArtworkError> {
    let manifest: Manifest =
        serde_json::from_slice(manifest).map_err(ArtworkError::InvalidManifest)?;
    if manifest.version != 1 {
        return Err(ArtworkError::UnsupportedVersion);
    }
    if manifest.artifacts.len() != ARTWORK.len() || artifacts.len() != ARTWORK.len() {
        return Err(ArtworkError::WrongCount);
    }
    let mut supplied = BTreeMap::new();
    for (name, bytes) in artifacts {
        if supplied.insert(*name, *bytes).is_some() {
            return Err(ArtworkError::Duplicate);
        }
    }
    let mut statuses = BTreeSet::new();
    let mut filenames = BTreeSet::new();
    for entry in manifest.artifacts {
        if !statuses.insert(entry.status.clone()) || !filenames.insert(entry.file.clone()) {
            return Err(ArtworkError::Duplicate);
        }
        let expected = ARTWORK
            .iter()
            .find(|artwork| artwork.status == entry.status)
            .ok_or(ArtworkError::UnexpectedArtifact)?;
        if entry.file != expected.filename {
            return Err(ArtworkError::UnexpectedArtifact);
        }
        if entry.id != expected.id {
            return Err(ArtworkError::WrongSemanticId);
        }
        let bytes = supplied
            .get(entry.file.as_str())
            .ok_or(ArtworkError::MissingArtifact)?;
        if !std::str::from_utf8(bytes)
            .is_ok_and(|svg| svg.contains(&format!("id=\"{}\"", expected.id)))
        {
            return Err(ArtworkError::WrongSemanticId);
        }
        if format!("{:x}", Sha256::digest(bytes)) != entry.sha256 {
            return Err(ArtworkError::DigestMismatch);
        }
    }
    Ok(())
}

pub fn verify_bundle() -> Result<(), ArtworkError> {
    let artifacts: Vec<_> = ARTWORK
        .iter()
        .map(|artwork| (artwork.filename, artwork.svg.as_bytes()))
        .collect();
    verify_artifacts(MANIFEST, &artifacts)
}
