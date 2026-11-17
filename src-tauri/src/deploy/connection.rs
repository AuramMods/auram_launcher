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

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    Ok(SftpRemote {
        session,
        ssh,
        root: root.trim_end_matches('/').into(),
    })
}
impl SftpRemote {
    pub async fn close(self) {
        let _ = self.session.close().await;
        let _ = self
            .ssh
            .disconnect(russh::Disconnect::ByApplication, "Auram finished", "en")
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
            let mut file = self.session.create(self.guarded(path).await?).await?;
            let mut buffer = vec![0u8; 65536];
            let mut sent = 0;
            loop {
                job.check()?;
                let n = source.read(&mut buffer).await?;
                if n == 0 {
                    break;
                }
                file.write_all(&buffer[..n]).await?;
                sent += n as u64;
                task.upload(sent, Some(size), n as u64);
            }
            file.flush().await?;
            file.shutdown().await?;
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
