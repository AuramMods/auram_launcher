//! Disposable loopback benchmark for incremental removals, not directory swaps.
use anyhow::{bail, Context, Result};
use auram_core::{
    deploy::{
        self,
        connection::{self, ConnectionSettings, HostKeyNeeded, Remote, Secret},
    },
    download::Downloader,
    jobs::Job,
    pack::{Manifest, PackTag, Packs},
    server::ServerVersion,
    storage::{self, Layout},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Instant,
};

#[tokio::main]
async fn main() -> Result<()> {
    let count = std::env::args()
        .nth(1)
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(64usize);
    let mut settings = ConnectionSettings {
        host: "127.0.0.1".into(),
        port: std::env::var("AURAM_SFTP_TEST_PORT")?.parse()?,
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
        .context("Expected fixture host key")?
        .fingerprint
        .clone();
    let remote = connection::connect(&settings, &secret, &Job::default()).await?;
    let name = format!("activation-{:016x}", rand::random::<u64>());
    remote.mkdir(&name).await?;
    remote.close().await;
    settings.directory = format!("/upload/{name}");
    let remote = connection::connect_pool(&settings, &secret, &Job::default()).await?;
    let connections = remote.connection_count();
    remote.mkdir("config").await?;
    remote.mkdir("world").await?;
    remote.write("world/level.dat", b"world").await?;
    remote.write("server.properties", b"private").await?;
    for index in 0..count {
        remote
            .write(
                &format!("config/{index}.toml"),
                format!("value={index}").as_bytes(),
            )
            .await?;
    }
    let temp = tempfile::tempdir()?;
    let layout = Layout::new(temp.path().into())?;
    let packs = Packs::new(layout.clone(), Downloader::new(&layout)?);
    let version = ServerVersion {
        schema: 1,
        version: "activation-fixture".into(),
        commit: "a".repeat(40),
        minecraft: "1.20.1".into(),
        forge: "47.4.10".into(),
    };
    let manifest = Manifest {
        schema: 1,
        policy: 0,
        tag: PackTag {
            name: version.version.clone(),
            sha: version.commit.clone(),
        },
        files: BTreeMap::new(),
    };
    storage::write_json(&layout.state("server-deploy-payload.json"), &manifest)?;
    storage::write_json(&layout.server().join("auram.json"), &version)?;
    let mut plan = deploy::inspect(&packs, &remote, &Job::default()).await?;
    plan.actions = (0..count)
        .map(|index| {
            serde_json::from_value(
                serde_json::json!({"kind":"remove", "path":format!("config/{index}.toml")}),
            )
            .unwrap()
        })
        .collect();
    plan.replacements = 0;
    plan.removals = count;
    plan.uploads = 0;
    let activation = Arc::new(Mutex::new(None));
    let observed = activation.clone();
    let job = Job::new(
        "Activation benchmark",
        &[],
        Arc::new(move |p| {
            if p.phases
                .get(p.active_phase)
                .is_some_and(|phase| phase.id == "sftp-apply")
            {
                observed.lock().unwrap().get_or_insert_with(Instant::now);
            }
        }),
    );
    let started = Instant::now();
    deploy::apply(&packs, &remote, plan, &job).await?;
    let elapsed = started.elapsed().as_secs_f64();
    let activation_seconds = activation
        .lock()
        .unwrap()
        .context("Missing activation phase")?
        .elapsed()
        .as_secs_f64();
    let transactions = remote.list(".auram-deploy/transactions").await?;
    let transaction = transactions
        .iter()
        .find(|(_, attrs)| attrs.directory)
        .context("Missing transaction")?
        .0
        .clone();
    for index in 0..count {
        if remote
            .stat(&format!("config/{index}.toml"))
            .await?
            .is_some()
        {
            bail!("Obsolete file still present");
        }
        let bytes = remote
            .read(
                &format!(".auram-deploy/transactions/{transaction}/backup/{index}"),
                1024,
            )
            .await?;
        if bytes != format!("value={index}").as_bytes() {
            bail!("Backup mismatch");
        }
    }
    if remote.read("world/level.dat", 1024).await? != b"world"
        || remote.read("server.properties", 1024).await? != b"private"
    {
        bail!("User data changed");
    }
    let installed: ServerVersion = serde_json::from_slice(&remote.read("auram.json", 1024).await?)?;
    if installed.version != version.version {
        bail!("Version mismatch");
    }
    println!(
        "{}",
        serde_json::json!({"files":count,"connections":connections,"remainingConnections":remote.connection_count(),"activationSeconds":activation_seconds,"deploymentSeconds":elapsed,"integrity":"all originals retained, user data untouched, version correct"})
    );
    remote.close().await;
    Ok(())
}
