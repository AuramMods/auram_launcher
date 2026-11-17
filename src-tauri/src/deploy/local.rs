//! Local transport for the same version-based, recoverable deployment engine.
use super::{
    connection::{Attrs, Remote},
    safe_path,
};
use crate::{
    jobs::{Job, TaskState},
    storage,
};
use anyhow::{bail, Context, Result};
use sha1::Digest;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct LocalRemote {
    root: PathBuf,
}
impl LocalRemote {
    pub fn new(root: PathBuf) -> Result<Self> {
        let root = std::fs::canonicalize(root)?;
        if !root.is_dir() {
            bail!("Local server root is not a directory");
        }
        Ok(Self { root })
    }
    fn path(&self, relative: &str) -> Result<PathBuf> {
        safe_path(relative)?;
        storage::safe_join(&self.root, relative)
    }
}
impl Remote for LocalRemote {
    async fn stat(&self, path: &str) -> Result<Option<Attrs>> {
        match tokio::fs::symlink_metadata(self.path(path)?).await {
            Ok(m) => Ok(Some(Attrs {
                size: m.len(),
                directory: m.is_dir(),
                symlink: m.file_type().is_symlink(),
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    async fn list(&self, path: &str) -> Result<Vec<(String, Attrs)>> {
        let mut directory = tokio::fs::read_dir(self.path(path)?).await?;
        let mut entries = vec![];
        while let Some(entry) = directory.next_entry().await? {
            let m = tokio::fs::symlink_metadata(entry.path()).await?;
            entries.push((
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("Non-UTF8 server filename"))?,
                Attrs {
                    size: m.len(),
                    directory: m.is_dir(),
                    symlink: m.file_type().is_symlink(),
                },
            ));
        }
        Ok(entries)
    }
    async fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>> {
        let mut bytes = vec![];
        tokio::fs::File::open(self.path(path)?)
            .await?
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() as u64 > limit {
            bail!("Server metadata is too large: {path}");
        }
        Ok(bytes)
    }
    async fn hash(&self, path: &str) -> Result<String> {
        let mut file = tokio::fs::File::open(self.path(path)?).await?;
        let size = file.metadata().await?.len();
        let mut hash = sha1::Sha1::new();
        hash.update(format!("blob {size}\0"));
        let mut buffer = vec![0; 65536];
        let mut read = 0;
        loop {
            let count = file.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            read += count as u64;
        }
        if read != size {
            bail!("Server file changed while hashing");
        }
        Ok(hex::encode(hash.finalize()))
    }
    async fn upload(&self, local: &Path, path: &str, label: &str, job: &Job) -> Result<()> {
        let size = tokio::fs::metadata(local).await?.len();
        let mut task = job.task(label, label, Some(size));
        let result = async {
            let mut source = tokio::fs::File::open(local).await?;
            let mut destination = tokio::fs::File::create(self.path(path)?).await?;
            let mut buffer = vec![0; 65536];
            let mut written = 0;
            loop {
                job.check()?;
                let count = source.read(&mut buffer).await?;
                if count == 0 {
                    break;
                }
                destination.write_all(&buffer[..count]).await?;
                written += count as u64;
                task.upload(written, Some(size), count as u64);
            }
            destination.sync_all().await?;
            task.state(TaskState::Verifying, 0);
            if written != size
                || self.stat(path).await?.context("Staged file missing")?.size != size
            {
                bail!("Local deployment file size mismatch: {label}");
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        match &result {
            Ok(()) => task.complete(false),
            Err(error) => task.fail(&format!("{error:#}")),
        }
        result
    }
    async fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
        Ok(tokio::fs::write(self.path(path)?, bytes).await?)
    }
    async fn mkdir(&self, path: &str) -> Result<()> {
        Ok(tokio::fs::create_dir(self.path(path)?).await?)
    }
    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        // Windows rename cannot replace an existing file. Transactions move
        // originals to their retained backup before promoting staged content.
        Ok(tokio::fs::rename(self.path(from)?, self.path(to)?).await?)
    }
    async fn remove(&self, path: &str) -> Result<()> {
        Ok(tokio::fs::remove_file(self.path(path)?).await?)
    }
    async fn rmdir(&self, path: &str) -> Result<()> {
        Ok(tokio::fs::remove_dir(self.path(path)?).await?)
    }
}
