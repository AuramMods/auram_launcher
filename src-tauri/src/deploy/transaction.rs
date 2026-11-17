use super::{
    connection::{parents, Remote},
    payload_path,
    plan::{Action, ActionKind, DeploymentPlan},
    safe_path,
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
    if journal.schema != 1
        || journal.id.len() != 32
        || !journal.id.bytes().all(|c| c.is_ascii_hexdigit())
        || journal.actions.len() > 100002
    {
        bail!("Invalid remote deployment journal");
    }
    let mut names = BTreeSet::new();
    for action in &journal.actions {
        safe_path(&action.path)?;
        if !(payload_path(&action.path)
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
pub async fn recover(remote: &impl Remote, job: &Job) -> Result<()> {
    job.phase("sftp-recover", "Recover deployment", false);
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
        if !exists(remote, &started(index)).await? {
            continue;
        }
        job.progress(
            "sftp-recover",
            "Restoring the previous remote installation",
            (journal.actions.len() - index) as u64,
            journal.actions.len() as u64,
        );
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
            let installed = matches!(action.kind, ActionKind::Upload)
                && !exists(remote, &stage(&journal.id, index)).await?;
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
    }
    unlock(remote, &journal).await?;
    job.progress(
        "sftp-recover",
        "Previous remote installation restored; retry deployment when ready",
        1,
        1,
    );
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
    plan.actions.sort_by_key(|a| {
        (
            usize::from(!a.path.starts_with("mods/")),
            match a.kind {
                ActionKind::Remove => 0,
                ActionKind::Rename => 1,
                ActionKind::Upload => 2,
            },
        )
    });
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
    for action in &plan.actions {
        if let Some(meta) = remote.stat(&action.path).await? {
            if meta.directory || meta.symlink {
                bail!("Remote directory or symlink blocks file {}", action.path);
            }
            originals.insert(action.path.clone());
        }
    }
    let journal = Journal {
        schema: 1,
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
        job.phase("sftp-upload", "Upload files", true);
        job.plan_downloads(
            "SFTP uploads",
            &plan
                .actions
                .iter()
                .filter_map(|a| {
                    a.file
                        .as_ref()
                        .filter(|_| matches!(a.kind, ActionKind::Upload))
                        .map(|f| (a.path.clone(), a.path.clone(), Some(f.size)))
                })
                .collect::<Vec<_>>(),
        );
        parents(remote, &stage(&id, 0)).await?;
        futures_util::stream::iter(
            plan.actions
                .clone()
                .into_iter()
                .enumerate()
                .filter(|(_, action)| matches!(action.kind, ActionKind::Upload)),
        )
        .map(|(index, action)| {
            let id=&id;let marker=&marker;let ownership=&ownership;let payload_manifest=&plan.payload;
            async move {
            job.check()?;
            let target = stage(id, index);
            if let Some(payload) = &action.payload {
                let local = packs.file(payload_manifest, payload, job).await?;
                remote.upload(&local, &target, &action.path, job).await?;
            } else {
                remote
                    .write(
                        &target,
                        if action.path == "auram.json" {
                            marker
                        } else {
                            ownership
                        },
                    )
                    .await?;
            }
            Ok::<_, anyhow::Error>(())
            }
        })
        .buffer_unordered(4)
        .try_collect::<Vec<_>>()
        .await?;
        job.check()?;
        for action in &journal.actions {
            let present=exists(remote,&action.path).await?;
            if present!=journal.originals.contains(&action.path) {bail!("Remote file moved while staging: {}. Stop the server and retry.",action.path);}
            if let Some(from)=&action.from {if !exists(remote,from).await?{bail!("Remote rename source moved while staging: {from}. Stop the server and retry.");}}
            let alternate=if action.path.starts_with("mods/") {
                action.path.strip_suffix(".jar.client").map(|p|format!("{p}.jar")).or_else(||action.path.ends_with(".jar").then(||format!("{}.client",action.path)))
            }else{None};
            if let Some(alternate)=alternate {if exists(remote,&alternate).await?{bail!("Remote jar state changed while staging: {alternate}. Stop the server and retry.");}}
        }
        job.phase("sftp-apply", "Activate update", false);
        // Cancellation is deferred during activation, as in local pack updates.
        for (index, action) in plan.actions.iter().enumerate() {
            job.progress(
                "sftp-apply",
                "Activating staged files; retaining originals",
                index as u64,
                plan.actions.len() as u64,
            );
            parents(remote, &action.path).await?;
            remote.write(&started(index), b"started").await?;
            if matches!(action.kind, ActionKind::Rename) {
                remote
                    .rename(
                        action.from.as_ref().context("Rename source missing")?,
                        &action.path,
                    )
                    .await?;
            } else {
                if journal.originals.contains(&action.path) {
                    let old = backup(&id, index);
                    parents(remote, &old).await?;
                    remote.rename(&action.path, &old).await?;
                }
                if matches!(action.kind, ActionKind::Upload) {
                    remote.rename(&stage(&id, index), &action.path).await?;
                }
            }
        }
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
        format!("Remote Auram {} deployed", plan.target_version.version),
        1,
        1,
    );
    Ok(())
}
