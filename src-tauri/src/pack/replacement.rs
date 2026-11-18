//! Destination-volume staging and retained originals for explicit four-folder
//! snapshot replacement. The journal's removal is the multi-path commit point.
use super::{distribution::policy, source, Manifest};
use crate::{jobs::Job, storage};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

const JOURNAL: &str = ".auram-replacement.json";
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Saved {
    relative: String,
    existed: bool,
    directory: bool,
    install: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Journal {
    schema: u32,
    id: String,
    server: bool,
    paths: Vec<Saved>,
}

pub(super) fn directories(server: bool) -> Vec<String> {
    policy()
        .replacement_directories
        .iter()
        .map(|p| {
            if server {
                p.clone()
            } else {
                format!("minecraft/{p}")
            }
        })
        .collect()
}
pub(super) fn inside(path: &str, server: bool) -> bool {
    directories(server)
        .iter()
        .any(|d| path == d || path.starts_with(&format!("{d}/")))
}
fn work(root: &Path, id: &str) -> Result<PathBuf> {
    if id.len() != 16 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("Invalid replacement journal identity");
    }
    storage::safe_join(root, &format!(".auram-backups/{id}"))
}
fn remove_owned(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}
pub(super) fn recover(root: &Path) -> Result<bool> {
    let pointer = storage::safe_join(root, JOURNAL)?;
    let Some(journal) = storage::read_optional::<Journal>(&pointer)? else {
        return Ok(false);
    };
    if journal.schema != 1 {
        bail!("Unsupported scoped replacement journal");
    }
    let workspace = work(root, &journal.id)?;
    let allowed = directories(journal.server);
    let mut names = std::collections::BTreeSet::new();
    for saved in &journal.paths {
        storage::safe_join(root, &saved.relative)?;
        if !names.insert(saved.relative.clone())
            || saved.directory && !allowed.contains(&saved.relative)
        {
            bail!("Invalid scoped replacement path");
        }
    }
    for saved in journal.paths.iter().rev() {
        let original = storage::safe_join(&workspace.join("original"), &saved.relative)?;
        let target = storage::safe_join(root, &saved.relative)?;
        if original.try_exists()? {
            remove_owned(&target)?;
            fs::create_dir_all(target.parent().context("Replacement path has no parent")?)?;
            fs::rename(&original, &target)
                .context("Restore the retained original replacement path")?;
        } else if !saved.existed {
            remove_owned(&target)?;
        } else if !target.try_exists()? {
            bail!("Replacement recovery cannot find the original {}; retained recovery state was left intact",saved.relative);
        }
    }
    fs::remove_file(pointer)?;
    // Orphaned staging/backups remain identifiable and safe to inspect; they
    // cannot replay after recovery or erase a later successful installation.
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply(
    root: &Path,
    desired: &Manifest,
    next: &Manifest,
    changes: &[(String, Option<PathBuf>)],
    blob_root: &Path,
    server: bool,
    job: &Job,
) -> Result<()> {
    apply_checked(
        root,
        desired,
        next,
        changes,
        blob_root,
        server,
        job,
        &|_| Ok(()),
    )
}
#[allow(clippy::too_many_arguments)]
fn apply_checked(
    root: &Path,
    desired: &Manifest,
    next: &Manifest,
    changes: &[(String, Option<PathBuf>)],
    blob_root: &Path,
    server: bool,
    job: &Job,
    checkpoint: &dyn Fn(&str) -> Result<()>,
) -> Result<()> {
    recover(root)?;
    let id = format!("{:016x}", rand::random::<u64>());
    let workspace = work(root, &id)?;
    fs::create_dir_all(workspace.join("stage"))?;
    let dirs = directories(server);
    let mut journal = Journal {
        schema: 1,
        id,
        server,
        paths: Vec::new(),
    };
    for directory in &dirs {
        if desired.files.contains_key(directory) {
            bail!("The pack requires {directory} to be a directory");
        }
        let target = storage::safe_join(root, directory)?;
        if target.try_exists()? && !target.is_dir() {
            bail!("A non-directory blocks replacement of {directory}");
        }
        fs::create_dir_all(storage::safe_join(&workspace.join("stage"), directory)?)?;
        journal.paths.push(Saved {
            relative: directory.clone(),
            existed: target.try_exists()?,
            directory: true,
            install: true,
        });
    }
    let entries = desired
        .files
        .iter()
        .filter(|(p, _)| inside(p, server))
        .collect::<Vec<_>>();
    let progress = crate::local_work::Progress::new(
        job,
        "Staging verified replacement folders",
        entries.len(),
    );
    for (path, file) in entries {
        job.check()?;
        let source = blob_root.join(&file.sha);
        if source.metadata()?.len() != file.size
            || !crate::download::Checksum::GitBlob(file.sha.clone()).verify(&source)?
        {
            bail!("Replacement source failed integrity verification: {path}");
        }
        let staged = storage::safe_join(&workspace.join("stage"), path)?;
        storage::atomic_copy(&source, &staged)?;
        source::set_mode(&staged, &file.mode)?;
        progress.complete();
    }
    for (relative, source) in changes
        .iter()
        .filter(|(p, _)| !inside(p, server) && p != ".auram-manifest.json")
    {
        job.check()?;
        let target = storage::safe_join(root, relative)?;
        if target.is_dir() {
            bail!("A directory blocks replacement of managed file {relative}");
        }
        if let Some(source) = source {
            let staged = storage::safe_join(&workspace.join("stage"), relative)?;
            storage::atomic_copy(source, &staged)?;
            if let Some(file) = desired.files.get(relative) {
                if staged.metadata()?.len() != file.size
                    || !crate::download::Checksum::GitBlob(file.sha.clone()).verify(&staged)?
                {
                    bail!("Replacement managed file failed integrity verification");
                }
                source::set_mode(&staged, &file.mode)?;
            }
        }
        journal.paths.push(Saved {
            relative: relative.clone(),
            existed: target.try_exists()?,
            directory: false,
            install: source.is_some(),
        });
    }
    storage::write_json(&workspace.join("stage/.auram-manifest.json"), next)?;
    journal.paths.push(Saved {
        relative: ".auram-manifest.json".into(),
        existed: root.join(".auram-manifest.json").try_exists()?,
        directory: false,
        install: true,
    });
    job.check()?;
    storage::write_json(&root.join(JOURNAL), &journal)?;
    // Cancellation is deferred from here until activation or rollback finishes.
    let apply = (|| {
        checkpoint("prepared")?;
        for (index, saved) in journal.paths.iter().enumerate() {
            let target = storage::safe_join(root, &saved.relative)?;
            let original = storage::safe_join(&workspace.join("original"), &saved.relative)?;
            if saved.existed {
                checkpoint(&format!("before-backup-{index}"))?;
                fs::create_dir_all(original.parent().unwrap())?;
                fs::rename(&target, &original).with_context(|| {
                    format!(
                        "Retain original {}; close programs using the destination and retry",
                        saved.relative
                    )
                })?;
                checkpoint(&format!("after-backup-{index}"))?;
            }
            if saved.install {
                checkpoint(&format!("before-activate-{index}"))?;
                let staged = storage::safe_join(&workspace.join("stage"), &saved.relative)?;
                fs::create_dir_all(target.parent().unwrap())?;
                fs::rename(staged, &target)
                    .with_context(|| format!("Activate {}", saved.relative))?;
                checkpoint(&format!("after-activate-{index}"))?;
            }
            job.progress(
                "pack-apply",
                "Activating verified folders and managed files",
                (index + 1) as u64,
                journal.paths.len() as u64,
            );
        }
        checkpoint("before-commit")?;
        fs::remove_file(root.join(JOURNAL))?;
        checkpoint("after-commit")?;
        Ok::<_, anyhow::Error>(())
    })();
    if apply.is_err() {
        recover(root).context("Restore the previous installation after replacement failed")?;
    }
    apply
}

#[cfg(test)]
#[path = "replacement_tests.rs"]
mod tests;
