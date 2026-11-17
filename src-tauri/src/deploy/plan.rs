use super::connection::{Attrs, Remote};
use super::{canonical, payload_path, safe_path, Baseline, OwnedFile};
use crate::{
    jobs::Job,
    pack::{Manifest, PackFile, Packs},
    server::ServerVersion,
};
use anyhow::{bail, Context, Result};
use futures_util::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionKind {
    Upload,
    Remove,
    Rename,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub kind: ActionKind,
    pub path: String,
    pub from: Option<String>,
    pub file: Option<PackFile>,
    pub payload: Option<String>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentPlan {
    pub remote_version: Option<ServerVersion>,
    pub target_version: ServerVersion,
    pub strategy: String,
    pub changelists: usize,
    pub uploads: usize,
    pub removals: usize,
    pub renames: usize,
    pub reused: usize,
    pub bytes: u64,
    pub recovery_required: bool,
    pub notes: Vec<String>,
    pub actions: Vec<Action>,
    #[serde(skip)]
    pub baseline: Baseline,
    #[serde(skip)]
    pub payload: Manifest,
}
pub async fn optional_json<T: serde::de::DeserializeOwned>(
    remote: &impl Remote,
    path: &str,
) -> Result<Option<T>> {
    if remote.stat(path).await?.is_none() {
        return Ok(None);
    }
    Ok(Some(
        serde_json::from_slice(&remote.read(path, 16 * 1024 * 1024).await?)
            .with_context(|| format!("Read remote {path}"))?,
    ))
}
pub fn validate_baseline(baseline: &Baseline) -> Result<()> {
    if baseline.schema != 1 {
        bail!("Unsupported remote deployment baseline");
    }
    for (key, owned) in &baseline.files {
        safe_path(&owned.path)?;
        if !payload_path(&owned.path)
            || canonical(&owned.path) != *key
            || owned.file.sha.len() != 40
            || !owned.file.sha.bytes().all(|c| c.is_ascii_hexdigit())
        {
            bail!("Invalid remote managed file: {}", owned.path);
        }
    }
    Ok(())
}
pub async fn inventory(
    remote: &impl Remote,
    job: &Job,
) -> Result<BTreeMap<String, (String, Attrs)>> {
    let mut queue = super::ROOTS
        .iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>();
    let mut found = BTreeMap::new();
    while let Some(path) = queue.pop() {
        job.check()?;
        let Some(meta) = remote.stat(&path).await? else {
            continue;
        };
        if meta.symlink || !meta.directory {
            bail!("Remote payload folder is not a regular directory: {path}");
        }
        for (name, attrs) in remote.list(&path).await? {
            if name.contains('/') || name == "." || name == ".." {
                bail!("Invalid SFTP directory entry");
            }
            let path = format!("{path}/{name}");
            safe_path(&path)?;
            if attrs.symlink {
                continue;
            }
            if attrs.directory {
                if path.split('/').count() > 64 {
                    bail!("Remote payload directory nesting is too deep");
                }
                queue.push(path);
            } else if found
                .insert(canonical(&path), (path.clone(), attrs))
                .is_some()
            {
                bail!("Both .jar and .jar.client exist for {path}; remove the duplicate before deploying");
            }
            if found.len() + queue.len() > 100000 {
                bail!("Remote payload inventory is too large");
            }
        }
    }
    Ok(found)
}
pub async fn inspect(packs: &Packs, remote: &impl Remote, job: &Job) -> Result<DeploymentPlan> {
    job.phase("sftp-plan", "Compare server", false);
    let (payload, target_version) = super::built_payload(packs)?;
    let remote_version = super::read_version(remote).await?;
    let recovery_required = remote.stat(".auram-deploy/lock").await?.is_some();
    let mut plan = DeploymentPlan {
        remote_version: remote_version.clone(),
        target_version: target_version.clone(),
        strategy: "full reconciliation".into(),
        changelists: 0,
        uploads: 0,
        removals: 0,
        renames: 0,
        reused: 0,
        bytes: 0,
        recovery_required,
        notes: vec![],
        actions: vec![],
        baseline: Baseline {
            schema: 1,
            version: target_version,
            files: BTreeMap::new(),
        },
        payload,
    };
    if recovery_required {
        plan.notes.push("An interrupted or active deployment owns the remote lock. Recover it before preparing another deployment.".into());
        return Ok(plan);
    }
    let previous = match optional_json::<Baseline>(remote, ".auram-deploy.json").await {
        Ok(value) => value,
        Err(error) => {
            plan.notes.push(format!("Remote ownership metadata could not be used ({error}); untracked files will be retained."));
            None
        }
    };
    let previous = if let Some(baseline) = previous {
        if validate_baseline(&baseline).is_ok()
            && remote_version.as_ref().is_some_and(|v| {
                v.version == baseline.version.version && v.commit == baseline.version.commit
            })
        {
            Some(baseline)
        } else {
            plan.notes.push(
                "Remote version and ownership metadata disagree; unknown files will be preserved."
                    .into(),
            );
            None
        }
    } else {
        None
    };
    let hints = if let Some(version) = &remote_version {
        match packs
            .server_changelists(&version.version, &version.commit, &plan.payload.tag, job)
            .await
        {
            Ok((base, changes, count)) => {
                plan.strategy = "changelists".into();
                plan.changelists = count;
                Some((base, changes))
            }
            Err(error) => {
                job.check()?;
                plan.notes.push(format!(
                    "Changelist fallback: {error}. Comparing the complete selected server payload."
                ));
                None
            }
        }
    } else {
        None
    };
    if remote_version
        .as_ref()
        .is_some_and(|v| !v.forge.is_empty() && v.forge != plan.target_version.forge)
    {
        plan.notes.push(format!("The target requires Forge {}. SFTP deploys pack files only; update the remote Forge runtime separately.",plan.target_version.forge));
    }
    job.progress("sftp", "Inspecting mods, scripts, config and kubejs", 0, 0);
    let files = inventory(remote, job).await?;
    let mut desired = BTreeMap::new();
    for (path, file) in &plan.payload.files {
        if !payload_path(path) {
            continue;
        }
        safe_path(path)?;
        if desired
            .insert(canonical(path), (path.clone(), file.clone()))
            .is_some()
        {
            bail!("Built payload has duplicate enabled/disabled jars");
        }
    }
    let mut consumed = BTreeSet::new();
    let hash_requests = desired
        .iter()
        .filter_map(|(key, (path, file))| {
            if key.starts_with("mods/") && key.ends_with(".jar") {
                return None;
            }
            let (remote_path, _) = files.get(key)?;
            let source = file.source.as_deref().unwrap_or(path);
            let known = previous
                .as_ref()
                .and_then(|b| b.files.get(key))
                .is_some_and(|old| old.file.sha == file.sha)
                || hints.as_ref().is_some_and(|(base, changes)| {
                    !changes.contains(source)
                        && base.files.get(source).is_some_and(|f| f.sha == file.sha)
                });
            (!known).then(|| (remote_path.clone(), remote_path.clone()))
        })
        .collect::<Vec<_>>();
    let hashes=futures_util::stream::iter(hash_requests).map(|(key,path)|async move {
        job.check()?;
        let hash=tokio::select!{_=job.cancel.cancelled()=>bail!("Inspection cancelled"),value=remote.hash(&path)=>value?};
        Ok::<_,anyhow::Error>((key,hash))
    }).buffer_unordered(8).try_collect::<BTreeMap<_,_>>().await?;
    let prior_files = if let Some(previous) = &previous {
        previous
            .files
            .iter()
            .map(|(key, owned)| (key.clone(), owned.file.clone()))
            .collect::<BTreeMap<_, _>>()
    } else if let Some((base, changes)) = &hints {
        base.files
            .iter()
            .filter_map(|(source, file)| {
                let path = source
                    .strip_prefix("minecraft/")
                    .filter(|p| payload_path(p))?;
                if changes.contains(source) {
                    Some((canonical(path), file.clone()))
                } else {
                    None
                }
            })
            .collect()
    } else {
        BTreeMap::new()
    };
    for (index, (key, (path, file))) in desired.iter().enumerate() {
        job.check()?;
        job.progress(
            "sftp",
            "Comparing the selected server payload",
            index as u64,
            desired.len() as u64,
        );
        let existing = files.get(key);
        let destination = existing
            .map(|(p, _)| p.clone())
            .unwrap_or_else(|| path.clone());
        let before = previous.as_ref().and_then(|b| b.files.get(key));
        let source = file.source.as_deref().unwrap_or(path);
        let unchanged_in_chain = hints.as_ref().is_some_and(|(base, changes)| {
            !changes.contains(source) && base.files.get(source).is_some_and(|f| f.sha == file.sha)
        });
        let same_baseline = before.is_some_and(|old| old.file.sha == file.sha);
        let jar = key.starts_with("mods/") && key.ends_with(".jar");
        let matches = if let Some((remote_path, attrs)) = existing {
            if jar {
                let known = before.map(|b| b.file.sha.as_str()).or_else(|| {
                    hints
                        .as_ref()
                        .and_then(|(base, _)| base.files.get(source).map(|f| f.sha.as_str()))
                });
                attrs.size == file.size && known.is_none_or(|hash| hash == file.sha)
            } else if same_baseline || unchanged_in_chain {
                true
            } else {
                hashes
                    .get(remote_path)
                    .is_some_and(|hash| hash == &file.sha)
            }
        } else {
            false
        };
        if matches {
            plan.reused += 1;
        } else {
            let mut rename = None;
            if existing.is_none() {
                for (old_key, old_file) in &prior_files {
                    if !desired.contains_key(old_key)
                        && !consumed.contains(old_key)
                        && old_file.sha == file.sha
                    {
                        if let Some((remote_path, attrs)) = files.get(old_key) {
                            if attrs.size == file.size
                                && ((jar && previous.is_some())
                                    || remote.hash(remote_path).await? == file.sha)
                            {
                                rename = Some((old_key.clone(), remote_path.clone()));
                                break;
                            }
                        }
                    }
                }
            }
            if let Some((old_key, from)) = rename {
                let destination = if from.ends_with(".jar.client") && !path.ends_with(".client") {
                    format!("{path}.client")
                } else {
                    path.clone()
                };
                consumed.insert(old_key);
                plan.actions.push(Action {
                    kind: ActionKind::Rename,
                    path: destination.clone(),
                    from: Some(from),
                    file: Some(file.clone()),
                    payload: Some(path.clone()),
                });
                plan.baseline.files.insert(
                    key.clone(),
                    OwnedFile {
                        path: destination,
                        file: file.clone(),
                    },
                );
                plan.renames += 1;
                continue;
            }
            plan.actions.push(Action {
                kind: ActionKind::Upload,
                path: destination.clone(),
                from: None,
                file: Some(file.clone()),
                payload: Some(path.clone()),
            });
            plan.uploads += 1;
            plan.bytes += file.size;
        }
        plan.baseline.files.insert(
            key.clone(),
            OwnedFile {
                path: destination,
                file: file.clone(),
            },
        );
    }
    let mut obsolete = BTreeMap::new();
    if let Some(previous) = &previous {
        for (key, file) in &previous.files {
            if !desired.contains_key(key) && !consumed.contains(key) {
                obsolete.insert(key.clone(), (file.file.clone(), true));
            }
        }
    }
    // Legacy servers have no ownership file. Changelists may establish deleted
    // distribution paths, but their actual remote bytes must match the old blob.
    if previous.is_none() {
        if let Some((base, changes)) = &hints {
            for source in changes {
                if let Some(path) = source
                    .strip_prefix("minecraft/")
                    .filter(|p| payload_path(p))
                {
                    let key = canonical(path);
                    if !desired.contains_key(&key) && !consumed.contains(&key) {
                        if let Some(file) = base.files.get(source) {
                            obsolete.insert(key, (file.clone(), false));
                        }
                    }
                }
            }
        }
    }
    for (key, (old, owned)) in obsolete {
        if let Some((path, _)) = files.get(&key) {
            let config = !key.starts_with("mods/");
            if (!owned || config) && remote.hash(path).await? != old.sha {
                plan.notes
                    .push(format!("Preserved locally modified obsolete file: {path}"));
                continue;
            }
            plan.actions.push(Action {
                kind: ActionKind::Remove,
                path: path.clone(),
                from: None,
                file: Some(old),
                payload: None,
            });
            plan.removals += 1;
        }
    }
    let untracked = files
        .iter()
        .filter(|(key, _)| {
            key.starts_with("mods/")
                && !desired.contains_key(*key)
                && !plan.actions.iter().any(|action| {
                    &canonical(&action.path) == *key
                        || action.from.as_ref().is_some_and(|p| &canonical(p) == *key)
                })
        })
        .map(|(_, (path, _))| path.clone())
        .collect::<Vec<_>>();
    if !untracked.is_empty() {
        plan.notes.push(format!(
            "Preserving {} untracked mod file(s): {}{}",
            untracked.len(),
            untracked
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            if untracked.len() > 8 { " …" } else { "" }
        ));
    }
    Ok(plan)
}
