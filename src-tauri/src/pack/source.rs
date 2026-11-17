use super::{version, Manifest, PackFile, PackTag, Packs};
use crate::{jobs::Job, storage};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
use unicode_normalization::UnicodeNormalization;

pub(super) const CLIENT_POLICY: u32 = 1;

#[derive(Clone)]
pub struct Repository {
    api: String,
    raw: String,
}
impl Default for Repository {
    fn default() -> Self {
        Self {
            api: "https://api.github.com/repos/AuramMods/Auram".into(),
            raw: "https://raw.githubusercontent.com/AuramMods/Auram/".into(),
        }
    }
}
impl Repository {
    pub(super) fn raw_url(&self, revision: &str, path: &str) -> Result<url::Url> {
        repo_path(path)?;
        let mut url = url::Url::parse(&self.raw)?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid repository URL"))?
            .pop_if_empty()
            .push(revision)
            .extend(path.split('/'));
        Ok(url)
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Tree {
    tree: Vec<Entry>,
    #[serde(default)]
    truncated: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    path: String,
    sha: String,
    #[serde(rename = "type")]
    kind: String,
    mode: String,
    size: Option<u64>,
}
impl Entry {
    fn file(self) -> Result<PackFile> {
        oid(&self.sha)?;
        if self.kind != "blob" || !matches!(self.mode.as_str(), "100644" | "100755") {
            bail!(
                "Unsupported Git entry {} ({} / {})",
                self.path,
                self.kind,
                self.mode
            );
        }
        Ok(PackFile {
            sha: self.sha,
            size: self.size.context("Git file size missing")?,
            mode: self.mode,
            source: None,
        })
    }
}
#[derive(Deserialize)]
struct GitObject {
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}
#[derive(Deserialize)]
struct Ref {
    object: GitObject,
}
#[derive(Deserialize)]
struct Release {
    version: u64,
    changelist: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Changelist {
    schema: u32,
    version: u64,
    previous_version: Option<String>,
    previous_commit: Option<String>,
    content_commit: String,
    changes: Vec<String>,
}
impl Changelist {
    fn plan(&self, release: &Release) -> Result<BTreeMap<String, char>> {
        if self.schema != 1 || self.version != release.version {
            bail!("Unsupported or inconsistent pack changelist");
        }
        oid(&self.content_commit)?;
        if self.previous_version.is_some() != self.previous_commit.is_some() {
            bail!("Changelist has an incomplete previous release identity");
        }
        let mut changes = BTreeMap::new();
        for entry in &self.changes {
            let operation = entry.chars().next().context("Empty changelist operation")?;
            if !matches!(operation, '+' | '*' | '-') {
                bail!("Unknown changelist operation");
            }
            let path = &entry[1..]; // ASCII operation; preserve every path byte after it.
            repo_path(path)?;
            if changes.insert(path.into(), operation).is_some() {
                bail!("Duplicate changelist path: {path}");
            }
        }
        Ok(changes)
    }
}

pub(super) enum Selection {
    Client(ignore::gitignore::Gitignore),
    Server,
}
impl Selection {
    pub(super) fn includes(&self, path: &str, directory: bool) -> bool {
        let selected = if directory {
            path == "minecraft"
                || path.starts_with("minecraft/")
                || matches!(self, Self::Server)
                    && (path == "server_files" || path.starts_with("server_files/"))
        } else {
            ["mmc-pack.json", "instance.cfg", "icon-256.png"].contains(&path)
                || path.starts_with("minecraft/")
                || matches!(self, Self::Server)
                    && (path == "server_config.conf" || path.starts_with("server_files/"))
        };
        if !selected
            || super::runtime_or_userdata(&format!("{}{}", path, if directory { "/" } else { "" }))
        {
            return false;
        }
        !matches!(self, Self::Client(ignore) if ignore.matched_path_or_any_parents(path,directory).is_ignore())
    }
}

pub(super) struct Source<'a> {
    packs: &'a Packs,
}
impl<'a> Source<'a> {
    pub(super) fn new(packs: &'a Packs) -> Self {
        Self { packs }
    }
    async fn api<T: serde::de::DeserializeOwned>(&self, path: &str, job: &Job) -> Result<T> {
        self.json(&format!("{}{path}", self.packs.repository.api), job)
            .await
    }
    async fn json<T: serde::de::DeserializeOwned>(&self, url: &str, job: &Job) -> Result<T> {
        job.check()?;
        tokio::select! {
            _=job.cancel.cancelled()=>bail!("Operation cancelled"),
            result=self.packs.net.json(url)=>result,
        }
    }
    pub(super) async fn resolve(&self, name: &str, job: &Job) -> Result<PackTag> {
        if version(name).is_none() {
            bail!("Invalid pack release tag: {name}");
        }
        let reference: Ref = self.api(&format!("/git/ref/tags/{name}"), job).await?;
        let mut object = reference.object;
        let mut seen = BTreeSet::new();
        loop {
            oid(&object.sha)?;
            if !seen.insert(object.sha.clone()) || seen.len() > 32 {
                bail!("Cyclic Git tag objects");
            }
            match object.kind.as_str() {
                "commit" => {
                    return Ok(PackTag {
                        name: name.into(),
                        sha: object.sha,
                    })
                }
                "tag" => {
                    let tag: Ref = self.api(&format!("/git/tags/{}", object.sha), job).await?;
                    object = tag.object;
                }
                _ => bail!("Release tag does not identify a commit"),
            }
        }
    }
    pub(super) async fn discover(&self, job: &Job) -> Result<PackTag> {
        job.progress("pack", "Checking the published pack build", 0, 0);
        let hint: Result<Release> = self
            .json(
                self.packs
                    .repository
                    .raw_url("main", "auram.json")?
                    .as_str(),
                job,
            )
            .await;
        match hint {
            Ok(hint) => {
                let tag = self.resolve(&hint.version.to_string(), job).await?;
                let pinned = self
                    .release(&tag, job)
                    .await?
                    .context("Published pack metadata missing")?;
                if pinned.version != hint.version {
                    bail!("Published pack version disagrees with its tag");
                }
                return Ok(tag);
            }
            Err(error)
                if error
                    .downcast_ref::<reqwest::Error>()
                    .and_then(|e| e.status())
                    == Some(reqwest::StatusCode::NOT_FOUND) => {}
            Err(error) => return Err(error),
        }
        // Compatibility with releases predating auram.json; do not require GitHub Release objects.
        #[derive(Deserialize)]
        struct Tag {
            name: String,
        }
        let mut tags = Vec::<Tag>::new();
        for page in 1.. {
            let batch: Vec<Tag> = self
                .api(&format!("/tags?per_page=100&page={page}"), job)
                .await?;
            let done = batch.len() < 100;
            tags.extend(batch);
            if done {
                break;
            }
        }
        tags.retain(|t| version(&t.name).is_some());
        tags.sort_by(|a, b| version(&b.name).cmp(&version(&a.name)));
        self.resolve(
            &tags
                .first()
                .context("The pack repository has no version tags")?
                .name,
            job,
        )
        .await
    }
    async fn tree(&self, sha: &str, recursive: bool, job: &Job) -> Result<Tree> {
        oid(sha)?;
        job.check()?;
        let cache = self.packs.layout.state(&format!(
            "git-trees/{sha}-{}.json",
            if recursive { "recursive" } else { "direct" }
        ));
        if let Some(tree) = storage::read_optional::<Tree>(&cache).ok().flatten() {
            if (recursive || !tree.truncated)
                && tree
                    .tree
                    .iter()
                    .all(|entry| repo_path(&entry.path).is_ok() && oid(&entry.sha).is_ok())
            {
                return Ok(tree);
            }
        }
        let tree: Tree = self
            .api(
                &format!(
                    "/git/trees/{sha}{}",
                    if recursive { "?recursive=1" } else { "" }
                ),
                job,
            )
            .await?;
        if !recursive && tree.truncated {
            bail!("GitHub returned an incomplete pack directory");
        }
        for entry in &tree.tree {
            repo_path(&entry.path)?;
            oid(&entry.sha)?;
        }
        storage::write_json(&cache, &tree)?;
        Ok(tree)
    }
    async fn lookup_entry(&self, tag: &PackTag, path: &str, job: &Job) -> Result<Option<Entry>> {
        repo_path(path)?;
        let parts = path.split('/').collect::<Vec<_>>();
        let mut sha = tag.sha.clone();
        for (index, part) in parts.iter().enumerate() {
            let tree = self.tree(&sha, false, job).await?;
            let Some(entry) = tree.tree.into_iter().find(|e| e.path == *part) else {
                return Ok(None);
            };
            if index + 1 == parts.len() {
                return Ok(Some(entry));
            }
            if entry.kind != "tree" {
                return Ok(None);
            }
            sha = entry.sha;
        }
        Ok(None)
    }
    async fn lookup(&self, tag: &PackTag, path: &str, job: &Job) -> Result<Option<PackFile>> {
        self.lookup_entry(tag, path, job)
            .await?
            .map(Entry::file)
            .transpose()
    }
    async fn lookup_files(
        &self,
        tag: &PackTag,
        paths: Vec<String>,
        job: &Job,
    ) -> Result<BTreeMap<String, PackFile>> {
        let mut groups = BTreeMap::<String, Vec<String>>::new();
        for path in paths {
            let parts = path.split('/').collect::<Vec<_>>();
            let prefix = if parts.len() > 2 {
                format!("{}/{}", parts[0], parts[1])
            } else {
                String::new()
            };
            groups.entry(prefix).or_default().push(path);
        }
        let mut result = BTreeMap::new();
        for (prefix, paths) in groups {
            if paths.len() > 2 && !prefix.is_empty() {
                if let Some(parent) = self
                    .lookup_entry(tag, &prefix, job)
                    .await?
                    .filter(|e| e.kind == "tree")
                {
                    let tree = self.tree(&parent.sha, true, job).await?;
                    if !tree.truncated {
                        let mut entries = tree
                            .tree
                            .into_iter()
                            .map(|e| (format!("{prefix}/{}", e.path), e))
                            .collect::<BTreeMap<_, _>>();
                        for path in paths {
                            let file = entries
                                .remove(&path)
                                .with_context(|| format!("Target release is missing {path}"))?
                                .file()?;
                            result.insert(path, file);
                        }
                        continue;
                    }
                }
            }
            for path in paths {
                let file = self
                    .lookup(tag, &path, job)
                    .await?
                    .with_context(|| format!("Target release is missing {path}"))?;
                result.insert(path, file);
            }
        }
        Ok(result)
    }
    async fn bytes(&self, tag: &PackTag, path: &str, job: &Job) -> Result<Option<Vec<u8>>> {
        let Some(file) = self.lookup(tag, path, job).await? else {
            return Ok(None);
        };
        if file.size > 8 * 1024 * 1024 {
            bail!("Pack metadata is too large: {path}");
        }
        let manifest = Manifest {
            schema: 1,
            policy: 0,
            tag: tag.clone(),
            files: BTreeMap::from([(path.into(), file)]),
        };
        Ok(Some(fs::read(
            self.packs.file(&manifest, path, job).await?,
        )?))
    }
    async fn release(&self, tag: &PackTag, job: &Job) -> Result<Option<Release>> {
        let Some(bytes) = self.bytes(tag, "auram.json", job).await? else {
            return Ok(None);
        };
        let release: Release = serde_json::from_slice(&bytes)?;
        if release.version.to_string() != tag.name {
            bail!("Pack tag and auram.json disagree");
        }
        repo_path(&release.changelist)?;
        Ok(Some(release))
    }
    pub(super) async fn selection(&self, tag: &PackTag, job: &Job) -> Result<Selection> {
        let mut builder = ignore::gitignore::GitignoreBuilder::new("");
        if let Some(bytes) = self.bytes(tag, ".packignore", job).await? {
            for line in std::str::from_utf8(&bytes)?.lines() {
                builder.add_line(None, line)?;
            }
        }
        Ok(Selection::Client(builder.build()?))
    }
    pub(super) async fn inventory(
        &self,
        tag: PackTag,
        selection: &Selection,
        job: &Job,
    ) -> Result<Manifest> {
        let mut files = BTreeMap::new();
        // Read root and minecraft's immediate children before recursive requests, pruning
        // bundled runtimes, worlds, tooling and server payload outside this profile.
        let mut queue = vec![(String::new(), tag.sha.clone(), false)];
        while let Some((prefix, sha, recursive)) = queue.pop() {
            let tree = self.tree(&sha, recursive, job).await?;
            if tree.truncated {
                queue.push((prefix, sha, false));
                continue;
            }
            for entry in tree.tree {
                let path = format!("{prefix}{}", entry.path);
                let directory = entry.kind == "tree";
                if !selection.includes(&path, directory) {
                    continue;
                }
                if directory {
                    if !recursive {
                        queue.push((format!("{path}/"), entry.sha, path != "minecraft"));
                    }
                } else {
                    files.insert(path, entry.file()?);
                }
            }
        }
        let result = Manifest {
            schema: 1,
            policy: if matches!(selection, Selection::Client(_)) {
                CLIENT_POLICY
            } else {
                0
            },
            tag,
            files,
        };
        validate_inventory(&result)?;
        if !result.files.contains_key("mmc-pack.json") {
            bail!("Pack manifest does not contain mmc-pack.json");
        }
        Ok(result)
    }
    fn snapshot(&self, sha: &str) -> std::path::PathBuf {
        self.packs
            .layout
            .state(&format!("pack-client-{CLIENT_POLICY}-{sha}.json"))
    }
    pub(super) async fn client_target(
        &self,
        tag: PackTag,
        installed: Option<&Manifest>,
        repair: bool,
        job: &Job,
    ) -> Result<(Manifest, Option<BTreeSet<String>>)> {
        let selection = self.selection(&tag, job).await?;
        if !repair {
            if let Some(base) = installed.filter(|m| m.policy == CLIENT_POLICY) {
                if let Some(expected) =
                    storage::read_optional::<Manifest>(&self.snapshot(&base.tag.sha))
                        .ok()
                        .flatten()
                        .filter(|m| m.tag.sha == base.tag.sha)
                {
                    let delta = self.delta(&expected, &tag, &selection, job).await;
                    match delta {
                        Ok((manifest, paths, count)) => {
                            job.progress("pack",format!("Updating {} → {} using {count} changelist(s); {} selected paths",base.tag.name,tag.name,paths.len()),0,paths.len() as u64);
                            storage::write_json(&self.snapshot(&tag.sha), &manifest)?;
                            return Ok((manifest, Some(paths)));
                        }
                        Err(error) => {
                            job.check()?;
                            job.progress("pack",format!("Changelist unavailable ({error}); reconciling the selected target files"),0,0);
                        }
                    }
                }
            }
        }
        job.progress(
            "pack",
            if repair {
                "Reading complete selected inventory for repair"
            } else {
                "Reading sparse pack inventory"
            },
            0,
            0,
        );
        let manifest = self.inventory(tag, &selection, job).await?;
        storage::write_json(&self.snapshot(&manifest.tag.sha), &manifest)?;
        Ok((manifest, None))
    }
    pub(super) async fn delta(
        &self,
        base: &Manifest,
        target: &PackTag,
        selection: &Selection,
        job: &Job,
    ) -> Result<(Manifest, BTreeSet<String>, usize)> {
        let policy = if matches!(selection, Selection::Client(_)) {
            CLIENT_POLICY
        } else {
            0
        };
        if base.schema != 1 || base.policy != policy {
            bail!("Installation policy changed");
        }
        validate_inventory(base)?;
        let mut current = target.clone();
        let mut seen = BTreeSet::new();
        let mut plan = BTreeMap::new();
        while current.sha != base.tag.sha {
            if !seen.insert(current.sha.clone()) || seen.len() > 8 {
                bail!("Changelist chain cycles or is too long");
            }
            let release = self
                .release(&current, job)
                .await?
                .context("Release has no changelist metadata")?;
            let bytes = self
                .bytes(&current, &release.changelist, job)
                .await?
                .context("Release changelist is missing")?;
            let list: Changelist = serde_json::from_slice(&bytes)?;
            let changes = list.plan(&release)?;
            if changes.contains_key(".packignore") {
                bail!("Pack installation filter changed");
            }
            #[derive(Deserialize)]
            struct Parent {
                sha: String,
            }
            #[derive(Deserialize)]
            struct Commit {
                parents: Vec<Parent>,
            }
            let commit: Commit = self
                .api(&format!("/git/commits/{}", current.sha), job)
                .await?;
            if commit.parents.len() != 1
                || commit.parents.first().map(|p| p.sha.as_str())
                    != Some(list.content_commit.as_str())
            {
                bail!("Changelist provenance disagrees with the release commit");
            }
            for (path, operation) in changes {
                plan.entry(path).or_insert(operation);
            }
            let previous = list
                .previous_version
                .context("Changelist chain does not reach the installed commit")?;
            let previous_commit = list.previous_commit.context("Previous commit missing")?;
            oid(&previous_commit)?;
            current = self.resolve(&previous, job).await?;
            if current.sha != previous_commit {
                bail!("Previous release tag was retargeted or the chain is inconsistent");
            }
        }
        let mut result = base.clone();
        result.tag = target.clone();
        let mut touched = BTreeSet::new();
        let mut upserts = Vec::new();
        for (path, operation) in plan {
            if !selection.includes(&path, false) {
                continue;
            }
            touched.insert(path.clone());
            if operation == '-' {
                result.files.remove(&path);
            } else {
                upserts.push(path);
            }
        }
        result
            .files
            .extend(self.lookup_files(target, upserts, job).await?);
        validate_inventory(&result)?;
        Ok((result, touched, seen.len()))
    }
}

fn oid(value: &str) -> Result<()> {
    if value.len() != 40 || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("Invalid Git object ID");
    }
    Ok(())
}
fn repo_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains(['\0', '\\'])
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        bail!("Unsafe repository path: {path:?}");
    }
    Ok(())
}
pub(super) fn validate_inventory(manifest: &Manifest) -> Result<()> {
    let mut names = BTreeSet::new();
    for (path, file) in &manifest.files {
        oid(&file.sha)?;
        storage::safe_join(Path::new(""), path)?;
        if !matches!(file.mode.as_str(), "100644" | "100755") {
            bail!("Unsupported Git file mode at {path}");
        }
        let key = normalized_path(path);
        if !names.insert(key) {
            bail!("Case-insensitive destination collision: {path}");
        }
    }
    for name in &names {
        for (index, _) in name.match_indices('/') {
            if names.contains(&name[..index]) {
                bail!("File/directory destination collision: {name}");
            }
        }
    }
    Ok(())
}
pub(super) fn normalized_path(path: &str) -> String {
    path.nfd().flat_map(char::to_lowercase).collect()
}
pub(super) fn mode_matches(path: &Path, mode: &str) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok((fs::metadata(path)?.permissions().mode() & 0o111 != 0) == (mode == "100755"))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(true)
    }
}
pub(super) fn set_mode(path: &Path, mode: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if mode == "100755" { 0o755 } else { 0o644 }),
        )?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
