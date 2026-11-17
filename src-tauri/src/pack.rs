use crate::{
    download::{Checksum, Download, Downloader},
    jobs::Job,
    storage::{self, Layout},
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const API: &str = "https://api.github.com/repos/AuramMods/Auram";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackTag {
    pub name: String,
    pub sha: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackFile {
    pub sha: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub tag: PackTag,
    pub files: BTreeMap<String, PackFile>,
}
#[derive(Deserialize)]
struct TagResponse {
    name: String,
    commit: Commit,
}
#[derive(Deserialize)]
struct Commit {
    sha: String,
}
#[derive(Deserialize)]
struct Tree {
    tree: Vec<TreeEntry>,
    #[serde(default)]
    truncated: bool,
}
#[derive(Deserialize)]
struct TreeEntry {
    path: String,
    sha: String,
    #[serde(rename = "type")]
    kind: String,
    mode: String,
    size: Option<u64>,
}

#[derive(Clone)]
pub struct Packs {
    pub layout: Layout,
    pub net: Downloader,
}
impl Packs {
    pub fn installed(&self) -> Result<Option<Manifest>> {
        storage::read_optional(&self.layout.client().join(".auram-manifest.json"))
    }
    pub async fn latest(&self) -> Result<PackTag> {
        let mut tags = Vec::new();
        for page in 1..=100 {
            let batch: Vec<TagResponse> = self
                .net
                .json(&format!("{API}/tags?per_page=100&page={page}"))
                .await?;
            let length = batch.len();
            tags.extend(
                batch
                    .into_iter()
                    .filter(|tag| version(&tag.name).is_some())
                    .map(|tag| PackTag {
                        name: tag.name,
                        sha: tag.commit.sha,
                    }),
            );
            if length < 100 {
                break;
            }
        }
        tags.sort_by(|a, b| version(&b.name).cmp(&version(&a.name)));
        tags.into_iter()
            .next()
            .context("The pack repository has no version tags")
    }
    pub async fn manifest(&self, tag: PackTag, job: &Job) -> Result<Manifest> {
        let cache = self.layout.state(&format!("pack-{}.json", tag.sha));
        if let Some(value) = storage::read_optional::<Manifest>(&cache)? {
            return Ok(value);
        }
        job.progress("pack", "Reading pack file manifest", 0, 0);
        let recursive: Tree = self
            .net
            .json(&format!("{API}/git/trees/{}?recursive=1", tag.sha))
            .await?;
        let mut entries = recursive.tree;
        if recursive.truncated {
            entries = Vec::new();
            let mut queue = vec![(String::new(), tag.sha.clone())];
            while let Some((prefix, sha)) = queue.pop() {
                job.check()?;
                let tree: Tree = self.net.json(&format!("{API}/git/trees/{sha}")).await?;
                if tree.truncated {
                    bail!("GitHub returned an incomplete pack directory");
                }
                for mut entry in tree.tree {
                    entry.path = format!("{prefix}{}", entry.path);
                    if entry.kind == "tree" {
                        queue.push((format!("{}/", entry.path), entry.sha));
                    } else {
                        entries.push(entry);
                    }
                }
            }
        }
        let files = entries
            .into_iter()
            .filter(|e| e.kind == "blob" && e.mode != "120000")
            .map(|e| {
                (
                    e.path,
                    PackFile {
                        sha: e.sha,
                        size: e.size.unwrap_or(0),
                        source: None,
                    },
                )
            })
            .collect();
        let manifest = Manifest {
            schema: 1,
            tag,
            files,
        };
        storage::write_json(&cache, &manifest)?;
        Ok(manifest)
    }
    pub fn blob_path(&self, file: &PackFile) -> PathBuf {
        self.layout.root.join("cache/blobs").join(&file.sha)
    }
    pub fn download(&self, manifest: &Manifest, path: &str, file: &PackFile) -> Result<Download> {
        let mut url = url::Url::parse("https://raw.githubusercontent.com/")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid raw URL"))?
            .extend(["AuramMods", "Auram", &manifest.tag.sha])
            .extend(file.source.as_deref().unwrap_or(path).split('/'));
        Ok(Download {
            url: url.into(),
            target: self.blob_path(file),
            size: Some(file.size),
            checksum: Checksum::GitBlob(file.sha.clone()),
        })
    }
    pub async fn file(&self, manifest: &Manifest, path: &str, job: &Job) -> Result<PathBuf> {
        let file = manifest
            .files
            .get(path)
            .with_context(|| format!("Pack is missing {path}"))?;
        let item = self.download(manifest, path, file)?;
        self.net.get(&item, job).await?;
        Ok(item.target)
    }
    pub async fn client_manifest(&self, manifest: &Manifest, job: &Job) -> Result<Manifest> {
        let mut ignore = ignore::gitignore::GitignoreBuilder::new("");
        if manifest.files.contains_key(".packignore") {
            for line in fs::read_to_string(self.file(manifest, ".packignore", job).await?)?.lines()
            {
                ignore.add_line(None, line)?;
            }
        }
        let ignore = ignore.build()?;
        let mut result = manifest.clone();
        result.files.retain(|path, _| {
            let selected = ["mmc-pack.json", "instance.cfg", "icon-256.png"]
                .contains(&path.as_str())
                || path.starts_with("minecraft/");
            selected
                && !ignore.matched_path_or_any_parents(path, false).is_ignore()
                && !runtime_or_userdata(path)
        });
        if !result.files.contains_key("mmc-pack.json") {
            bail!("Pack manifest does not contain mmc-pack.json");
        }
        Ok(result)
    }
    pub async fn update(&self, repair: bool, job: &Job) -> Result<Manifest> {
        storage::recover(
            &self.layout.client(),
            &self.layout.state("client-transaction"),
        )?;
        let old = self.installed()?;
        let tag = if repair {
            old.as_ref()
                .context("Install the pack before repairing it")?
                .tag
                .clone()
        } else {
            match self.latest().await {
                Ok(tag) => tag,
                Err(error) => {
                    if let Some(installed) = old {
                        job.progress(
                            "pack",
                            format!("Using installed pack; update check unavailable: {error}"),
                            0,
                            0,
                        );
                        return Ok(installed);
                    }
                    return Err(error);
                }
            }
        };
        let full = self.manifest(tag, job).await?;
        let desired = self.client_manifest(&full, job).await?;
        self.reconcile(
            &self.layout.client(),
            &self.layout.state("client-transaction"),
            old.as_ref(),
            &desired,
            job,
            false,
        )
        .await
    }
    pub async fn reconcile(
        &self,
        root: &Path,
        transaction: &Path,
        old: Option<&Manifest>,
        desired: &Manifest,
        job: &Job,
        server: bool,
    ) -> Result<Manifest> {
        storage::recover(root, transaction)?;
        let mut next = desired.clone();
        next.files.clear();
        let mut downloads = Vec::new();
        let mut changes = Vec::new();
        let backup = self.layout.root.join("backups").join(format!(
            "{}-{}",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            rand::random::<u32>()
        ));
        for (relative, file) in &desired.files {
            job.check()?;
            let target = storage::safe_join(root, relative)?;
            let previous = old.and_then(|m| m.files.get(relative));
            if target.exists() {
                let valid = Checksum::GitBlob(file.sha.clone()).verify(&target)?;
                if valid {
                    next.files.insert(relative.clone(), file.clone());
                    continue;
                }
                let modified_server_config = server
                    && ["config/", "defaultconfigs/"]
                        .iter()
                        .any(|prefix| relative.starts_with(prefix))
                    && previous.is_none_or(|file| {
                        !Checksum::GitBlob(file.sha.clone())
                            .verify(&target)
                            .unwrap_or(false)
                    });
                if modified_server_config
                    || mutable(relative, server)
                    || (previous.is_none() && old.is_some())
                {
                    if let Some(previous) = previous {
                        next.files.insert(relative.clone(), previous.clone());
                    }
                    continue;
                }
                if previous.is_none_or(|previous| {
                    !Checksum::GitBlob(previous.sha.clone())
                        .verify(&target)
                        .unwrap_or(false)
                }) {
                    storage::atomic_copy(&target, &storage::safe_join(&backup, relative)?)?;
                }
            }
            downloads.push(self.download(desired, relative, file)?);
            changes.push((relative.clone(), Some(self.blob_path(file))));
            next.files.insert(relative.clone(), file.clone());
        }
        if let Some(old) = old {
            for (relative, file) in &old.files {
                if desired.files.contains_key(relative) || mutable(relative, server) {
                    continue;
                }
                let target = storage::safe_join(root, relative)?;
                if target.exists() {
                    if server
                        && ["config/", "defaultconfigs/"]
                            .iter()
                            .any(|prefix| relative.starts_with(prefix))
                        && !Checksum::GitBlob(file.sha.clone()).verify(&target)?
                    {
                        continue;
                    }
                    if !Checksum::GitBlob(file.sha.clone()).verify(&target)? {
                        storage::atomic_copy(&target, &storage::safe_join(&backup, relative)?)?;
                    }
                    changes.push((relative.clone(), None));
                }
            }
        }
        self.net.many(downloads, job).await?;
        job.check()?;
        job.progress("pack", "Applying verified files", 0, changes.len() as u64);
        let state = self.layout.state("pending-manifest.json");
        storage::write_json(&state, &next)?;
        changes.push((".auram-manifest.json".into(), Some(state)));
        // Cancellation is deferred during the short journaled apply/rollback phase.
        storage::transact(root, transaction, &changes)?;
        job.progress("pack", format!("Pack {} ready", desired.tag.name), 1, 1);
        Ok(next)
    }
}
fn version(raw: &str) -> Option<semver::Version> {
    let raw = raw.trim_start_matches('v');
    let mut components = raw.split('-').next()?.split('.').count();
    let mut value = raw.to_owned();
    while components < 3 {
        let at = value.find('-').unwrap_or(value.len());
        value.insert_str(at, ".0");
        components += 1;
    }
    semver::Version::parse(&value).ok()
}
fn runtime_or_userdata(path: &str) -> bool {
    let path = path.strip_prefix("minecraft/").unwrap_or("");
    [
        "assets/",
        "libraries/",
        "versions/",
        "natives/",
        "saves/",
        "screenshots/",
        "backups/",
        "logs/",
        "crash-reports/",
        "cache/",
        ".mixin.out/",
        "usercache.json",
        "usernamecache.json",
        "journeymap/data/",
        "Distant_Horizons_server_data/",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
}
pub fn mutable(path: &str, server: bool) -> bool {
    let path = path.strip_prefix("minecraft/").unwrap_or(path);
    if server {
        return [
            "world/",
            "world_nether/",
            "world_the_end/",
            "server.properties",
            "ops.json",
            "whitelist.json",
            "banned-ips.json",
            "banned-players.json",
            "eula.txt",
            "user_jvm_args.txt",
        ]
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(prefix));
    }
    [
        "options.txt",
        "servers.dat",
        "servers.dat_old",
        "shaderpacks/",
        "resourcepacks/",
        "journeymap/data/",
        "config/jei/jei-client.ini",
        "config/embeddium-options.json",
    ]
    .iter()
    .any(|prefix| path == *prefix || path.starts_with(prefix))
        || runtime_or_userdata(&format!("minecraft/{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha1::Digest;
    fn file(bytes: &[u8]) -> PackFile {
        let mut hash = sha1::Sha1::new();
        hash.update(format!("blob {}\0", bytes.len()));
        hash.update(bytes);
        PackFile {
            sha: hex::encode(hash.finalize()),
            size: bytes.len() as u64,
            source: None,
        }
    }
    #[test]
    fn version_sort_is_semantic() {
        assert!(version("1.0.10") > version("1.0.9"));
        assert!(version("v1.2") > version("1.1.99"));
        assert!(version("1.0.0") > version("1.0.0-beta"));
    }
    #[tokio::test]
    async fn update_reuses_blobs_and_preserves_worlds_and_unknown_files() {
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout::new(temp.path().into()).unwrap();
        let packs = Packs {
            net: Downloader::new(&layout).unwrap(),
            layout: layout.clone(),
        };
        let old_file = file(b"old");
        let new_file = file(b"new");
        storage::atomic_write(&packs.blob_path(&new_file), b"new").unwrap();
        storage::atomic_write(&layout.client().join("minecraft/mods/a.jar"), b"old").unwrap();
        storage::atomic_write(&layout.game().join("saves/world/level.dat"), b"world").unwrap();
        storage::atomic_write(&layout.game().join("mods/custom.jar"), b"custom").unwrap();
        storage::atomic_write(&layout.runtime().join("assets/objects/unchanged"), b"asset")
            .unwrap();
        let old = Manifest {
            schema: 1,
            tag: PackTag {
                name: "1".into(),
                sha: "one".into(),
            },
            files: BTreeMap::from([("minecraft/mods/a.jar".into(), old_file)]),
        };
        let desired = Manifest {
            schema: 1,
            tag: PackTag {
                name: "2".into(),
                sha: "two".into(),
            },
            files: BTreeMap::from([("minecraft/mods/a.jar".into(), new_file)]),
        };
        packs
            .reconcile(
                &layout.client(),
                &layout.state("tx"),
                Some(&old),
                &desired,
                &Job::default(),
                false,
            )
            .await
            .unwrap();
        assert_eq!(fs::read(layout.game().join("mods/a.jar")).unwrap(), b"new");
        assert!(layout.game().join("saves/world/level.dat").exists());
        assert!(layout.game().join("mods/custom.jar").exists());
        assert_eq!(
            fs::read(layout.runtime().join("assets/objects/unchanged")).unwrap(),
            b"asset"
        );
    }
    #[tokio::test]
    async fn server_updates_preserve_locally_modified_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout::new(temp.path().into()).unwrap();
        let packs = Packs {
            net: Downloader::new(&layout).unwrap(),
            layout: layout.clone(),
        };
        let original = file(b"original");
        let updated = file(b"pack-update");
        storage::atomic_write(&packs.blob_path(&updated), b"pack-update").unwrap();
        storage::atomic_write(
            &layout.server().join("config/settings.toml"),
            b"my-settings",
        )
        .unwrap();
        let old = Manifest {
            schema: 1,
            tag: PackTag {
                name: "old".into(),
                sha: "old".into(),
            },
            files: BTreeMap::from([("config/settings.toml".into(), original)]),
        };
        let desired = Manifest {
            schema: 1,
            tag: PackTag {
                name: "new".into(),
                sha: "new".into(),
            },
            files: BTreeMap::from([("config/settings.toml".into(), updated)]),
        };
        packs
            .reconcile(
                &layout.server(),
                &layout.state("server-tx"),
                Some(&old),
                &desired,
                &Job::default(),
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            fs::read(layout.server().join("config/settings.toml")).unwrap(),
            b"my-settings"
        );
    }
}
