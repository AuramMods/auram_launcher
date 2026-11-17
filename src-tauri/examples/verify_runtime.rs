use auram_core::{
    download::Downloader,
    jobs::Job,
    runtime::{Platform, Runtime, Versions},
    storage::Layout,
};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    auram_core::java_bridge::entry();
    let layout = Layout::new(std::env::temp_dir().join("auram-runtime-verification"))?;
    let runtime = Runtime {
        net: Downloader::new(&layout)?,
        layout,
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
    if std::env::args().any(|a| a == "--client") {
        let ready = runtime
            .ensure(
                Versions {
                    minecraft: "1.20.1".into(),
                    forge: "47.4.10".into(),
                },
                &job,
            )
            .await?;
        println!(
            "Verified download body bytes: {}",
            runtime.net.transferred_bytes()
        );
        println!(
            "Forge {} ready: {}",
            ready.versions.forge,
            ready.java.display()
        );
    } else {
        println!("Java ready: {}", runtime.java(&job).await?.display());
    }
    Ok(())
}
