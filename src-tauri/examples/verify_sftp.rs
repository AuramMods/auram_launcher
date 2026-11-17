//! Live loopback-only SFTP regression, intended for the documented disposable container.
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
use std::collections::BTreeMap;
fn blob(bytes: &[u8]) -> String {
    let mut h = sha1::Sha1::new();
    h.update(format!("blob {}\0", bytes.len()));
    h.update(bytes);
    hex::encode(h.finalize())
}
fn payload(packs: &Packs, version: &str, files: &[(&str, &[u8])]) -> Result<()> {
    let tag = PackTag {
        name: version.into(),
        sha: blob(version.as_bytes()),
    };
    let mut payload = Manifest {
        schema: 1,
        policy: 0,
        tag: tag.clone(),
        files: BTreeMap::new(),
    };
    for (path, bytes) in files {
        let file = PackFile {
            sha: blob(bytes),
            size: bytes.len() as u64,
            mode: "100644".into(),
            source: Some(format!("minecraft/{path}")),
        };
        storage::atomic_write(&packs.blob_path(&file), bytes)?;
        payload.files.insert(path.to_string(), file);
    }
    storage::write_json(&packs.layout.state("server-deploy-payload.json"), &payload)?;
    storage::write_json(
        &packs.layout.server().join("auram.json"),
        &ServerVersion {
            schema: 1,
            version: version.into(),
            commit: tag.sha,
            minecraft: "1.20.1".into(),
            forge: "47.4.10".into(),
        },
    )?;
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let port = std::env::var("AURAM_SFTP_TEST_PORT")?.parse()?;
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
    let job = Job::default();
    let error = match connection::connect(&settings, &secret, &job).await {
        Ok(_) => bail!("Unknown SSH key was accepted"),
        Err(e) => e,
    };
    let key = error
        .downcast_ref::<HostKeyNeeded>()
        .context("Expected SSH host-key confirmation")?;
    settings.fingerprint = key.fingerprint.clone();
    let remote = connection::connect(&settings, &secret, &job).await?;
    let directory = format!("test-{:016x}", rand::random::<u64>());
    remote.mkdir(&directory).await?;
    remote.close().await;
    settings.directory = format!("/upload/{directory}");
    let remote = connection::connect_pool(&settings, &secret, &job).await?;
    if remote.connection_count() != deploy::MAX_CONNECTIONS {
        bail!("Expected twenty actual SFTP connections");
    }
    // SFTP v3 must not overwrite the nonempty deployment lock directory.
    for path in ["lock-a/a", "lock-b/b"] {
        connection::parents(&remote, path).await?;
        remote.write(path, b"lock").await?;
    }
    if remote.rename("lock-a", "lock-b").await.is_ok() {
        bail!("Server overwrites nonempty lock directories");
    }
    let temp = tempfile::tempdir()?;
    let layout = Layout::new(temp.path().into())?;
    let packs = Packs::new(layout.clone(), Downloader::new(&layout)?);
    if std::env::args().any(|arg| arg == "--credential-store") {
        settings.save(
            &layout,
            Some(Secret {
                password: "fixture-only".into(),
                passphrase: "fixture-passphrase".into(),
            }),
        )?;
        let restored = ConnectionSettings::load(&layout)?;
        let credentials = restored.secret(&layout)?;
        if restored.host != settings.host
            || restored.fingerprint != settings.fingerprint
            || credentials.password != "fixture-only"
            || credentials.passphrase != "fixture-passphrase"
        {
            bail!("Native credential persistence failed");
        }
        let file = std::fs::read_to_string(layout.state("sftp-connection.json"))?;
        if file.contains("fixture-only") || file.contains("fixture-passphrase") {
            bail!("Secret leaked into ordinary settings");
        }
        restored.forget(&layout)?;
        if !restored.secret(&layout)?.password.is_empty() {
            bail!("Native credential deletion failed");
        }
        println!("PASS: native credential-store persistence, metadata reload, no plaintext settings and credential deletion");
        for (id, password) in [("saved-a", "instance-a"), ("saved-b", "instance-b")] {
            settings.save_instance_secret(
                &layout,
                id,
                Secret {
                    password: password.into(),
                    passphrase: String::new(),
                },
            )?;
        }
        if settings.instance_secret(&layout, "saved-a")?.password != "instance-a"
            || settings.instance_secret(&layout, "saved-b")?.password != "instance-b"
        {
            bail!("Saved server credentials were shared across instances");
        }
        settings.forget_instance_secret(&layout, "saved-a")?;
        if !settings
            .instance_secret(&layout, "saved-a")?
            .password
            .is_empty()
            || settings.instance_secret(&layout, "saved-b")?.password != "instance-b"
        {
            bail!("Removing one saved server affected another credential");
        }
        settings.forget_instance_secret(&layout, "saved-b")?;
        println!("PASS: per-instance native credentials survive reload and independent deletion");
    }
    payload(
        &packs,
        "fixture-old",
        &[
            ("mods/client.jar", b"client"),
            ("mods/rename-old.jar", b"same"),
            ("config/a.toml", b"before"),
            ("scripts/a.zs", b"script"),
            ("kubejs/server_scripts/a.js", b"js"),
        ],
    )?;
    for (path, bytes) in [
        ("mods/client.jar.client", b"client".as_slice()),
        ("mods/rename-old.jar.client", b"same"),
        ("world/level.dat", b"world"),
        ("server.properties", b"private"),
    ] {
        connection::parents(&remote, path).await?;
        remote.write(path, bytes).await?;
    }
    let plan = deploy::inspect(&packs, &remote, &job).await?;
    if plan.replacements != 4 || plan.uploads != 5 {
        bail!("Unexpected initial plan");
    }
    deploy::apply(&packs, &remote, plan, &job).await?;
    let bytes = vec![b'x'; 262145];
    payload(
        &packs,
        "fixture-new",
        &[
            ("mods/client.jar", b"CLIENT"),
            ("mods/rename-new.jar", b"same"),
            ("mods/large.jar", &bytes),
            ("config/a.toml", b"after"),
            ("scripts/a.zs", b"script"),
            ("kubejs/server_scripts/a.js", b"js"),
        ],
    )?;
    let plan = deploy::inspect(&packs, &remote, &job).await?;
    if plan.uploads != 6 || plan.replacements != 4 {
        bail!("Unexpected update plan");
    }
    // Cancellation must preserve the old version and complete recovery using
    // the pool even though its original job's cancellation token is set.
    let token = tokio_util::sync::CancellationToken::new();
    let trigger = token.clone();
    let mut cancelled = Job::new(
        "Cancel live transfer",
        &[],
        std::sync::Arc::new(move |progress| {
            if progress.uploaded_bytes > 0 {
                trigger.cancel();
            }
        }),
    );
    cancelled.cancel = token;
    let cancelled_remote = connection::connect_pool(&settings, &secret, &cancelled).await?;
    if deploy::apply(&packs, &cancelled_remote, plan.clone(), &cancelled)
        .await
        .is_ok()
    {
        bail!("Cancelled live SFTP transfer unexpectedly completed");
    }
    if deploy::read_version(&cancelled_remote)
        .await?
        .context("Previous version lost")?
        .version
        != "fixture-old"
        || cancelled_remote.stat(".auram-deploy/lock").await?.is_some()
        || cancelled_remote.read("config/a.toml", 100).await? != b"before"
    {
        bail!("Cancelled live deployment did not restore the previous server");
    }
    cancelled_remote.close().await;
    println!("PASS: cancellation during live uploads preserves the previous server and completes rollback");
    deploy::apply(&packs, &remote, plan, &job).await?;
    if remote.hash("mods/client.jar.client").await? != blob(b"CLIENT")
        || remote.hash("mods/rename-new.jar").await? != blob(b"same")
        || remote.hash("mods/large.jar").await? != blob(&bytes)
    {
        bail!("Live SFTP upload content mismatch");
    }
    if remote.read("server.properties", 100).await? != b"private"
        || remote.read("world/level.dat", 100).await? != b"world"
    {
        bail!("Server-owned data changed");
    }
    let plan = deploy::inspect(&packs, &remote, &job).await?;
    if plan.uploads + plan.removals + plan.renames != 0 {
        bail!("Repeated sync was not idempotent");
    }
    if plan.remote_version.context("Version missing")?.version != "fixture-new" {
        bail!("Remote version was not committed");
    }
    remote.close().await;
    let mut changed = settings.clone();
    changed.fingerprint = "SHA256:wrong-fixture-key".into();
    let error = match connection::connect(&changed, &secret, &job).await {
        Ok(_) => bail!("Changed SSH key was accepted"),
        Err(e) => e,
    };
    if !error
        .downcast_ref::<HostKeyNeeded>()
        .is_some_and(|e| e.changed)
    {
        bail!("Changed host key was not reported");
    }
    if connection::connect(
        &settings,
        &Secret {
            password: "incorrect-fixture".into(),
            ..Default::default()
        },
        &job,
    )
    .await
    .is_ok()
    {
        bail!("Incorrect credentials were accepted");
    }
    if let Ok(path) = std::env::var("AURAM_SFTP_TEST_KEY") {
        settings.authentication = "key".into();
        settings.key_path = path;
        connection::connect(
            &settings,
            &Secret {
                passphrase: "fixture-passphrase".into(),
                ..Default::default()
            },
            &job,
        )
        .await?
        .close()
        .await;
        println!("PASS: encrypted private-key authentication");
    }
    println!("PASS: actual OpenSSH SFTP, host-key pinning/rejection, password authentication/rejection, scoped full/update deploys, disabled jar aliases, twenty separate connections, full directory replacement, streaming upload hashes, nonempty lock exclusion, preserved worlds/properties, version commit and idempotent repeat");
    Ok(())
}
