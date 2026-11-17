use auram_core::{
    download::Downloader,
    jobs::Job,
    pack::Packs,
    runtime::{Platform, Runtime},
    server,
    storage::{self, Layout},
};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let layout = Layout::new(std::env::temp_dir().join("auram-runtime-verification"))?;
    let packs = Packs {
        net: Downloader::new(&layout)?,
        layout: layout.clone(),
    };
    let runtime = Runtime {
        net: packs.net.clone(),
        layout: layout.clone(),
        platform: Platform::current()?,
    };
    let job = Job {
        emit: std::sync::Arc::new(|p| {
            if p.operation != "download" {
                println!("{}: {}", p.operation, p.message)
            }
        }),
        ..Default::default()
    };
    let full = packs.manifest(packs.latest().await?, &job).await?;
    storage::atomic_copy(
        &packs.file(&full, "mmc-pack.json", &job).await?,
        &layout.client().join("mmc-pack.json"),
    )?;
    // An isolated harness selects the pack commit without installing the graphical client.
    let baseline = auram_core::pack::Manifest {
        files: Default::default(),
        ..full
    };
    storage::write_json(&layout.client().join(".auram-manifest.json"), &baseline)?;
    server::build(&packs, &runtime, &job).await?;
    println!("Server built at {}", layout.server().display());
    Ok(())
}
