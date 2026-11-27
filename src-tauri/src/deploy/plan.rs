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
    ReplaceDirectory,
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
    pub replacements: usize,
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
// List only mod directories needed to preserve enabled/disabled names. Never
// walk config/script trees or fetch remote file bodies during inspection.
async fn mod_entries(
    remote: &impl Remote,
    required: &BTreeSet<String>,
    job: &Job,
) -> Result<BTreeMap<String, (String, Attrs)>> {
    let directories = required
        .iter()
        .filter(|p| p.starts_with("mods/"))
        .map(|p| p.rsplit_once('/').unwrap().0.to_string())
        .collect::<BTreeSet<_>>();
    let lists = futures_util::stream::iter(directories)
        .map(|directory| async move {
            let mut task = job.task(&format!("sftp-list:{directory}"), &directory, None);
            task.state(crate::jobs::TaskState::Running, 0);
            let result = super::connection::wait(
                job,
                &format!("listing {directory}"),
                std::time::Duration::from_secs(30),
                async {
                    match remote.stat(&directory).await? {
                        None => Ok(Vec::new()),
                        Some(meta) if meta.directory && !meta.symlink => {
                            remote.list(&directory).await
                        }
                        Some(_) => {
                            bail!("Remote mod folder is not a regular directory: {directory}")
                        }
                    }
                },
            )
            .await;
            match &result {
                Ok(_) => task.complete(false),
                Err(error) => task.fail(&format!("{error:#}")),
            }
            Ok::<_, anyhow::Error>((directory, result?))
        })
        .buffer_unordered(8)
        .try_collect::<Vec<_>>()
        .await?;
    let mut found = BTreeMap::new();
    for (directory, entries) in lists {
        for (name, attrs) in entries {
            let path = format!("{directory}/{name}");
            safe_path(&path)?;
            let key = canonical(&path);
            if !required.contains(&key) {
                continue;
            }
            if attrs.directory || attrs.symlink {
                bail!("A remote directory or symlink blocks {path}");
            }
            if found.insert(key, (path.clone(), attrs)).is_some() {
                bail!("Both .jar and .jar.client exist for {path}; remove the duplicate before deploying");
            }
        }
    }
    Ok(found)
}
fn desired(payload: &Manifest) -> Result<BTreeMap<String, OwnedFile>> {
    let mut files = BTreeMap::new();
    for (path, file) in &payload.files {
        if !payload_path(path) {
            continue;
        }
        safe_path(path)?;
        if files
            .insert(
                canonical(path),
                OwnedFile {
                    path: path.clone(),
                    file: file.clone(),
                },
            )
            .is_some()
        {
            bail!("Built payload has duplicate enabled/disabled jars");
        }
    }
    Ok(files)
}
async fn replacement(plan: &mut DeploymentPlan, remote: &impl Remote, job: &Job) -> Result<()> {
    let required = plan
        .baseline
        .files
        .keys()
        .filter(|p| p.starts_with("mods/"))
        .cloned()
        .collect();
    let aliases = mod_entries(remote, &required, job).await?;
    for (key, owned) in &mut plan.baseline.files {
        if let Some((path, _)) = aliases.get(key) {
            owned.path = path.clone();
        }
    }
    plan.strategy = "full replacement".into();
    plan.replacements = super::ROOTS.len();
    plan.uploads = plan.baseline.files.len();
    plan.bytes = plan.baseline.files.values().map(|f| f.file.size).sum();
    plan.actions = super::ROOTS
        .iter()
        .map(|root| Action {
            kind: ActionKind::ReplaceDirectory,
            path: (*root).into(),
            from: None,
            file: None,
            payload: None,
        })
        .collect();
    plan.notes.push("Original payload directories will be retained under .auram-deploy/transactions/. No remote payload file bodies were read during inspection.".into());
    Ok(())
}

