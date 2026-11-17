//! Scoped SFTP deployments. Credentials and remote mutations stay in Rust.
pub mod connection;
pub mod local;
mod plan;
mod transaction;
pub use plan::{inspect, DeploymentPlan};
pub use transaction::{apply, recover};

pub async fn read_version(
    remote: &impl connection::Remote,
) -> anyhow::Result<Option<crate::server::ServerVersion>> {
    if remote.stat("auram.json").await?.is_none() {
        return Ok(None);
    }
    Ok(serde_json::from_slice::<crate::server::ServerVersion>(
        &remote.read("auram.json", 1024 * 1024).await?,
    )
    .ok()
    .filter(|v| !v.version.trim().is_empty()))
}

use crate::{
    pack::{Manifest, PackFile, Packs},
    server::ServerVersion,
    storage,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

pub const ROOTS: [&str; 4] = ["mods", "scripts", "config", "kubejs"];
pub fn payload_path(path: &str) -> bool {
    ROOTS
        .iter()
        .any(|root| path.starts_with(&format!("{root}/")))
}
pub fn safe_path(path: &str) -> Result<()> {
    storage::safe_join(Path::new(""), path)?;
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        bail!("Unsafe SFTP path: {path:?}");
    }
    Ok(())
}
pub fn canonical(path: &str) -> String {
    if path.starts_with("mods/") {
        path.strip_suffix(".jar.client")
            .map(|p| format!("{p}.jar"))
            .unwrap_or_else(|| path.into())
    } else {
        path.into()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnedFile {
    pub path: String,
    pub file: PackFile,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Baseline {
    pub schema: u32,
    pub version: ServerVersion,
    pub files: BTreeMap<String, OwnedFile>,
}
pub fn built_payload(packs: &Packs) -> Result<(Manifest, ServerVersion)> {
    let manifest: Manifest =
        storage::read_optional(&packs.layout.state("server-deploy-payload.json"))?
            .context("Build the server once to prepare its deployment payload")?;
    let version: ServerVersion = storage::read_json(&packs.layout.server().join("auram.json"))?;
    if version.version != manifest.tag.name || version.commit != manifest.tag.sha {
        bail!("Built server metadata and payload disagree; rebuild the server");
    }
    Ok((manifest, version))
}

#[cfg(test)]
pub(crate) mod tests;
