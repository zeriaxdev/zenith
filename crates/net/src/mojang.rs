//! Mojang/piston version manifest.

use anyhow::{Context, Result};
use serde::Deserialize;
use zenith_core::VersionEntry;

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Deserialize)]
struct Manifest {
    versions: Vec<VersionEntry>,
}

pub fn fetch_versions() -> Result<Vec<VersionEntry>> {
    let manifest: Manifest = ureq::get(MANIFEST_URL)
        .call()
        .context("fetching version manifest")?
        .into_json()?;
    Ok(manifest.versions)
}