// The validated release identity is the baseline. Only changed distribution
// paths are examined; local edits on untouched paths do not need a remote read.
async fn patch(
    plan: &mut DeploymentPlan,
    remote: &impl Remote,
    previous: Option<&Baseline>,
    base: Option<&Manifest>,
    job: &Job,
) -> Result<()> {
    let old = if let Some(previous) = previous {
        previous.files.clone()
    } else {
        base.context("Missing deployed pack inventory")?
            .files
            .iter()
            .filter_map(|(source, file)| {
                let path = source.strip_prefix("minecraft/").unwrap_or(source);
                if !payload_path(path) {
                    return None;
                }
                Some((
                    canonical(path),
                    OwnedFile {
                        path: path.into(),
                        file: file.clone(),
                    },
                ))
            })
            .collect()
    };
    let obsolete = old
        .iter()
        .filter(|(key, _)| !plan.baseline.files.contains_key(*key))
        .map(|(key, owned)| (key.clone(), owned.clone()))
        .collect::<BTreeMap<_, _>>();
    let incoming = plan
        .baseline
        .files
        .iter()
        .filter(|(key, target)| {
            old.get(*key)
                .is_none_or(|before| before.file.sha != target.file.sha)
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<BTreeMap<_, _>>();
    plan.reused = plan.baseline.files.len() - incoming.len();
    let required = incoming
        .keys()
        .chain(obsolete.keys())
        .filter(|p| p.starts_with("mods/"))
        .cloned()
        .collect();
    let aliases = mod_entries(remote, &required, job).await?;
    let mut consumed = BTreeSet::new();
    for (index, (key, target)) in incoming.iter().enumerate() {
        job.check()?;
        job.progress(
            "sftp-plan",
            "Planning changed server files from validated changelists",
            index as u64,
            incoming.len() as u64,
        );
        let mut destination = aliases
            .get(key)
            .map(|(p, _)| p.clone())
            .unwrap_or_else(|| target.path.clone());
        if key.starts_with("mods/") && !aliases.contains_key(key) {
            if let Some((old_key, (from, _))) = obsolete
                .iter()
                .filter(|(old_key, owned)| {
                    !consumed.contains(*old_key) && owned.file.sha == target.file.sha
                })
                .filter_map(|(old_key, _)| aliases.get(old_key).map(|entry| (old_key, entry)))
                .find(|(_, (_, attrs))| attrs.size == target.file.size)
            {
                if from.ends_with(".jar.client") && !destination.ends_with(".client") {
                    destination.push_str(".client");
                }
                plan.actions.push(Action {
                    kind: ActionKind::Rename,
                    path: destination.clone(),
                    from: Some(from.clone()),
                    file: Some(target.file.clone()),
                    payload: Some(target.path.clone()),
                });
                plan.baseline.files.get_mut(key).unwrap().path = destination;
                consumed.insert(old_key.clone());
                plan.renames += 1;
                continue;
            }
        }
        plan.actions.push(Action {
            kind: ActionKind::Upload,
            path: destination.clone(),
            from: None,
            file: Some(target.file.clone()),
            payload: Some(target.path.clone()),
        });
        plan.baseline.files.get_mut(key).unwrap().path = destination;
        plan.uploads += 1;
        plan.bytes += target.file.size;
    }
    for (key, owned) in obsolete {
        if consumed.contains(&key) {
            continue;
        }
        let path = if key.starts_with("mods/") {
            let Some((path, _)) = aliases.get(&key) else {
                continue;
            };
            path.clone()
        } else {
            owned.path
        };
        plan.actions.push(Action {
            kind: ActionKind::Remove,
            path,
            from: None,
            file: Some(owned.file),
            payload: None,
        });
        plan.removals += 1;
    }
    Ok(())
}

pub async fn inspect(packs: &Packs, remote: &impl Remote, job: &Job) -> Result<DeploymentPlan> {
    inspect_selected(packs, remote, false, job).await
}

async fn previous_baseline(
    remote: &impl Remote,
    version: &ServerVersion,
    plan: &mut DeploymentPlan,
    job: &Job,
) -> Result<Option<Baseline>> {
    match super::connection::wait(
        job,
        "reading deployment ownership",
        std::time::Duration::from_secs(30),
        optional_json::<Baseline>(remote, ".auram-deploy.json"),
    )
    .await
    {
        Ok(value) => Ok(value.filter(|b| {
            validate_baseline(b).is_ok()
                && b.version.version == version.version
                && (version.commit.is_empty() || b.version.commit == version.commit)
        })),
        Err(error) if error.downcast_ref::<serde_json::Error>().is_some() => {
            plan.notes.push(
                "Invalid ownership metadata ignored; using the released server inventory.".into(),
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub async fn inspect_selected(
    packs: &Packs,
    remote: &impl Remote,
    explicit: bool,
    job: &Job,
) -> Result<DeploymentPlan> {
    job.phase("sftp-plan", "Plan server update", false);
    job.progress("sftp-plan", "Reading the remote Auram version", 0, 0);
    let (payload, target_version) = super::built_payload(packs)?;
    let remote_version = super::connection::wait(
        job,
        "reading auram.json",
        std::time::Duration::from_secs(30),
        super::read_version(remote),
    )
    .await?;
    let recovery_required = super::connection::wait(
        job,
        "checking the deployment journal",
        std::time::Duration::from_secs(30),
        remote.stat(".auram-deploy/lock"),
    )
    .await?
    .is_some();
    let files = desired(&payload)?;
    let mut plan = DeploymentPlan {
        remote_version: remote_version.clone(),
        target_version: target_version.clone(),
        strategy: "changelists".into(),
        changelists: 0,
        uploads: 0,
        removals: 0,
        renames: 0,
        reused: 0,
        replacements: 0,
        bytes: 0,
        recovery_required,
        notes: vec![],
        actions: vec![],
        baseline: Baseline {
            schema: 1,
            version: target_version.clone(),
            files,
        },
        payload,
    };
    if recovery_required {
        plan.notes.push("An interrupted or active deployment owns the remote lock. Recover it before preparing another deployment.".into());
        return Ok(plan);
    }
    if let Some(version) = &remote_version {
        if !version.forge.is_empty() && version.forge != target_version.forge {
            plan.notes.push(format!("The target requires Forge {}. SFTP deploys pack files only; update the remote Forge runtime separately.",target_version.forge));
        }
        if version.version == target_version.version
            && (version.commit.is_empty() || version.commit == target_version.commit)
        {
            plan.reused = plan.baseline.files.len();
            plan.notes.push("The server version already matches the built pack. No remote payload files were read.".into());
            return Ok(plan);
        }
        let current = crate::pack::version(&version.version);
        let target = crate::pack::version(&target_version.version);
        if explicit
            || current
                .zip(target)
                .is_some_and(|(current, target)| current >= target)
        {
            plan.strategy = "versionDiff".into();
            job.progress(
                "sftp-plan",
                "Comparing the deployed and selected pack versions",
                0,
                0,
            );
            let previous = previous_baseline(remote, version, &mut plan, job).await?;
            let base = if previous.is_some() {
                None
            } else {
                Some(
                    super::connection::wait(
                        job,
                        "resolving the deployed version inventory",
                        std::time::Duration::from_secs(180),
                        packs.server_baseline(&version.version, &version.commit, job),
                    )
                    .await?,
                )
            };
            patch(&mut plan, remote, previous.as_ref(), base.as_ref(), job).await?;
            return Ok(plan);
        }
        job.progress(
            "sftp-plan",
            "Resolving the server version and validated changelist route",
            0,
            0,
        );
        match super::connection::wait(
            job,
            "resolving the changelist route",
            std::time::Duration::from_secs(180),
            packs.server_changelists(&version.version, &version.commit, &plan.payload.tag, job),
        )
        .await
        {
            Ok((base, _changes, count)) => {
                plan.changelists = count;
                let previous = previous_baseline(remote, version, &mut plan, job).await?;
                patch(&mut plan, remote, previous.as_ref(), Some(&base), job).await?;
                return Ok(plan);
            }
            Err(error) => {
                job.check()?;
                plan.notes.push(format!("No usable changelist route: {error:#}. Preparing a full replacement of the four payload directories."));
            }
        }
    } else {
        plan.notes.push("No recognized remote auram.json version. Preparing a full replacement of the four payload directories.".into());
    }
    job.progress(
        "sftp-plan",
        "Preparing fresh mods, scripts, config and kubejs directories; no file hashing",
        0,
        0,
    );
    replacement(&mut plan, remote, job).await?;
    Ok(plan)
}
