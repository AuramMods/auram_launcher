//! Signed, complete pack inventories and content-addressed partial payloads.
use super::{Manifest, PackFile, PackTag};
use crate::storage;
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::OnceLock,
};

pub const MAX_METADATA: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub sha: String,
    pub size: u64,
    pub mode: String,
    pub sha256: String,
}
impl Entry {
    pub fn file(&self) -> PackFile {
        PackFile {
            sha: self.sha.clone(),
            size: self.size,
            mode: self.mode.clone(),
            source: None,
        }
    }
    fn validate(&self) -> Result<()> {
        hash(&self.sha, 40)?;
        hash(&self.sha256, 64)?;
        if !matches!(self.mode.as_str(), "100644" | "100755") {
            bail!("Unsupported distribution file mode");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Change {
    pub before: Option<Entry>,
    pub after: Option<Entry>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transition {
    pub target: PackTag,
    pub base: Option<PackTag>,
    pub changes: BTreeMap<String, Change>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Member {
    pub size: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bundle {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub members: BTreeMap<String, Member>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub schema: u32,
    pub policy: u32,
    pub repository: String,
    pub target: PackTag,
    pub producer: String,
    pub pack_ignore: String,
    pub files: BTreeMap<String, Entry>,
    pub history: Vec<Transition>,
    pub bundles: BTreeMap<String, Bundle>,
    pub objects: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub payload: String,
    pub signature: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    pub schema: u32,
    pub version: u32,
    pub roots: Vec<String>,
    pub directories: Vec<String>,
    pub excluded_minecraft_prefixes: Vec<String>,
    pub excluded_server_names: Vec<String>,
    pub excluded_server_prefixes: Vec<String>,
    pub replacement_directories: Vec<String>,
    pub maximum_update_bundles: usize,
    pub small_object_bytes: u64,
    pub small_bundle_bytes: u64,
    pub maximum_bundle_bytes: u64,
}
pub fn policy() -> &'static Policy {
    static POLICY: OnceLock<Policy> = OnceLock::new();
    POLICY.get_or_init(|| {
        serde_json::from_str(include_str!("../../pack-policy.json")).expect("compiled pack policy")
    })
}
pub fn selected(path: &str) -> bool {
    let policy = policy();
    if policy.roots.iter().any(|p| p == path) {
        return true;
    }
    if let Some(relative) = path.strip_prefix("minecraft/") {
        return !policy
            .excluded_minecraft_prefixes
            .iter()
            .any(|p| relative.starts_with(p));
    }
    if let Some(relative) = path.strip_prefix("server_files/") {
        return !policy
            .excluded_server_names
            .iter()
            .any(|p| relative.rsplit('/').next() == Some(p.as_str()))
            && !policy
                .excluded_server_prefixes
                .iter()
                .any(|p| relative.starts_with(p));
    }
    false
}
fn hash(value: &str, length: usize) -> Result<()> {
    if value.len() != length
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        bail!("Invalid distribution hash");
    }
    Ok(())
}
fn identity(tag: &PackTag) -> Result<()> {
    hash(&tag.sha, 40)?;
    if super::version(&tag.name).is_none()
        || !tag
            .name
            .trim_start_matches('v')
            .bytes()
            .all(|c| c.is_ascii_digit() || c == b'.')
    {
        bail!("Invalid distribution version");
    }
    Ok(())
}
impl Envelope {
    pub fn verify(&self, public_key: &str) -> Result<Catalog> {
        if self.payload.len() > MAX_METADATA {
            bail!("Pack metadata is too large");
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(&self.payload)?;
        let key =
            minisign_verify::PublicKey::decode(public_key).context("Read pack verification key")?;
        let signature =
            minisign_verify::Signature::decode(&self.signature).context("Read pack signature")?;
        key.verify(&bytes, &signature, false)
            .context("Invalid pack metadata signature")?;
        let catalog: Catalog = serde_json::from_slice(&bytes)?;
        catalog.validate()?;
        Ok(catalog)
    }
}
impl Catalog {
    pub fn manifest(&self) -> Manifest {
        Manifest {
            schema: 1,
            policy: 0,
            tag: self.target.clone(),
            files: self
                .files
                .iter()
                .map(|(p, f)| (p.clone(), f.file()))
                .collect(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema != 1
            || self.policy != policy().version
            || self.repository != "AuramMods/Auram"
        {
            bail!("Unsupported pack distribution schema/policy/repository");
        }
        identity(&self.target)?;
        hash(&self.producer, 40)?;
        super::source::validate_inventory(&self.manifest())?;
        if !self.files.contains_key("mmc-pack.json") {
            bail!("Distribution is missing mmc-pack.json");
        }
        for (path, file) in &self.files {
            if !selected(path) {
                bail!("Unselected distribution path: {path}");
            }
            file.validate()?;
        }
        let mut current = Some(&self.target);
        let mut seen = BTreeSet::new();
        let mut snapshot = self.files.clone();
        for link in &self.history {
            identity(&link.target)?;
            if !current
                .is_some_and(|tag| tag.name == link.target.name && tag.sha == link.target.sha)
                || !seen.insert(link.target.sha.clone())
            {
                bail!("Broken/cyclic distribution history");
            }
            for (path, change) in &link.changes {
                storage::safe_join(Path::new(""), path)?;
                if !selected(path) || change.before.is_none() && change.after.is_none() {
                    bail!("Invalid history change");
                }
                for file in [&change.before, &change.after].into_iter().flatten() {
                    file.validate()?;
                }
                if snapshot.get(path) != change.after.as_ref() {
                    bail!("History disagrees with its target snapshot");
                }
                if let Some(before) = &change.before {
                    snapshot.insert(path.clone(), before.clone());
                } else {
                    snapshot.remove(path);
                }
            }
            if let Some(tag) = &link.base {
                identity(tag)?;
                if seen.contains(&tag.sha) {
                    bail!("Cyclic distribution predecessor");
                }
            }
            current = link.base.as_ref();
        }
        for (sha, bundle) in &self.bundles {
            hash(sha, 64)?;
            if sha != &bundle.sha256
                || bundle.size == 0
                || bundle.size >= policy().maximum_bundle_bytes
            {
                bail!("Invalid bundle hash/size");
            }
            let url = url::Url::parse(&bundle.url)?;
            let parts = url
                .path_segments()
                .context("Invalid bundle URL")?
                .collect::<Vec<_>>();
            if url.scheme() != "https"
                || url.host_str() != Some("github.com")
                || url.port().is_some()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || parts.len() != 6
                || parts[..4] != ["AuramMods", "Auram", "releases", "download"]
                || super::version(parts[4]).is_none()
                || parts[5] != format!("pack-{sha}.zip")
            {
                bail!("Bundle URL is not an immutable pack asset");
            }
            for (oid, member) in &bundle.members {
                hash(oid, 40)?;
                hash(&member.sha256, 64)?;
            }
        }
        for (oid, location) in &self.objects {
            hash(oid, 40)?;
            if !self
                .bundles
                .get(location)
                .is_some_and(|b| b.members.contains_key(oid))
            {
                bail!("Invalid object locator");
            }
        }
        for file in self.files.values() {
            if let Some(location) = self.objects.get(&file.sha) {
                let member = &self.bundles[location].members[&file.sha];
                if member.size != file.size || member.sha256 != file.sha256 {
                    bail!("Object locator disagrees with target file");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn producer_fixture_and_shared_scope_are_valid() {
        let catalog: Catalog =
            serde_json::from_str(include_str!("../../pack-fixture.json")).unwrap();
        catalog.validate().unwrap();
        assert!(selected("minecraft/schematics/世界.nbt"));
        for path in [
            "minecraft/saves/world/level.dat",
            "server_files/ops.json",
            "server_files/eula.txt",
            "minecraft/assets/object",
            "scripts/release.py",
        ] {
            assert!(!selected(path), "{path}");
        }
    }
    #[test]
    fn unsigned_and_mismatched_metadata_cannot_be_trusted() {
        let fixture = include_bytes!("../../pack-fixture.json");
        let envelope = Envelope {
            payload: base64::engine::general_purpose::STANDARD.encode(fixture),
            signature: "fake".into(),
        };
        assert!(envelope.verify(include_str!("../../pack.pub")).is_err());
        let mut catalog: Catalog = serde_json::from_slice(fixture).unwrap();
        catalog.files.get_mut("mmc-pack.json").unwrap().mode = "120000".into();
        assert!(catalog.validate().is_err());
    }
    #[test]
    fn locally_signed_producer_fixture_verifies_and_tampering_fails() {
        let mut envelope: Envelope =
            serde_json::from_str(include_str!("../../fixture.envelope.json")).unwrap();
        let public_key = include_str!("../../fixture.pub");
        let catalog = envelope.verify(public_key).unwrap();
        assert_eq!(catalog.target.name, "7");
        assert!(envelope.verify(include_str!("../../pack.pub")).is_err());
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(&envelope.payload)
            .unwrap();
        bytes[5] ^= 1;
        envelope.payload = base64::engine::general_purpose::STANDARD.encode(bytes);
        assert!(envelope.verify(public_key).is_err());
    }
}
