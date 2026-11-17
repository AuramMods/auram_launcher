//! Read-only, warm-cache comparison using an existing managed server's real files.
use anyhow::Result;
use auram_core::{download::Checksum, local_work, pack::Manifest, storage};
use std::{path::PathBuf, time::Instant};
fn main() -> Result<()> {
    let root = PathBuf::from(std::env::args().nth(1).expect("server directory"));
    let limit = std::env::args()
        .nth(2)
        .map(|n| n.parse::<u64>())
        .transpose()?
        .unwrap_or(256 * 1024 * 1024);
    let manifest: Manifest = storage::read_json(&root.join(".auram-manifest.json"))?;
    let mut bytes = 0;
    let files = manifest
        .files
        .iter()
        .take_while(|(_, file)| {
            bytes += file.size;
            bytes <= limit
        })
        .collect::<Vec<_>>();
    let check = |(relative, file): &(&String, &auram_core::pack::PackFile)| -> Result<bool> {
        let target = storage::safe_join(&root, relative)?;
        Checksum::GitBlob(file.sha.clone()).verify(&target)
    };
    // Warm identical inputs before either timed pass; all operations are read-only.
    let expected = local_work::map(&files, check)?;
    for mode in ["serial", "parallel", "parallel", "serial"] {
        let started = Instant::now();
        let actual = if mode == "serial" {
            files.iter().map(check).collect::<Result<Vec<_>>>()?
        } else {
            local_work::map(&files, check)?
        };
        anyhow::ensure!(actual == expected, "Files changed during the benchmark");
        println!(
            "{mode}: {:.3}s, {} files, {} workers",
            started.elapsed().as_secs_f64(),
            files.len(),
            if mode == "serial" {
                1
            } else {
                local_work::workers()
            }
        );
    }
    Ok(())
}
