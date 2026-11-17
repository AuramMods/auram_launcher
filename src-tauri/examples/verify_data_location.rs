//! Disposable data relocation and verified download promotion across real volumes.
//! Pass a directory on a different volume to exercise the actual EXDEV fallback.
use anyhow::{bail, Context, Result};
use auram_core::{
    data_location::Locations,
    download::{Checksum, Download, Downloader},
    jobs::Job,
    storage::{self, Layout},
};
use sha2::Digest;
use std::{fs, path::PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[tokio::main]
async fn main() -> Result<()> {
    let parent = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .context("Pass a disposable test directory on another volume")?;
    let source_temp = tempfile::tempdir()?;
    let destination = tempfile::tempdir_in(parent)?;
    let locations = Locations {
        default_root: source_temp.path().join("default/AuramLauncher"),
        control: source_temp.path().join("control"),
        folder_name: "AuramLauncher".into(),
    };
    let layout = Layout::new(locations.default_root.clone())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let from = fs::metadata(&layout.root)?.dev();
        let to = fs::metadata(destination.path())?.dev();
        if from == to {
            bail!("Use two actual filesystem volumes, not two directories on the same volume");
        }
        println!("Actual source device {from}; destination device {to}");
    }
    for (path, bytes) in [
        (
            "client/minecraft/saves/world/level.dat",
            b"world".as_slice(),
        ),
        ("server/world/level.dat", b"server world"),
        ("downloads/resumable.part", b"resumable partial"),
        ("client/minecraft/options.txt", b"options"),
    ] {
        storage::atomic_write(&layout.root.join(path), bytes)?;
    }
    let asset = vec![73u8; 4 * 1024 * 1024];
    storage::atomic_write(
        &layout.root.join("cache/minecraft/assets/objects/asset"),
        &asset,
    )?;
    let root = locations.destination(Some(destination.path()))?;
    locations.relocate(&layout, root.clone(), &Job::default())?;
    if locations.resolve()? != root
        || layout.root.exists()
        || fs::read(root.join("client/minecraft/saves/world/level.dat"))? != b"world"
    {
        bail!("Cross-volume data relocation failed");
    }
    let moved = Layout::new(root.clone())?;
    locations.relocate(&moved, locations.destination(None)?, &Job::default())?;
    if locations.resolve()? != layout.root
        || root.exists()
        || fs::read(layout.root.join("cache/minecraft/assets/objects/asset"))? != asset
    {
        bail!("Reset to default failed");
    }
    println!("PASS real cross-volume copy, verified cache/world/partial preservation, original cleanup and reset to default");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let body = b"verified cross-volume download".to_vec();
    let server_body = body.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut request = [0; 4096];
        let read = stream.read(&mut request).await?;
        if read == 0 {
            bail!("Fixture request closed without data");
        }
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    server_body.len()
                )
                .as_bytes(),
            )
            .await?;
        stream.write_all(&server_body).await?;
        Ok::<_, anyhow::Error>(())
    });
    let net = Downloader::new(&layout)?;
    let download = Download {
        url: format!("http://{address}/fixture"),
        target: destination.path().join("download with spaces 世界.bin"),
        size: Some(body.len() as u64),
        checksum: Checksum::Sha256(hex::encode(sha2::Sha256::digest(&body))),
        label: None,
    };
    net.get(&download, &Job::default()).await?;
    server.await??;
    if fs::read(&download.target)? != body {
        bail!("Promoted download integrity mismatch");
    }
    let transferred = net.transferred_bytes();
    net.get(&download, &Job::default()).await?;
    if transferred != body.len() as u64 || net.transferred_bytes() != transferred {
        bail!("Verified download was fetched again");
    }
    println!(
        "PASS actual cross-volume download promotion and zero-body reuse of its verified result"
    );
    Ok(())
}
