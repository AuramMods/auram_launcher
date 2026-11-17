use crate::{
    download::{Checksum, Download, Downloader},
    jobs::Job,
    storage::{self, Layout},
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
mod source;
pub use source::Repository;
use source::{Selection, Source, CLIENT_POLICY};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackTag {
    pub name: String,
    pub sha: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackFile {
    pub sha: String,
    pub size: u64,
    #[serde(default = "regular_mode")]
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    #[serde(default)]
    pub policy: u32,
    pub tag: PackTag,
    pub files: BTreeMap<String, PackFile>,
}
fn regular_mode() -> String {
    "100644".into()
}

#[derive(Clone)]
pub struct Packs {
    pub layout: Layout,
    pub net: Downloader,
    repository: Repository,
}
impl Packs {
    pub async fn server_changelists(
        &self,
        version: &str,
        commit: &str,
        target: &PackTag,
        job: &Job,
    ) -> Result<(Manifest, BTreeSet<String>, usize)> {
        let tag = self.source().resolve(version, job).await?;
        if !commit.is_empty() && commit != tag.sha {
            bail!("Remote version commit disagrees with its immutable pack tag");
        }
        let base = self.manifest(tag, job).await?;
        let (_, changes, count) = self
            .source()
            .delta(&base, target, &Selection::Server, job)
            .await?;
        // Translate the old distribution through its own server inclusion rules.
        // This also makes removed/remapped server paths available to the SFTP
        // planner when the remote only has auram.json, without reading mod jars.
        let config: crate::server::ServerConfig = serde_json::from_slice(
            &tokio::fs::read(self.file(&base, "server_config.conf", job).await?).await?,
        )
        .context("Read the previous pack's server configuration")?;
        Ok((config.manifest(&base)?, changes, count))
    }
    pub fn new(layout: Layout, net: Downloader) -> Self {
        Self {
            layout,
            net,
            repository: Repository::default(),
        }
    }
    fn source(&self) -> Source<'_> {
        Source::new(self)
    }
    pub fn installed(&self) -> Result<Option<Manifest>> {
        if self.layout.client_reset_pending()? {
            return Ok(None);
        }
        storage::read_optional(&self.layout.client().join(".auram-manifest.json"))
    }
    pub async fn latest(&self) -> Result<PackTag> {
        self.source().discover(&Job::default()).await
    }
    pub async fn manifest(&self, tag: PackTag, job: &Job) -> Result<Manifest> {
        let cache = self
            .layout
            .state(&format!("pack-server-source-1-{}.json", tag.sha));
        if let Some(value) = storage::read_optional::<Manifest>(&cache).ok().flatten() {
            if value.tag.sha == tag.sha && source::validate_inventory(&value).is_ok() {
                return Ok(value);
            }
        }
        job.progress("pack", "Reading pack file manifest", 0, 0);
        let manifest = self
            .source()
            .inventory(tag, &Selection::Server, job)
            .await?;
        storage::write_json(&cache, &manifest)?;
        Ok(manifest)
    }
    pub fn blob_path(&self, file: &PackFile) -> PathBuf {
        self.layout.root.join("cache/blobs").join(&file.sha)
    }
    pub fn download(&self, manifest: &Manifest, path: &str, file: &PackFile) -> Result<Download> {
        let url = self
            .repository
            .raw_url(&manifest.tag.sha, file.source.as_deref().unwrap_or(path))?;
        Ok(Download {
            label: Some(file.source.as_deref().unwrap_or(path).into()),
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
        if !Downloader::valid(&item).await? {
            self.reuse_owned(
                &self.layout.client(),
                self.installed()?.as_ref(),
                &item,
                job,
            )?;
        }
        self.net.get(&item, job).await?;
        Ok(item.target)
    }
    fn reuse_owned(
        &self,
        root: &Path,
        old: Option<&Manifest>,
        download: &Download,
        job: &Job,
    ) -> Result<()> {
        let Checksum::GitBlob(sha) = &download.checksum else {
            return Ok(());
        };
        if let Some(old) = old {
            for (path, previous) in &old.files {
                job.check()?;
                if previous.sha == *sha {
                    let existing = storage::safe_join(root, path)?;
                    if existing.is_file() && download.checksum.verify(&existing)? {
                        storage::atomic_copy(&existing, &download.target)?;
                        break;
                    }
                }
            }
        }
        Ok(())
    }
    pub async fn client_manifest(&self, manifest: &Manifest, job: &Job) -> Result<Manifest> {
        let selection = self.source().selection(&manifest.tag, job).await?;
        let mut result = manifest.clone();
        result.policy = CLIENT_POLICY;
        result
            .files
            .retain(|path, _| selection.includes(path, false));
        if !result.files.contains_key("mmc-pack.json") {
            bail!("Pack manifest does not contain mmc-pack.json");
        }
        Ok(result)
    }
    pub async fn update(&self, repair: bool, job: &Job) -> Result<Manifest> {
        let layout = self.layout.clone();
        tokio::task::spawn_blocking(move || layout.resume_client_reset()).await??;
        job.phase("pack-plan", "Read pack", false);
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
            match self.source().discover(job).await {
                Ok(tag) => tag,
                Err(error) => {
                    job.check()?;
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
        let (desired, changed) = self
            .source()
            .client_target(tag, old.as_ref(), repair, job)
            .await?;
        self.reconcile_plan(
            &self.layout.client(),
            &self.layout.state("client-transaction"),
            old.as_ref(),
            &desired,
            job,
            false,
            changed.as_ref(),
        )
        .await
    }
    pub async fn force_reinstall(&self, job: &Job) -> Result<Manifest> {
        job.check()?;
        job.phase("client-reset", "Delete client pack", false);
        job.progress(
            "client-reset",
            "Deleting the entire client pack, including local worlds and custom files",
            0,
            0,
        );
        let layout = self.layout.clone();
        // Cancellation is deferred once the persistent deletion intent is written.
        tokio::task::spawn_blocking(move || -> Result<()> {
            layout.begin_client_reset()?;
            layout.resume_client_reset()
        })
        .await??;
        job.check()?;
        self.update(false, job).await
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
        self.reconcile_plan(root, transaction, old, desired, job, server, None)
            .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn reconcile_plan(
        &self,
        root: &Path,
        transaction: &Path,
        old: Option<&Manifest>,
        desired: &Manifest,
        job: &Job,
        server: bool,
        touched: Option<&BTreeSet<String>>,
    ) -> Result<Manifest> {
        source::validate_inventory(desired)?;
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
        for (index, (relative, file)) in desired.files.iter().enumerate() {
            job.check()?;
            job.progress(
                "pack",
                if server {
                    "Inspecting managed server files"
                } else {
                    "Inspecting managed pack files"
                },
                index as u64,
                desired.files.len() as u64,
            );
            let target = storage::safe_join(root, relative)?;
            let previous = old.and_then(|m| m.files.get(relative)).or_else(|| {
                // On a case-insensitive filesystem a case-only rename is still owned.
                old?.files.iter().find_map(|(path, file)| {
                    if path != relative
                        && source::normalized_path(path) == source::normalized_path(relative)
                        && storage::safe_join(root, path).ok().is_some_and(|owned| {
                            same_file::is_same_file(&target, owned).unwrap_or(false)
                        })
                    {
                        Some(file)
                    } else {
                        None
                    }
                })
            });
            if touched.is_some_and(|paths| !paths.contains(relative)) {
                if let Some(previous) = previous {
                    next.files.insert(relative.clone(), previous.clone());
                }
                continue;
            }
            let replaces_directory = target.is_dir()
                && old.is_some_and(|m| {
                    m.files
                        .keys()
                        .any(|path| path.starts_with(&format!("{relative}/")))
                });
            if target.exists() && !replaces_directory {
                let valid = Checksum::GitBlob(file.sha.clone()).verify(&target)?;
                let case_rename =
                    previous.is_some() && old.is_some_and(|m| !m.files.contains_key(relative));
                if valid && source::mode_matches(&target, &file.mode)? && !case_rename {
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
            let download = self.download(desired, relative, file)?;
            // A rename can reuse a verified old managed file even after the blob cache was evicted.
            if !Downloader::valid(&download).await? {
                self.reuse_owned(root, old, &download, job)?;
            }
            downloads.push(download);
            changes.push((relative.clone(), Some(self.blob_path(file))));
            next.files.insert(relative.clone(), file.clone());
        }
        if let Some(old) = old {
            for (relative, file) in &old.files {
                if desired.files.contains_key(relative) || mutable(relative, server) {
                    continue;
                }
                if touched.is_some_and(|paths| !paths.contains(relative)) {
                    next.files.insert(relative.clone(), file.clone());
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
        job.phase(
            if server { "server-files" } else { "pack-files" },
            if server { "Server files" } else { "Pack files" },
            true,
        );
        self.net
            .many_named(
                downloads,
                job,
                if server { "Server files" } else { "Pack files" },
            )
            .await?;
        job.phase(
            if server { "server-apply" } else { "pack-apply" },
            "Apply files",
            false,
        );
        job.progress("pack", "Staging verified files", 0, changes.len() as u64);
        // Apply executable bits to private staging copies, keeping cached blobs immutable.
        let staging = self.layout.state("pack-apply-staging");
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        for (path, source) in &mut changes {
            job.check()?;
            if let Some(blob) = source {
                if let Some(file) = desired.files.get(path) {
                    let staged = storage::safe_join(&staging, path)?;
                    storage::atomic_copy(blob, &staged)?;
                    source::set_mode(&staged, &file.mode)?;
                    *blob = staged;
                }
            }
        }
        job.check()?;
        job.progress("pack", "Applying verified files", 0, changes.len() as u64);
        let state = self.layout.state("pending-manifest.json");
        storage::write_json(&state, &next)?;
        changes.push((".auram-manifest.json".into(), Some(state)));
        // Cancellation is deferred during the short journaled apply/rollback phase.
        storage::transact(root, transaction, &changes)?;
        job.progress("pack", format!("Pack {} ready", desired.tag.name), 1, 1);
        job.complete_phase();
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
        "lib/",
        "data/",
        "mod_data/",
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
            mode: regular_mode(),
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
        let packs = Packs::new(layout.clone(), Downloader::new(&layout).unwrap());
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
            policy: 0,
            tag: PackTag {
                name: "1".into(),
                sha: "one".into(),
            },
            files: BTreeMap::from([("minecraft/mods/a.jar".into(), old_file)]),
        };
        let desired = Manifest {
            schema: 1,
            policy: 0,
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
        let packs = Packs::new(layout.clone(), Downloader::new(&layout).unwrap());
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
            policy: 0,
            tag: PackTag {
                name: "old".into(),
                sha: "old".into(),
            },
            files: BTreeMap::from([("config/settings.toml".into(), original)]),
        };
        let desired = Manifest {
            schema: 1,
            policy: 0,
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
