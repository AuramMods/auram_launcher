//! Bounded, ordered activation. All admitted mutations settle before rollback.
use super::{
    connection::Remote,
    plan::{Action, ActionKind},
    MAX_CONNECTIONS,
};
use anyhow::{bail, Result};
use futures_util::{stream::FuturesUnordered, StreamExt};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    ops::Range,
};

pub(super) fn order(action: &Action) -> (u8, u8) {
    (
        u8::from(action.path != "mods" && !action.path.starts_with("mods/")),
        match action.kind {
            ActionKind::Remove => 0,
            ActionKind::Rename => 1,
            ActionKind::Upload | ActionKind::ReplaceDirectory => 2,
        },
    )
}
fn overlaps(paths: &BTreeSet<String>, path: &str) -> bool {
    let mut ancestor = path;
    loop {
        if paths.contains(ancestor) {
            return true;
        }
        match ancestor.rsplit_once('/') {
            Some((parent, _)) => ancestor = parent,
            None => break,
        }
    }
    let prefix = format!("{path}/");
    paths
        .range(prefix.clone()..)
        .next()
        .is_some_and(|p| p.starts_with(&prefix))
}
/// Retain mod/remove/rename/upload ordering and serialize overlapping paths.
/// Sources also count as writes. Be conservative on case-insensitive hosts.
pub(super) fn waves(actions: &[Action]) -> Vec<Range<usize>> {
    let mut result = Vec::new();
    let mut paths = BTreeSet::new();
    let mut start = 0;
    for (index, action) in actions.iter().enumerate() {
        let touched = std::iter::once(&action.path)
            .chain(action.from.iter())
            .map(|p| p.to_lowercase())
            .collect::<Vec<_>>();
        let metadata = |a: &Action| ["auram.json", ".auram-deploy.json"].contains(&a.path.as_str());
        if index > start
            && (order(action) != order(&actions[index - 1])
                || metadata(action)
                || metadata(&actions[index - 1])
                || touched.iter().any(|p| overlaps(&paths, p)))
        {
            result.push(start..index);
            start = index;
            paths.clear();
        }
        paths.extend(touched);
    }
    if start < actions.len() {
        result.push(start..actions.len());
    }
    result
}

/// Do not short-circuit/drop in-flight live writes when a peer fails. Stop
/// admitting work and drain every started request before restoring backups.
pub(super) async fn settle<F: Future<Output = Result<()>>>(
    mut operations: impl Iterator<Item = F>,
) -> Result<()> {
    let mut running = FuturesUnordered::new();
    for operation in operations.by_ref().take(MAX_CONNECTIONS) {
        running.push(operation);
    }
    let mut first_error = None;
    while let Some(result) = running.next().await {
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
        if first_error.is_none() {
            if let Some(operation) = operations.next() {
                running.push(operation);
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Deduplicate parent checks for a wave; each directory is created once only
/// after its ancestors exist. Leaf checks remain fresh on every SFTP mutation.
pub(super) async fn parents(
    remote: &impl Remote,
    paths: impl Iterator<Item = String>,
) -> Result<()> {
    let mut directories = BTreeSet::new();
    for path in paths {
        let mut child = path.as_str();
        while let Some((parent, _)) = child.rsplit_once('/') {
            directories.insert(parent.to_owned());
            child = parent;
        }
    }
    let mut by_depth = BTreeMap::<usize, Vec<String>>::new();
    for path in directories {
        by_depth
            .entry(path.split('/').count())
            .or_default()
            .push(path);
    }
    for paths in by_depth.into_values() {
        settle(paths.into_iter().map(|path| async move {
            match remote.stat(&path).await? {
                Some(meta) if meta.directory && !meta.symlink => Ok(()),
                Some(_) => bail!("A remote file blocks directory {path}"),
                None => remote.mkdir(&path).await,
            }
        }))
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn action(path: &str, from: Option<&str>) -> Action {
        Action {
            kind: if from.is_some() {
                ActionKind::Rename
            } else {
                ActionKind::Upload
            },
            path: path.into(),
            from: from.map(str::to_owned),
            file: None,
            payload: None,
        }
    }
    #[test]
    fn dependency_waves_keep_sources_ancestors_case_collisions_and_metadata_ordered() {
        let actions = [
            action("mods/b.jar", Some("mods/a.jar")),
            action("mods/c.jar", Some("mods/b.jar")),
            action("config/a", None),
            action("config/independent", None),
            action("config/a/nested", None),
            action("config/A/nested", None),
            action(".auram-deploy.json", None),
            action("auram.json", None),
        ];
        assert_eq!(
            waves(&actions),
            vec![0..1, 1..2, 2..4, 4..5, 5..6, 6..7, 7..8]
        );
    }
    #[tokio::test]
    async fn admitted_mutations_finish_even_when_cancel_is_requested() {
        let job = crate::jobs::Job::default();
        let finished = std::sync::atomic::AtomicUsize::new(0);
        settle((0..40).map(|index| {
            let job = &job;
            let finished = &finished;
            async move {
                if index == 0 {
                    job.cancel.cancel();
                }
                tokio::task::yield_now().await;
                finished.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        }))
        .await
        .unwrap();
        assert!(job.cancel.is_cancelled());
        assert_eq!(finished.load(std::sync::atomic::Ordering::SeqCst), 40);
    }
}
