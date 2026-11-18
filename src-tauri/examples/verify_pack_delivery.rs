//! Headless source-provider exercise against an isolated data root.
use anyhow::{bail, Context, Result};
use auram_core::{download::Downloader, jobs::Job, pack::Packs, storage::Layout};
use std::{path::PathBuf, time::Instant};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().context("QA data directory is required")?);
    let expected = args.next().context("Expected pack tag is required")?;
    let mode = args.next().context("Specify update, repair or runtime")?;
    let origin = args.next();
    let fixture_key = args.next().map(std::fs::read_to_string).transpose()?;
    let marker = root.join(".auram-delivery-qa");
    if !root.exists() {
        std::fs::create_dir(&root)?;
        std::fs::write(&marker, b"isolated pack QA\n")?;
    } else if !marker.is_file() {
        bail!("Choose an empty path for isolated pack QA");
    }
    let layout = Layout::new(root)?;
    let net = Downloader::new(&layout)?;
    let packs = match origin {
        Some(origin) => Packs::with_qa_origin(layout, net, &origin, fixture_key.as_deref())?,
        None => Packs::new(layout, net),
    };
    let start = Instant::now();
    if mode == "runtime" {
        use auram_core::runtime::{Platform, Runtime, Versions};
        let manifest = packs
            .installed()?
            .context("Install an isolated pack first")?;
        anyhow::ensure!(manifest.tag.name == expected, "Unexpected installed pack");
        let versions = Versions::read(&packs.layout.client())?;
        let runtime = Runtime {
            layout: packs.layout.clone(),
            net: packs.net.clone(),
            platform: Platform::current()?,
        };
        runtime.ensure(versions.clone(), &Job::default()).await?;
        println!(
            "{}",
            serde_json::json!({
                "target":manifest.tag, "minecraft":versions.minecraft, "forge":versions.forge,
                "runtimeDownloadBodyBytes":packs.net.transferred_bytes(),
                "seconds":start.elapsed().as_secs_f64()
            })
        );
        return Ok(());
    }
    let repair = match mode.as_str() {
        "update" => false,
        "repair" => true,
        _ => bail!("Specify update, repair or runtime"),
    };
    let manifest = packs.update(repair, &Job::default()).await?;
    anyhow::ensure!(
        manifest.tag.name == expected,
        "Expected pack {expected}, got {}",
        manifest.tag.name
    );
    println!(
        "{}",
        serde_json::json!({
            "target":manifest.tag,
            "seconds":start.elapsed().as_secs_f64(),
            "managedFiles":manifest.files.len(),
            "downloadBodyBytes":packs.net.transferred_bytes(),
            "strategy":auram_core::storage::read_optional::<serde_json::Value>(
                &packs.layout.state("last-pack-transfer.json")
            )?,
        }),
    );
    Ok(())
}
