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
    auram_core::java_bridge::entry();
    let root = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("auram-runtime-verification"));
    let layout = Layout::new(root)?;
    let packs = Packs::new(layout.clone(), Downloader::new(&layout)?);
    let runtime = Runtime {
        net: packs.net.clone(),
        layout: layout.clone(),
        platform: Platform::current()?,
    };
    let job = Job::new(
        "Verification",
        &[],
        std::sync::Arc::new(|p| {
            if p.operation != "download" {
                println!("{}: {}", p.operation, p.message)
            }
        }),
    );
    if packs.installed()?.is_none() {
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
    }
    let started = std::time::Instant::now();
    server::build(&packs, &runtime, &job).await?;
    println!(
        "Server built at {} in {:.3}s with {} local workers",
        layout.server().display(),
        started.elapsed().as_secs_f64(),
        auram_core::local_work::workers()
    );
    Ok(())
}
