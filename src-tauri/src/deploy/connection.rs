use super::{
    adaptive::{Retry, Slots, MAX_CONNECTIONS},
    safe_path,
};
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
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
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
    closed: AtomicBool,
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
        nodelay: true,
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
        closed: AtomicBool::new(false),
    })
}
impl SftpRemote {
    pub async fn close(self) {
        self.disconnect().await;
    }
    async fn disconnect(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            // Release the actual SSH connection even if SFTP shutdown stalls.
            let _ = self
                .ssh
                .disconnect(russh::Disconnect::ByApplication, "Auram finished", "en")
                .await;
            let _ = self.session.close().await;
        })
        .await;
    }
    fn path(&self, p: &str) -> Result<String> {
        safe_path(p)?;
        Ok(format!("{}/{p}", self.root))
    }
    async fn guarded_parent(&self, p: &str) -> Result<String> {
        let full = self.path(p)?;
        let Some((parent, _)) = p.rsplit_once('/') else {
            return Ok(full);
        };
        let expected = self.path(parent)?;
        // REALPATH resolves all ancestors in one request, instead of one
        // LSTAT per component for every tiny file. Recheck on every operation;
        // do not cache remote path safety or trust externally mutable aliases.
        match self.session.canonicalize(&expected).await {
            Ok(actual) if actual.trim_end_matches('/') == expected => return Ok(full),
            Ok(_) => bail!("Refusing a remote directory symlink or alias: {parent}"),
            Err(SftpError::Status(s)) if s.status_code == StatusCode::NoSuchFile => {}
            Err(error) => return Err(error.into()),
        }
        // Parents() also examines not-yet-created paths. Validate the existing
        // prefix normally until the first genuinely absent component.
        let mut partial = String::new();
        for piece in parent.split('/') {
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
                Err(error) => return Err(error.into()),
            }
        }
        Ok(full)
    }
    async fn guarded(&self, p: &str) -> Result<String> {
        let full = self.guarded_parent(p).await?;
        match self.session.symlink_metadata(&full).await {
            Ok(meta) if meta.is_symlink() => bail!("Refusing to follow a remote symlink: {p}"),
            Ok(_) => {}
            Err(SftpError::Status(s)) if s.status_code == StatusCode::NoSuchFile => {}
            Err(error) => return Err(error.into()),
        }
        Ok(full)
    }
}
/// Metadata operations use a surviving control connection. Private staged
/// uploads start with twenty transports and retire failed/limited slots.
pub struct SftpPool {
    remotes: Vec<SftpRemote>,
    slots: Slots,
    read_job: Job,
}
impl SftpPool {
    pub fn connection_count(&self) -> usize {
        self.slots.count()
    }
    fn control(&self) -> Result<&SftpRemote> {
        Ok(&self.remotes[self.slots.control()?])
    }
    pub async fn close(self) {
        futures_util::future::join_all(self.remotes.into_iter().map(SftpRemote::close)).await;
    }
}
/// Interpret actual protocol/transport failures. Permission, disk/integrity,
/// authentication and host-key failures must never become concurrency retries.
fn retry_kind(error: &anyhow::Error) -> Retry {
    for cause in error.chain() {
        if let Some(error) = cause.downcast_ref::<SftpError>() {
            if matches!(error, SftpError::Status(s) if s.status_code==StatusCode::PermissionDenied)
            {
                return Retry::Never;
            }
            match error {
                SftpError::Timeout => return Retry::Disconnected,
                SftpError::UnexpectedBehavior(message)
                    if message == "sender dropped"
                        || message.starts_with("SendError:")
                        || message.starts_with("RecvError:") =>
                {
                    return Retry::Disconnected
                }
                SftpError::Status(s)
                    if matches!(
                        s.status_code,
                        StatusCode::ConnectionLost | StatusCode::NoConnection
                    ) =>
                {
                    return Retry::Disconnected
                }
                _ => {}
            }
        }
        if let Some(error) = cause.downcast_ref::<russh::Error>() {
            match error {
                russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::ResourceShortage) => {
                    return Retry::Limit
                }
                russh::Error::HUP
                | russh::Error::Disconnect
                | russh::Error::ConnectionTimeout
                | russh::Error::KeepaliveTimeout
                | russh::Error::InactivityTimeout
                | russh::Error::SendError => return Retry::Disconnected,
                _ => {}
            }
        }
        if let Some(error) = cause.downcast_ref::<std::io::Error>() {
            if matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::TimedOut
            ) {
                return Retry::Disconnected;
            }
        }
    }
    let message = format!("{error:#}").to_ascii_lowercase();
    if [
        "too many connections",
        "too many concurrent connections",
        "connection limit",
        "maximum connections",
        "max connections",
        "too many sessions",
        "too many open files",
        "resource shortage",
        "maxstartups",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
    {
        return Retry::Limit;
    }
    if [
        "broken pipe",
        "connection reset",
        "connection closed",
        "connection lost",
        "channel closed",
        "unexpected eof on stream",
        "senderror:",
        "recverror:",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
    {
        return Retry::Disconnected;
    }
    Retry::Never
}
pub async fn connect_pool(
    settings: &ConnectionSettings,
    secret: &Secret,
    job: &Job,
) -> Result<SftpPool> {
    use futures_util::StreamExt;
    job.progress(
        "sftp-connect",
        "Opening twenty SSH/SFTP connections",
        0,
        MAX_CONNECTIONS as u64,
    );
    let primary = connect(settings, secret, job).await?;
    let root = primary.root.clone();
    let mut remotes = vec![primary];
    let mut missing = MAX_CONNECTIONS - 1;
    for round in 0..3 {
        let parallel = if round == 0 { MAX_CONNECTIONS - 1 } else { 4 };
        let attempts = futures_util::stream::iter(0..missing)
            .map(|_| connect(settings, secret, job))
            .buffer_unordered(parallel)
            .collect::<Vec<_>>()
            .await;
        let mut retry = 0;
        let mut fatal = None;
        for result in attempts {
            match result {
                Ok(remote) if remote.root == root => remotes.push(remote),
                Ok(remote) => {
                    remote.close().await;
                    fatal = Some(anyhow::anyhow!(
                        "SFTP connections resolved different server directories"
                    ));
                }
                Err(error) if error.is::<HostKeyNeeded>() => fatal = Some(error),
                Err(error) => {
                    if retry_kind(&error) != Retry::Never {
                        retry += 1;
                    }
                }
            }
        }
        if let Some(error) = fatal {
            futures_util::future::join_all(remotes.into_iter().map(SftpRemote::close)).await;
            return Err(error);
        }
        if let Err(error) = job.check() {
            futures_util::future::join_all(remotes.into_iter().map(SftpRemote::close)).await;
            return Err(error);
        }
        if retry == 0 || round == 2 {
            break;
        }
        missing = retry;
        job.progress(
            "sftp-connect",
            format!(
                "{} connections accepted; retrying server-limited connections",
                remotes.len()
            ),
            remotes.len() as u64,
            MAX_CONNECTIONS as u64,
        );
        tokio::select! {_=job.cancel.cancelled()=>{futures_util::future::join_all(remotes.into_iter().map(SftpRemote::close)).await;bail!("SFTP connection cancelled");},_=tokio::time::sleep(Duration::from_millis(200*(1<<round)))=>{}}
    }
    job.progress(
        "sftp-connect",
        format!(
            "{} SFTP connections ready for parallel uploads",
            remotes.len()
        ),
        remotes.len() as u64,
        MAX_CONNECTIONS as u64,
    );
    let slots = Slots::new(remotes.len());
    // Rollback reads must remain usable after the operation is cancelled.
    let mut read_job = job.clone();
    read_job.cancel = tokio_util::sync::CancellationToken::new();
    Ok(SftpPool {
        remotes,
        slots,
        read_job,
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
            .symlink_metadata(self.guarded_parent(path).await?)
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
            let full = wait(
                job,
                "validating the upload path",
                Duration::from_secs(30),
                self.guarded(path),
            )
            .await?;
            let mut file = wait(
                job,
                &format!("opening upload {label}"),
                Duration::from_secs(30),
                async { Ok(self.session.create(&full).await?) },
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
            let uploaded = self.session.symlink_metadata(&full).await?;
            if uploaded.is_dir() || uploaded.is_symlink() || uploaded.size != Some(size) {
                bail!("Uploaded file size/type mismatch: {label}");
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
    // Metadata/rollback requests stay independent of the upload cancellation
    // token, so cancellation cannot prevent restoration of the previous pack.
    async fn stat(&self, p: &str) -> Result<Option<Attrs>> {
        self.slots
            .run(
                &self.read_job,
                |index| self.remotes[index].stat(p),
                |index| self.remotes[index].disconnect(),
                retry_kind,
            )
            .await
    }
    async fn list(&self, p: &str) -> Result<Vec<(String, Attrs)>> {
        self.slots
            .run(
                &self.read_job,
                |index| self.remotes[index].list(p),
                |index| self.remotes[index].disconnect(),
                retry_kind,
            )
            .await
    }
    async fn read(&self, p: &str, n: u64) -> Result<Vec<u8>> {
        self.slots
            .run(
                &self.read_job,
                |index| self.remotes[index].read(p, n),
                |index| self.remotes[index].disconnect(),
                retry_kind,
            )
            .await
    }
    async fn hash(&self, p: &str) -> Result<String> {
        self.slots
            .run(
                &self.read_job,
                |index| self.remotes[index].hash(p),
                |index| self.remotes[index].disconnect(),
                retry_kind,
            )
            .await
    }
    async fn upload(&self, local: &Path, p: &str, label: &str, job: &Job) -> Result<()> {
        self.slots
            .run(
                job,
                |index| self.remotes[index].upload(local, p, label, job),
                |index| self.remotes[index].disconnect(),
                retry_kind,
            )
            .await
    }
    async fn write(&self, p: &str, b: &[u8]) -> Result<()> {
        self.control()?.write(p, b).await
    }
    async fn mkdir(&self, p: &str) -> Result<()> {
        self.control()?.mkdir(p).await
    }
    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.control()?.rename(from, to).await
    }
    async fn remove(&self, p: &str) -> Result<()> {
        self.control()?.remove(p).await
    }
    async fn rmdir(&self, p: &str) -> Result<()> {
        self.control()?.rmdir(p).await
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

#[cfg(test)]
mod tests {
    use super::*;
    fn status(code: StatusCode, message: &str) -> anyhow::Error {
        SftpError::Status(russh_sftp::protocol::Status {
            id: 1,
            status_code: code,
            error_message: message.into(),
            language_tag: String::new(),
        })
        .into()
    }
    #[test]
    fn actual_protocol_limits_back_off_but_permissions_and_integrity_errors_do_not() {
        assert_eq!(
            retry_kind(
                &anyhow::Error::from(SftpError::UnexpectedBehavior("sender dropped".into()))
                    .context("validating upload path")
            ),
            Retry::Disconnected
        );
        assert_eq!(
            retry_kind(
                &std::io::Error::other(SftpError::UnexpectedBehavior(
                    "RecvError: channel closed".into()
                ))
                .into()
            ),
            Retry::Disconnected
        );
        assert_eq!(
            retry_kind(&status(
                StatusCode::Failure,
                "Too many concurrent connections"
            )),
            Retry::Limit
        );
        assert_eq!(
            retry_kind(&status(
                StatusCode::PermissionDenied,
                "Too many connections"
            )),
            Retry::Never
        );
        assert_eq!(
            retry_kind(&status(StatusCode::Failure, "No space left on device")),
            Retry::Never
        );
        assert_eq!(
            retry_kind(
                &russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::ResourceShortage)
                    .into()
            ),
            Retry::Limit
        );
        assert_eq!(retry_kind(&russh::Error::HUP.into()), Retry::Disconnected);
        assert_eq!(
            retry_kind(&std::io::Error::from(std::io::ErrorKind::ConnectionReset).into()),
            Retry::Disconnected
        );
        assert_eq!(
            retry_kind(&anyhow::anyhow!("Uploaded file size mismatch")),
            Retry::Never
        );
        assert_eq!(
            retry_kind(
                &HostKeyNeeded {
                    fingerprint: "changed".into(),
                    changed: true
                }
                .into()
            ),
            Retry::Never
        );
    }
}
