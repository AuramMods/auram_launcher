//! Disposable loopback SFTP benchmark; never targets a user's server.
use anyhow::{bail, Context, Result};
use auram_core::{
    deploy::{
        self,
        connection::{self, ConnectionSettings, HostKeyNeeded, Remote, Secret},
    },
    download::Downloader,
    jobs::Job,
    pack::{Manifest, PackFile, PackTag, Packs},
    server::ServerVersion,
    storage::{self, Layout},
};
use sha1::Digest;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Instant,
};
fn blob(bytes: &[u8]) -> String {
    let mut h = sha1::Sha1::new();
    h.update(format!("blob {}\0", bytes.len()));
    h.update(bytes);
    hex::encode(h.finalize())
}
#[tokio::main]
async fn main() -> Result<()> {
    let port = std::env::var("AURAM_SFTP_TEST_PORT")?.parse()?;
    let count = std::env::args()
        .nth(1)
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(128usize);
    let mut settings = ConnectionSettings {
        host: "127.0.0.1".into(),
        port,
        username: "auram".into(),
        directory: "/upload".into(),
        ..Default::default()
    };
    let secret = Secret {
        password: "fixture-only".into(),
        ..Default::default()
    };
    let error = match connection::connect(&settings, &secret, &Job::default()).await {
        Ok(_) => bail!("Fixture accepted an unpinned key"),
        Err(error) => error,
    };
    settings.fingerprint = error
        .downcast_ref::<HostKeyNeeded>()
        .context("Expected fixture SSH host key")?
        .fingerprint
        .clone();
    let remote = connection::connect(&settings, &secret, &Job::default()).await?;
    let name = format!("bench-{:016x}", rand::random::<u64>());
    remote.mkdir(&name).await?;
    remote.close().await;
    settings.directory = format!("/upload/{name}");
    let remote = connection::connect_pool(&settings, &secret, &Job::default()).await?;
    let connections = remote.connection_count();
    // An optional disposable TCP proxy can lower its limit after all twenty
    // connections are established, exercising slot retirement mid-deployment.
    if let Ok(path) = std::env::var("AURAM_SFTP_BENCH_LIMIT_FILE") {
        std::fs::write(path, "3")?;
    }
    let temp = tempfile::tempdir()?;
    let layout = Layout::new(temp.path().into())?;
    let packs = Packs::new(layout.clone(), Downloader::new(&layout)?);
    let mut manifest = Manifest {
        schema: 1,
        policy: 0,
        tag: PackTag {
            name: "fixture".into(),
            sha: blob(b"fixture"),
        },
        files: BTreeMap::new(),
    };
    for index in 0..count {
        let path = format!("config/group-{}/nested/{index}.json", index % 16);
        let bytes = format!("{{\"setting\":{index}}}");
        let file = PackFile {
            sha: blob(bytes.as_bytes()),
            size: bytes.len() as u64,
            mode: "100644".into(),
            source: Some(format!("minecraft/{path}")),
        };
        storage::atomic_write(&packs.blob_path(&file), bytes.as_bytes())?;
        manifest.files.insert(path, file);
    }
    storage::write_json(&layout.state("server-deploy-payload.json"), &manifest)?;
    storage::write_json(
        &layout.server().join("auram.json"),
        &ServerVersion {
            schema: 1,
            version: "fixture".into(),
            commit: manifest.tag.sha.clone(),
            minecraft: "1.20.1".into(),
            forge: "47.4.10".into(),
        },
    )?;
    let started = Instant::now();
    let first = Arc::new(Mutex::new(None));
    let observed = first.clone();
    let job = Job::new(
        "Small-file benchmark",
        &[],
        Arc::new(move |progress| {
            if progress.uploaded_bytes > 0 {
                observed
                    .lock()
                    .unwrap()
                    .get_or_insert(started.elapsed().as_secs_f64());
            }
        }),
    );
    let plan = deploy::inspect(&packs, &remote, &job).await?;
    deploy::apply(&packs, &remote, plan, &job).await?;
    let elapsed = started.elapsed().as_secs_f64();
    for index in 0..count {
        let path = format!("config/group-{}/nested/{index}.json", index % 16);
        let bytes = remote.read(&path, 1024).await?;
        if blob(&bytes) != manifest.files[&path].sha {
            bail!("Fixture content mismatch");
        }
    }
    println!(
        "{}",
        serde_json::json!({"files":count,"connections":connections,"remainingConnections":remote.connection_count(),"firstUploadSeconds":*first.lock().unwrap(),"deploymentSeconds":elapsed,"filesPerSecond":count as f64/elapsed,"integrity":"all uploaded bodies matched"})
    );
    remote.close().await;
    Ok(())
}
