use auram_core::{download::Downloader, jobs::Job, pack::Packs, storage::Layout};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = std::env::temp_dir().join("auram-pack-inspection");
    let layout = Layout::new(root)?;
    let packs = Packs {
        net: Downloader::new(&layout)?,
        layout,
    };
    let tag = packs.latest().await?;
    let full = packs.manifest(tag, &Job::default()).await?;
    let client = packs.client_manifest(&full, &Job::default()).await?;
    println!(
        "Pack {} {}: {} client files, {} bytes",
        client.tag.name,
        client.tag.sha,
        client.files.len(),
        client.files.values().map(|f| f.size).sum::<u64>()
    );
    let meta = packs.file(&full, "mmc-pack.json", &Job::default()).await?;
    println!("{}", std::fs::read_to_string(meta)?);
    Ok(())
}
