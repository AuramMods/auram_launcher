use super::{
    activation,
    connection::{parents, Mutation, Remote},
    payload_path,
    plan::{Action, ActionKind, DeploymentPlan},
    safe_path, MAX_CONNECTIONS, ROOTS,
};
use crate::{jobs::Job, pack::Packs};
use anyhow::{bail, Context, Result};
use futures_util::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Serialize, Deserialize)]
struct Journal {
    schema: u32,
    id: String,
    actions: Vec<Action>,
    originals: BTreeSet<String>,
}
fn data_root(id: &str) -> String {
    format!(".auram-deploy/transactions/{id}")
}
fn stage(id: &str, index: usize) -> String {
    format!("{}/stage/{index}", data_root(id))
}
fn backup(id: &str, index: usize) -> String {
    format!("{}/backup/{index}", data_root(id))
}
fn started(index: usize) -> String {
    format!(".auram-deploy/lock/started-{index}")
}
async fn exists(remote: &impl Remote, path: &str) -> Result<bool> {
    Ok(remote.stat(path).await?.is_some())
}
fn validate(journal: &Journal) -> Result<()> {
    if ![1, 2].contains(&journal.schema)
        || journal.id.len() != 32
        || !journal.id.bytes().all(|c| c.is_ascii_hexdigit())
        || journal.actions.len() > 100002
    {
        bail!("Invalid remote deployment journal");
    }
    let mut names = BTreeSet::new();
    for action in &journal.actions {
        safe_path(&action.path)?;
        let directory = matches!(action.kind, ActionKind::ReplaceDirectory);
        if directory
            && (journal.schema != 2
                || !ROOTS.contains(&action.path.as_str())
                || action.from.is_some()
                || action.file.is_some()
                || action.payload.is_some())
        {
            bail!("Invalid directory replacement");
        }
        if !(directory
            || payload_path(&action.path)
            || ["auram.json", ".auram-deploy.json"].contains(&action.path.as_str()))
            || !names.insert(action.path.clone())
        {
            bail!("Invalid journal destination");
        }
        if let Some(from) = &action.from {
            safe_path(from)?;
            if !payload_path(from) {
                bail!("Invalid journal source");
            }
        }
        if matches!(action.kind, ActionKind::Rename) && action.from.is_none() {
            bail!("Missing rename source");
        }
    }
    for action in &journal.actions {
        if matches!(action.kind, ActionKind::ReplaceDirectory)
            && names
                .iter()
                .any(|p| p.starts_with(&format!("{}/", action.path)))
        {
            bail!("Overlapping directory replacement");
        }
    }
    for path in &journal.originals {
        if !names.contains(path) {
            bail!("Invalid journal original");
        }
    }
    Ok(())
}
async fn unlock(remote: &impl Remote, journal: &Journal) -> Result<()> {
    let receipt = format!("{}/receipt", data_root(&journal.id));
    parents(remote, &receipt).await?;
    remote.rename(".auram-deploy/lock", &receipt).await?;
    Ok(())
}
async fn activate(remote: &impl Remote, journal: &Journal, job: &Job) -> Result<()> {
    job.phase("sftp-apply", "Activate update", true);
    {
        // Cancellation is deferred during activation, as in local pack updates.
        let progress = crate::local_work::Progress::new(
            job,
            "Activating server files in parallel; retaining backups",
            journal.actions.len(),
        );
        // The private backup parent is shared; prepare it once, not per file.
        parents(remote, &backup(&journal.id, 0)).await?;
        for wave in activation::waves(&journal.actions) {
            activation::parents(
                remote,
                journal.actions[wave.clone()].iter().map(|a| a.path.clone()),
            )
            .await?;
            let operations = wave.map(|index| {
                let action = &journal.actions[index];
                let id = &journal.id;
                let originals = &journal.originals;
                let progress = &progress;
                async move {
                    let mut mutations = vec![Mutation::Write {
                        path: started(index),
                        bytes: b"started".to_vec(),
                    }];
                    if matches!(action.kind, ActionKind::Rename) {
                        mutations.push(Mutation::Rename {
                            from: action.from.clone().context("Rename source missing")?,
                            to: action.path.clone(),
                        });
                    } else {
                        if originals.contains(&action.path) {
                            mutations.push(Mutation::Rename {
                                from: action.path.clone(),
                                to: backup(id, index),
                            });
                        }
                        if matches!(
                            action.kind,
                            ActionKind::Upload | ActionKind::ReplaceDirectory
                        ) {
                            mutations.push(Mutation::Rename {
                                from: stage(id, index),
                                to: action.path.clone(),
                            });
                        }
                    }
                    // One lease for the marker → backup → promotion sequence.
                    // An uncertain live mutation is recovered, never replayed.
                    remote.mutate_batch(&mutations).await?;
                    progress.complete();
                    Ok::<_, anyhow::Error>(())
                }
            });
            // Drain admitted operations even on error, before rollback begins.
            // Ownership and auram.json have separate, final serial waves.
            activation::settle(operations).await?;
        }
    }
    Ok(())
}
pub async fn recover(remote: &impl Remote, job: &Job) -> Result<()> {
    job.phase("sftp-recover", "Recover deployment", true);
    let Some(_) = remote.stat(".auram-deploy/lock").await? else {
        return Ok(());
    };
    let journal: Journal = serde_json::from_slice(
        &remote
            .read(".auram-deploy/lock/journal.json", 32 * 1024 * 1024)
            .await?,
    )
    .context("Read the remote deployment journal; another launcher may still be deploying")?;
    validate(&journal)?;
    if exists(remote, ".auram-deploy/lock/complete").await? {
        return unlock(remote, &journal).await;
    }
    // Recovery deliberately ignores cancellation once restoration starts.
    for (index, action) in journal.actions.iter().enumerate().rev() {
        job.progress(
            "sftp-recover",
            "Inspecting and restoring the previous server installation",
            (journal.actions.len() - index - 1) as u64,
            journal.actions.len() as u64,
        );
        if !exists(remote, &started(index)).await? {
            job.progress(
                "sftp-recover",
                "Inspecting and restoring the previous server installation",
                (journal.actions.len() - index) as u64,
                journal.actions.len() as u64,
            );
            continue;
        }
        if matches!(action.kind, ActionKind::Rename) {
            let from = action.from.as_ref().context("Rename source missing")?;
            if exists(remote, &action.path).await? {
                if exists(remote, from).await? {
                    bail!("Cannot restore {from}: another file appeared there. Retained deployment journal and files.");
                }
                parents(remote, from).await?;
                remote.rename(&action.path, from).await?;
            }
        } else {
            let original = backup(&journal.id, index);
            let has_backup = exists(remote, &original).await?;
            let installed = matches!(
                action.kind,
                ActionKind::Upload | ActionKind::ReplaceDirectory
            ) && !exists(remote, &stage(&journal.id, index)).await?;
            if has_backup || (!journal.originals.contains(&action.path) && installed) {
                if exists(remote, &action.path).await? {
                    // Preserve even writes made by a running server during an interruption.
                    let retained = format!("{}/recovered/{index}", data_root(&journal.id));
                    parents(remote, &retained).await?;
                    if exists(remote, &retained).await? {
                        bail!("Recovery backup already exists at {retained}; original files remain retained");
                    }
                    remote.rename(&action.path, &retained).await?;
                }
                if has_backup {
                    parents(remote, &action.path).await?;
                    remote.rename(&original, &action.path).await?;
                }
            }
        }
        remote.remove(&started(index)).await?;
        job.progress(
            "sftp-recover",
            "Inspecting and restoring the previous server installation",
            (journal.actions.len() - index) as u64,
            journal.actions.len() as u64,
        );
    }
    unlock(remote, &journal).await?;
    job.progress(
        "sftp-recover",
        "Previous remote installation restored; retry deployment when ready",
        journal.actions.len() as u64,
        journal.actions.len() as u64,
    );
    job.flush();
    Ok(())
}
pub async fn apply(
    packs: &Packs,
    remote: &impl Remote,
    mut plan: DeploymentPlan,
    job: &Job,
) -> Result<()> {
    if plan.recovery_required {
        bail!("Recover the interrupted deployment first");
    }
    let id = format!("{:032x}", rand::random::<u128>());
    let mut originals = BTreeSet::new();
    // Retire obsolete owned paths first; never recursively replace a directory.
    plan.actions.sort_by_key(activation::order);
    let marker = serde_json::to_vec_pretty(&plan.target_version)?;
    let ownership = serde_json::to_vec_pretty(&plan.baseline)?;
    for path in [".auram-deploy.json", "auram.json"] {
        plan.actions.push(Action {
            kind: ActionKind::Upload,
            path: path.into(),
            from: None,
            file: None,
            payload: None,
        });
    }
    job.phase("sftp-destination", "Check destination", true);
    {
        let progress = crate::local_work::Progress::new(
            job,
            "Checking server files and directories",
            plan.actions.len(),
        );
        let existing = futures_util::stream::iter(plan.actions.clone())
            .map(|action| {
                let progress = &progress;
                async move {
                    job.check()?;
                    let existing = remote.stat(&action.path).await?;
                    if existing.as_ref().is_some_and(|meta| {
                        meta.symlink
                            || meta.directory != matches!(action.kind, ActionKind::ReplaceDirectory)
                    }) {
                        bail!("Remote directory or symlink blocks file {}", action.path);
                    }
                    progress.complete();
                    Ok::<_, anyhow::Error>(existing.map(|_| action.path.clone()))
                }
            })
            .buffer_unordered(MAX_CONNECTIONS)
            .try_collect::<Vec<_>>()
            .await?;
        originals.extend(existing.into_iter().flatten());
    }
    job.phase("sftp-stage", "Prepare folders", true);
    job.progress(
        "sftp-stage",
        "Creating the recoverable deployment journal",
        0,
        0,
    );
    let journal = Journal {
        schema: if plan.replacements > 0 { 2 } else { 1 },
        id: id.clone(),
        actions: plan.actions.clone(),
        originals,
    };
    validate(&journal)?;
    // Construct a nonempty lock directory before its atomic rename. There is
    // no crash window with an empty/unidentifiable lock, and an existing lock
    // cannot be replaced by an SFTP v3 rename of a nonempty directory.
    let prepared = format!(".auram-deploy/prepare-{id}");
    let pointer = format!("{prepared}/journal.json");
    parents(remote, &pointer).await?;
    remote
        .write(&pointer, &serde_json::to_vec(&journal)?)
        .await?;
    if let Err(error) = remote.rename(&prepared, ".auram-deploy/lock").await {
        let _ = remote.remove(&pointer).await;
        let _ = remote.rmdir(&prepared).await;
        return Err(error)
            .context("Acquire remote deployment lock; another launcher may be deploying");
    }
    let result = async {
        // Whole-directory replacement uploads into a separate tree. The live
        // four directories are not moved until every staged upload is ready.
        let staging_root=format!("{}/stage",data_root(&id));
        parents(remote,&stage(&id,0)).await?;
        let mut directories=BTreeSet::new();
        let mut uploads=Vec::new();
        for (index,action) in plan.actions.iter().enumerate() {
            let target=stage(&id,index);
            if matches!(action.kind,ActionKind::ReplaceDirectory) {
                directories.insert(target.clone());
                for (path,file) in &plan.payload.files {
                    if !path.starts_with(&format!("{}/",action.path)) {continue;}
                    let destination=&plan.baseline.files[&super::canonical(path)].path;
                    let relative=destination.strip_prefix(&format!("{}/",action.path)).context("Replacement file outside its directory")?;
                    uploads.push((format!("{target}/{relative}"),Some(path.clone()),Some(file.clone()),destination.clone()));
                }
            } else if matches!(action.kind,ActionKind::Upload) {
                uploads.push((target,action.payload.clone(),action.file.clone(),action.path.clone()));
            }
        }
        for (target,_,_,_) in &uploads {
            let mut parent=target.rsplit_once('/').unwrap().0;
            while parent!=staging_root {
                directories.insert(parent.to_string());
                parent=parent.rsplit_once('/').context("Invalid staging directory")?.0;
            }
        }
        let mut by_depth=std::collections::BTreeMap::<usize,Vec<String>>::new();
        for directory in directories {by_depth.entry(directory.split('/').count()).or_default().push(directory);}
        {
            let total=by_depth.values().map(Vec::len).sum();
            let progress=crate::local_work::Progress::new(job,"Preparing server deployment directories",total);
            for directories in by_depth.into_values() {
                let progress=&progress;
                futures_util::stream::iter(directories).map(|directory|async move {
                    job.check()?;remote.mkdir(&directory).await?;progress.complete();Ok::<_,anyhow::Error>(())
                }).buffer_unordered(MAX_CONNECTIONS).try_collect::<Vec<_>>().await?;
            }
        }
        job.phase("sftp-upload", "Transfer files", true);
        job.plan_downloads("Server uploads",&uploads.iter().filter_map(|(_,payload,file,label)|
            file.as_ref().filter(|_|payload.is_some()).map(|f|(label.clone(),label.clone(),Some(f.size)))
        ).collect::<Vec<_>>());
        futures_util::stream::iter(uploads).map(|(target,payload,_,label)| {
            let marker=&marker;let ownership=&ownership;let payload_manifest=&plan.payload;
            async move {
                job.check()?;
                if let Some(payload)=payload {
                    let local=packs.file(payload_manifest,&payload,job).await?;
                    remote.upload(&local,&target,&label,job).await?;
                } else {
                    remote.write(&target,if label=="auram.json" {marker}else{ownership}).await?;
                }
                Ok::<_,anyhow::Error>(())
            }
        }).buffer_unordered(MAX_CONNECTIONS).try_collect::<Vec<_>>().await?;
        job.check()?;
        job.phase("sftp-validate", "Verify destination", true);
        {
            let progress=crate::local_work::Progress::new(job,"Checking the server has not changed during transfer",journal.actions.len());
            futures_util::stream::iter(journal.actions.clone()).map(|action| {
                let progress=&progress;let originals=&journal.originals;
                async move {
                    job.check()?;
                    let present=exists(remote,&action.path).await?;
                    if present!=originals.contains(&action.path) {bail!("Remote file moved while staging: {}. Stop the server and retry.",action.path);}
                    if let Some(from)=&action.from {if !exists(remote,from).await?{bail!("Remote rename source moved while staging: {from}. Stop the server and retry.");}}
                    let alternate=if action.path.starts_with("mods/") {
                        action.path.strip_suffix(".jar.client").map(|p|format!("{p}.jar")).or_else(||action.path.ends_with(".jar").then(||format!("{}.client",action.path)))
                    }else{None};
                    if let Some(alternate)=alternate {if exists(remote,&alternate).await?{bail!("Remote jar state changed while staging: {alternate}. Stop the server and retry.");}}
                    progress.complete();Ok::<_,anyhow::Error>(())
                }
            }).buffer_unordered(MAX_CONNECTIONS).try_collect::<Vec<_>>().await?;
        }
        activate(remote, &journal, job).await?;
        job.detail("sftp-apply", "Finalizing the deployment journal");
        remote
            .write(".auram-deploy/lock/complete", b"complete")
            .await?;
        unlock(remote, &journal).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        if let Err(rollback) = recover(remote, job).await {
            bail!("Deployment failed: {error:#}. Recovery could not finish: {rollback:#}. Reconnect and choose Recover deployment; originals and journal are retained.");
        }
        if exists(remote, &format!("{}/receipt/complete", data_root(&id)))
            .await
            .unwrap_or(false)
        {
            return Ok(());
        }
        return Err(error).context("Deployment failed; previous remote installation restored");
    }
    job.progress(
        "sftp-apply",
        format!("Auram server {} deployed", plan.target_version.version),
        plan.actions.len() as u64,
        plan.actions.len() as u64,
    );
    job.flush();
    Ok(())
}
