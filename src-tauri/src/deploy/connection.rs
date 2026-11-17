use super::safe_path;
use crate::{
    jobs::{Job, TaskState},
    storage::{self, Layout},
};
use anyhow::{bail, Context, Result};
use russh::{
    client,
    keys::{self, HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate},
};
use russh_sftp::{
    client::{error::Error as SftpError, SftpSession},
    protocol::StatusCode,
};
use serde::{Deserialize, Serialize};
use sha1::Digest;
use std::{
    future::Future,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ConnectionSettings {
    pub schema: u32,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub directory: String,
    pub authentication: String,
    pub key_path: String,
    pub fingerprint: String,
}
impl Default for ConnectionSettings {
    fn default() -> Self {
        Self {
            schema: 1,
            host: String::new(),
            port: 22,
            username: String::new(),
            directory: ".".into(),
            authentication: "password".into(),
            key_path: String::new(),
            fingerprint: String::new(),
        }
    }
}
#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Secret {
    pub password: String,
    pub passphrase: String,
}
impl ConnectionSettings {
    fn instance_entry(&self, layout: &Layout, id: &str) -> Result<keyring::Entry> {
        Ok(keyring::Entry::new(
            "art.arcane.auram.sftp",
            &self.instance_identity(layout, id),
        )?)
    }
    pub(crate) fn instance_identity(&self, layout: &Layout, id: &str) -> String {
        hex::encode(sha2::Sha256::digest(format!(
            "{}:{id}:{}:{}:{}:{}:{}",
            layout.root.display(),
            self.host,
            self.port,
            self.username,
            self.authentication,
            self.key_path
        )))
    }
    pub fn instance_secret(&self, layout: &Layout, id: &str) -> Result<Secret> {
        match self.instance_entry(layout, id)?.get_password() {
            Ok(value) => Ok(serde_json::from_str(&value)?),
            Err(keyring::Error::NoEntry) if id == "legacy-sftp" => self.secret(layout),
            Err(keyring::Error::NoEntry) => Ok(Secret::default()),
            Err(error) => Err(error.into()),
        }
    }
    pub fn save_instance_secret(&self, layout: &Layout, id: &str, secret: Secret) -> Result<()> {
        Ok(self
            .instance_entry(layout, id)?
            .set_password(&serde_json::to_string(&secret)?)?)
    }
    pub fn forget_instance_secret(&self, layout: &Layout, id: &str) -> Result<()> {
        if id == "legacy-sftp" {
            match self.entry(layout)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(error) => return Err(error.into()),
            }
        }
        match self.instance_entry(layout, id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    fn entry(&self, layout: &Layout) -> Result<keyring::Entry> {
        let identity = hex::encode(sha2::Sha256::digest(format!(
            "{}:{}:{}:{}:{}:{}",
            layout.root.display(),
            self.host,
            self.port,
            self.username,
            self.authentication,
            self.key_path
        )));
        Ok(keyring::Entry::new("art.arcane.auram.sftp", &identity)?)
    }
    pub fn secret(&self, layout: &Layout) -> Result<Secret> {
        match self.entry(layout)?.get_password() {
            Ok(v) => Ok(serde_json::from_str(&v)?),
            Err(keyring::Error::NoEntry) => Ok(Secret::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, layout: &Layout, secret: Option<Secret>) -> Result<()> {
        if let Some(secret) = secret {
            self.entry(layout)?
                .set_password(&serde_json::to_string(&secret)?)?;
        }
        storage::write_json(&layout.state("sftp-connection.json"), self)
    }
    pub fn load(layout: &Layout) -> Result<Self> {
        Ok(storage::read_optional(&layout.state("sftp-connection.json"))?.unwrap_or_default())
    }
    pub fn forget(&self, layout: &Layout) -> Result<()> {
        match self.entry(layout)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => return Err(e.into()),
        }
        match std::fs::remove_file(layout.state("sftp-connection.json")) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyNeeded {
    pub fingerprint: String,
    pub changed: bool,
}
impl std::fmt::Display for HostKeyNeeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Confirm the server's SSH host key: {}", self.fingerprint)
    }
}
impl std::error::Error for HostKeyNeeded {}
struct Handler {
    expected: String,
    observed: Arc<Mutex<Option<String>>>,
}
impl client::Handler for Handler {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool> {
        let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        *self.observed.lock().unwrap_or_else(|e| e.into_inner()) = Some(fingerprint.clone());
        Ok(!self.expected.is_empty() && self.expected == fingerprint)
    }
}
pub struct SftpRemote {
    session: SftpSession,
    ssh: client::Handle<Handler>,
    root: String,
}
pub(crate) async fn wait<T>(
    job: &Job,
    operation: &str,
    timeout: Duration,
    request: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        biased;
        _ = job.cancel.cancelled() => bail!("SFTP cancelled while {operation}"),
        result = tokio::time::timeout(timeout, request) => result
            .with_context(|| format!("SFTP timed out after {} seconds while {operation}", timeout.as_secs()))?
            .with_context(|| format!("SFTP: {operation}")),
    }
}
pub async fn connect(
    settings: &ConnectionSettings,
    secret: &Secret,
    job: &Job,
) -> Result<SftpRemote> {
    let observed = Arc::new(Mutex::new(None));
    let config = client::Config {
        inactivity_timeout: Some(Duration::from_secs(90)),
        keepalive_interval: Some(Duration::from_secs(15)),
        ..Default::default()
    };
    let attempt = tokio::select! {_=job.cancel.cancelled()=>bail!("Connection cancelled"),v=tokio::time::timeout(Duration::from_secs(25),client::connect(Arc::new(config),(settings.host.as_str(),settings.port),Handler {expected:settings.fingerprint.clone(),observed:observed.clone()}))=>v.context("SSH connection timed out")?};
    let key = observed.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(fingerprint) = key {
        if settings.fingerprint != fingerprint {
            return Err(HostKeyNeeded {
                fingerprint,
                changed: !settings.fingerprint.is_empty(),
            }
            .into());
        }
    }
    let mut ssh = attempt.context("Connect to SFTP server")?;
    let auth = async {
        let result = match settings.authentication.as_str() {
            "password" => {
                ssh.authenticate_password(&settings.username, &secret.password)
                    .await?
            }
            "key" => {
                let key = keys::load_secret_key(
                    &settings.key_path,
                    if secret.passphrase.is_empty() {
                        None
                    } else {
                        Some(&secret.passphrase)
                    },
                )
                .context("Read SSH private key")?;
                let algorithm = ssh.best_supported_rsa_hash().await?.flatten();
                ssh.authenticate_publickey(
                    &settings.username,
                    PrivateKeyWithHashAlg::new(Arc::new(key), algorithm),
                )
                .await?
            }
            _ => bail!("Choose password or private-key authentication"),
        };
        if !result.success() {
            bail!("SFTP authentication was rejected. Check the username and credentials.");
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::select! {_=job.cancel.cancelled()=>bail!("Connection cancelled"),v=tokio::time::timeout(Duration::from_secs(30),auth)=>v.context("SSH authentication timed out")??};
    let (session, root) = wait(
        job,
        "opening the SFTP subsystem and server directory",
        Duration::from_secs(30),
        async {
            let channel = ssh.channel_open_session().await?;
            channel
                .request_subsystem(true, "sftp")
                .await
                .context("Open SFTP subsystem")?;
            let session = SftpSession::new(channel.into_stream()).await?;
            session.set_timeout(30);
            let root = session
                .canonicalize(if settings.directory.trim().is_empty() {
                    "."
                } else {
                    &settings.directory
                })
                .await
                .context("Open server directory")?;
            if !session.metadata(&root).await?.is_dir() {
                bail!("The SFTP server path is not a directory");
            }
            Ok((session, root))
        },
    )
    .await?;
    Ok(SftpRemote {
        session,
        ssh,
        root: root.trim_end_matches('/').into(),
    })
}
impl SftpRemote {
    pub async fn close(self) {
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            let _ = self.session.close().await;
            let _ = self
                .ssh
                .disconnect(russh::Disconnect::ByApplication, "Auram finished", "en")
                .await;
        })
        .await;
    }
    fn path(&self, p: &str) -> Result<String> {
        safe_path(p)?;
        Ok(format!("{}/{p}", self.root))
    }
    async fn guarded(&self, p: &str) -> Result<String> {
        let full = self.path(p)?;
        let mut partial = String::new();
        for piece in p.split('/') {
            if !partial.is_empty() {
                partial.push('/');
            }
            partial.push_str(piece);
            match self.session.symlink_metadata(self.path(&partial)?).await {
                Ok(meta) if meta.is_symlink() => {
                    bail!("Refusing to follow a remote symlink: {partial}")
                }
                Ok(_) => {}
                Err(SftpError::Status(s)) if s.status_code == StatusCode::NoSuchFile => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(full)
    }
}
/// Metadata and activation use the primary connection. Independent SSH/SFTP
/// transports carry up to eight file uploads, rather than one shared channel.
pub struct SftpPool {
    remotes: Vec<SftpRemote>,
    slots: Mutex<Vec<usize>>,
    available: tokio::sync::Semaphore,
}
struct UploadSlot<'a> {
    pool: &'a SftpPool,
    index: usize,
    _permit: tokio::sync::SemaphorePermit<'a>,
}
impl Drop for UploadSlot<'_> {
    fn drop(&mut self) {
        self.pool
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(self.index);
    }
}
impl SftpPool {
    pub fn connection_count(&self) -> usize {
        self.remotes.len()
    }
    pub async fn close(self) {
        futures_util::future::join_all(self.remotes.into_iter().map(SftpRemote::close)).await;
    }
}
pub async fn connect_pool(
    settings: &ConnectionSettings,
    secret: &Secret,
    job: &Job,
) -> Result<SftpPool> {
    use futures_util::StreamExt;
    job.progress(
        "sftp-connect",
        "Opening up to eight SSH/SFTP connections",
        0,
        0,
    );
    let primary = connect(settings, secret, job).await?;
    let additional = futures_util::stream::iter(0..7)
        .map(|_| connect(settings, secret, job))
        .buffer_unordered(7)
        .collect::<Vec<_>>()
        .await;
    let mut remotes = vec![primary];
    for result in additional {
        match result {
            Ok(remote) => remotes.push(remote),
            Err(error) if error.downcast_ref::<HostKeyNeeded>().is_some() => {
                futures_util::future::join_all(remotes.into_iter().map(SftpRemote::close)).await;
                return Err(error);
            }
            // Some providers cap simultaneous SSH sessions. Use connections
            // that actually succeeded, rather than guessing their limits.
            Err(_) => {}
        }
    }
    if let Err(error) = job.check() {
        futures_util::future::join_all(remotes.into_iter().map(SftpRemote::close)).await;
        return Err(error);
    }
    job.progress(
        "sftp-connect",
        format!(
            "{} SFTP connections ready for parallel uploads",
            remotes.len()
        ),
        1,
        1,
    );
    let count = remotes.len();
    Ok(SftpPool {
        remotes,
        slots: Mutex::new((0..count).collect()),
        available: tokio::sync::Semaphore::new(count),
    })
}
#[derive(Clone, Debug)]
pub struct Attrs {
    pub size: u64,
    pub directory: bool,
    pub symlink: bool,
}
pub trait Remote: Send + Sync {
    fn stat(&self, path: &str) -> impl Future<Output = Result<Option<Attrs>>> + Send;
    fn list(&self, path: &str) -> impl Future<Output = Result<Vec<(String, Attrs)>>> + Send;
    fn read(&self, path: &str, limit: u64) -> impl Future<Output = Result<Vec<u8>>> + Send;
    fn hash(&self, path: &str) -> impl Future<Output = Result<String>> + Send;
    fn upload(
        &self,
        local: &Path,
        path: &str,
        label: &str,
        job: &Job,
    ) -> impl Future<Output = Result<()>> + Send;
    fn write(&self, path: &str, bytes: &[u8]) -> impl Future<Output = Result<()>> + Send;
    fn mkdir(&self, path: &str) -> impl Future<Output = Result<()>> + Send;
    fn rename(&self, from: &str, to: &str) -> impl Future<Output = Result<()>> + Send;
    fn remove(&self, path: &str) -> impl Future<Output = Result<()>> + Send;
    fn rmdir(&self, path: &str) -> impl Future<Output = Result<()>> + Send;
}
impl Remote for SftpRemote {
    async fn stat(&self, path: &str) -> Result<Option<Attrs>> {
        match self
            .session
            .symlink_metadata(self.guarded(path).await?)
            .await
        {
            Ok(m) => Ok(Some(Attrs {
                size: if m.is_dir() {
                    m.size.unwrap_or(0)
                } else {
                    m.size.context("SFTP server omitted the file size")?
                },
                directory: m.is_dir(),
                symlink: m.is_symlink(),
            })),
            Err(SftpError::Status(s)) if s.status_code == StatusCode::NoSuchFile => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    async fn list(&self, path: &str) -> Result<Vec<(String, Attrs)>> {
        Ok(self
            .session
            .read_dir(self.guarded(path).await?)
            .await?
            .map(|e| {
                let m = e.metadata();
                (
                    e.file_name(),
                    Attrs {
                        size: m.size.unwrap_or(0),
                        directory: m.is_dir(),
                        symlink: m.is_symlink(),
                    },
                )
            })
            .collect())
    }
    async fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>> {
        let mut file = self.session.open(self.guarded(path).await?).await?;
        let mut out = Vec::new();
        (&mut file).take(limit + 1).read_to_end(&mut out).await?;
        file.shutdown().await?;
        if out.len() as u64 > limit {
            bail!("Remote metadata is too large: {path}");
        }
        Ok(out)
    }
    async fn hash(&self, path: &str) -> Result<String> {
        let mut file = self.session.open(self.guarded(path).await?).await?;
        let size = file
            .metadata()
            .await?
            .size
            .context("Remote file size missing")?;
        let mut hash = sha1::Sha1::new();
        hash.update(format!("blob {size}\0"));
        let mut buffer = vec![0u8; 65536];
        let mut count = 0;
        loop {
            let n = file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
            count += n as u64;
        }
        file.shutdown().await?;
        if count != size {
            bail!("Remote file changed while hashing: {path}");
        }
        Ok(hex::encode(hash.finalize()))
    }
    async fn upload(&self, local: &Path, path: &str, label: &str, job: &Job) -> Result<()> {
        let size = tokio::fs::metadata(local).await?.len();
        let mut task = job.task(label, label, Some(size));
        let result = async {
            let mut source = tokio::fs::File::open(local).await?;
            let mut file = wait(
                job,
                &format!("opening upload {label}"),
                Duration::from_secs(30),
                async { Ok(self.session.create(self.guarded(path).await?).await?) },
            )
            .await?;
            let mut buffer = vec![0u8; 65536];
            let mut sent = 0;
            loop {
                job.check()?;
                let n = source.read(&mut buffer).await?;
                if n == 0 {
                    break;
                }
                wait(
                    job,
                    &format!("uploading {label}"),
                    Duration::from_secs(30),
                    async {
                        file.write_all(&buffer[..n]).await?;
                        Ok(())
                    },
                )
                .await?;
                sent += n as u64;
                task.upload(sent, Some(size), n as u64);
            }
            wait(
                job,
                &format!("finishing upload {label}"),
                Duration::from_secs(30),
                async {
                    file.flush().await?;
                    file.shutdown().await?;
                    Ok(())
                },
            )
            .await?;
            task.state(TaskState::Verifying, 0);
            if self
                .stat(path)
                .await?
                .context("Uploaded file missing")?
                .size
                != size
            {
                bail!("Uploaded file size mismatch: {label}");
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        match &result {
            Ok(()) => task.complete(false),
            Err(e) => task.fail(&e.to_string()),
        };
        result
    }
    async fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let mut f = self.session.create(self.guarded(path).await?).await?;
        f.write_all(bytes).await?;
        f.flush().await?;
        f.shutdown().await?;
        Ok(())
    }
    async fn mkdir(&self, path: &str) -> Result<()> {
        self.session.create_dir(self.guarded(path).await?).await?;
        Ok(())
    }
    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.session
            .rename(self.guarded(from).await?, self.guarded(to).await?)
            .await?;
        Ok(())
    }
    async fn remove(&self, path: &str) -> Result<()> {
        self.session.remove_file(self.guarded(path).await?).await?;
        Ok(())
    }
    async fn rmdir(&self, path: &str) -> Result<()> {
        self.session.remove_dir(self.guarded(path).await?).await?;
        Ok(())
    }
}

impl Remote for SftpPool {
    async fn stat(&self, p: &str) -> Result<Option<Attrs>> {
        self.remotes[0].stat(p).await
    }
    async fn list(&self, p: &str) -> Result<Vec<(String, Attrs)>> {
        self.remotes[0].list(p).await
    }
    async fn read(&self, p: &str, n: u64) -> Result<Vec<u8>> {
        self.remotes[0].read(p, n).await
    }
    async fn hash(&self, p: &str) -> Result<String> {
        self.remotes[0].hash(p).await
    }
    async fn upload(&self, local: &Path, p: &str, label: &str, job: &Job) -> Result<()> {
        let permit = tokio::select! {
            biased;
            _=job.cancel.cancelled()=>bail!("SFTP upload cancelled"),
            permit=self.available.acquire()=>permit.context("SFTP upload pool closed")?,
        };
        let index = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop()
            .context("SFTP connection slot unavailable")?;
        let slot = UploadSlot {
            pool: self,
            index,
            _permit: permit,
        };
        self.remotes[slot.index].upload(local, p, label, job).await
    }
    async fn write(&self, p: &str, b: &[u8]) -> Result<()> {
        self.remotes[0].write(p, b).await
    }
    async fn mkdir(&self, p: &str) -> Result<()> {
        self.remotes[0].mkdir(p).await
    }
    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.remotes[0].rename(from, to).await
    }
    async fn remove(&self, p: &str) -> Result<()> {
        self.remotes[0].remove(p).await
    }
    async fn rmdir(&self, p: &str) -> Result<()> {
        self.remotes[0].rmdir(p).await
    }
}

pub async fn parents(remote: &impl Remote, path: &str) -> Result<()> {
    let mut p = String::new();
    let parts = path.split('/').collect::<Vec<_>>();
    for piece in &parts[..parts.len().saturating_sub(1)] {
        if !p.is_empty() {
            p.push('/');
        }
        p.push_str(piece);
        match remote.stat(&p).await? {
            Some(m) if m.directory && !m.symlink => {}
            Some(_) => bail!("A remote file blocks directory {p}"),
            None => remote.mkdir(&p).await?,
        }
    }
    Ok(())
}
