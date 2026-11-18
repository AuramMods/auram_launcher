//! Headless isolated Git transport validation; never opens the user's launcher.
use anyhow::{Context, Result};
use auram_core::{
    download::Downloader,
    jobs::Job,
    pack::{distribution::Envelope, git_transport::GitCache, Packs},
    storage::Layout,
};
use std::{collections::BTreeMap, path::PathBuf};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let catalog = PathBuf::from(
        args.next()
            .context("usage: verify_sparse_git <signed-catalog> <isolated-data-dir> [--full]")?,
    );
    let root = PathBuf::from(args.next().context("isolated data directory is required")?);
    let full = args.next().as_deref() == Some("--full");
    let catalog = serde_json::from_slice::<Envelope>(&std::fs::read(catalog)?)?
        .verify(include_str!("../pack.pub"))?;
    let layout = Layout::new(root)?;
    let packs = Packs::new(layout.clone(), Downloader::new(&layout)?);
    let git = GitCache {
        directory: layout.root.join("cache/git-native"),
        remote: "https://github.com/AuramMods/Auram.git".into(),
    };
    let files = catalog
        .files
        .iter()
        .filter(|(path, _)| {
            full || [
                "mmc-pack.json",
                "server_config.conf",
                "minecraft/config/fml.toml",
            ]
            .contains(&path.as_str())
        })
        .map(|(p, f)| (p.clone(), f.clone()))
        .collect::<BTreeMap<_, _>>();
    let job = Job::default();
    let start = std::time::Instant::now();
    git.ensure(&packs, &catalog, &files, &job).await?;
    let first = packs.net.transferred_bytes();
    git.ensure(&packs, &catalog, &files, &job).await?;
    let repeated = packs.net.transferred_bytes() - first;
    anyhow::ensure!(repeated == 0, "repeat retrieval downloaded more bodies");
    println!(
        "{}",
        serde_json::json!({"target":catalog.target,"selectedFiles":files.len(),"firstBytes":first,"repeatBytes":repeated,"elapsedSeconds":start.elapsed().as_secs_f64(),"transport":"native Git v2 blob-filtered snapshot and batched exact objects"})
    );
    Ok(())
}
