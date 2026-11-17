use crate::{
    jobs::{Job, Progress},
    storage::{self, Layout},
};
use anyhow::{bail, Context, Result};
use futures_util::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use sha1::Digest;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum Checksum {
    Sha1(String),
    Sha256(String),
    GitBlob(String),
}
impl Checksum {
    pub fn verify(&self, path: &Path) -> Result<bool> {
        let mut input = fs::File::open(path)?;
        let mut buffer = [0u8; 65536];
        match self {
            Self::Sha256(expected) => {
                let mut hash = sha2::Sha256::new();
                loop {
                    let count = input.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    hash.update(&buffer[..count]);
                }
                Ok(hex::encode(hash.finalize()).eq_ignore_ascii_case(expected))
            }
            Self::Sha1(expected) | Self::GitBlob(expected) => {
                let mut hash = sha1::Sha1::new();
                if matches!(self, Self::GitBlob(_)) {
                    hash.update(format!("blob {}\0", input.metadata()?.len()).as_bytes());
                }
                loop {
                    let count = input.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    hash.update(&buffer[..count]);
                }
                Ok(hex::encode(hash.finalize()).eq_ignore_ascii_case(expected))
            }
        }
    }
}
#[derive(Clone, Debug)]
pub struct Download {
    pub url: String,
    pub target: PathBuf,
    pub size: Option<u64>,
    pub checksum: Checksum,
}
#[derive(Default, Serialize, Deserialize)]
struct Partial {
    url: String,
    validator: Option<String>,
}
#[derive(Clone)]
pub struct Downloader {
    pub client: reqwest::Client,
    partial_dir: PathBuf,
}
impl Downloader {
    pub fn new(layout: &Layout) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent("AuramLauncher/2 (https://github.com/AuramMods/auram_launcher)")
                .connect_timeout(Duration::from_secs(25))
                .timeout(Duration::from_secs(180))
                .build()?,
            partial_dir: layout.root.join("downloads"),
        })
    }
    pub async fn json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let response = self.client.get(url).send().await?.error_for_status()?;
        let mut data = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            data.extend_from_slice(&chunk?);
            if data.len() > 32 * 1024 * 1024 {
                bail!("Metadata response is too large");
            }
        }
        Ok(serde_json::from_slice(&data)?)
    }
    pub async fn valid(download: &Download) -> Result<bool> {
        let value = download.clone();
        tokio::task::spawn_blocking(move || {
            let meta = match fs::metadata(&value.target) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(e) => return Err(e.into()),
            };
            if value.size.is_some_and(|size| meta.len() != size) {
                return Ok(false);
            }
            value.checksum.verify(&value.target)
        })
        .await?
    }
    pub async fn many(&self, downloads: Vec<Download>, job: &Job) -> Result<()> {
        let mut unique = std::collections::BTreeMap::<PathBuf, Download>::new();
        for download in downloads {
            if let Some(existing) = unique.get(&download.target) {
                if existing.checksum != download.checksum {
                    bail!("Conflicting checksums for one download destination");
                }
            } else {
                unique.insert(download.target.clone(), download);
            }
        }
        let downloads = unique.into_values().collect::<Vec<_>>();
        let total = downloads.len() as u64;
        let counter = std::sync::atomic::AtomicU64::new(0);
        let done = &counter;
        futures_util::stream::iter(downloads.into_iter().map(|download| async move {
            self.get(&download, job).await?;
            let completed = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            job.progress(
                "download",
                format!("Files ready: {completed} / {total}"),
                completed,
                total,
            );
            Ok::<_, anyhow::Error>(())
        }))
        .buffer_unordered(12)
        .try_collect::<Vec<_>>()
        .await?;
        Ok(())
    }
    pub async fn get(&self, download: &Download, job: &Job) -> Result<()> {
        job.check()?;
        if Self::valid(download).await? {
            return Ok(());
        }
        fs::create_dir_all(download.target.parent().context("Download has no parent")?)?;
        let key = hex::encode(sha2::Sha256::digest(
            format!("{}:{:?}", download.url, download.checksum).as_bytes(),
        ));
        let part = self.partial_dir.join(format!("{key}.part"));
        let meta = self.partial_dir.join(format!("{key}.json"));
        for attempt in 0..5 {
            job.check()?;
            let result = self.attempt(download, &part, &meta, job).await;
            match result {
                Ok(()) => return Ok(()),
                Err(error) => {
                    if job.cancel.is_cancelled() {
                        bail!("Operation cancelled");
                    }
                    if let Some(status) = error.downcast_ref::<PermanentStatus>() {
                        bail!("Download failed: HTTP {} ({})", status.0, download.url);
                    }
                    if attempt == 4 {
                        return Err(error).with_context(|| format!("Download {}", download.url));
                    }
                    let delay = error
                        .downcast_ref::<RetryAfter>()
                        .map(|e| e.0)
                        .unwrap_or(Duration::from_millis(400 * (1 << attempt)));
                    tokio::select! {_=job.cancel.cancelled()=>bail!("Operation cancelled"),_=tokio::time::sleep(delay)=>{}}
                }
            }
        }
        unreachable!()
    }
    async fn attempt(
        &self,
        download: &Download,
        part: &Path,
        meta: &Path,
        job: &Job,
    ) -> Result<()> {
        let previous = storage::read_optional::<Partial>(meta)?.unwrap_or_default();
        let offset = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        let mut request = self
            .client
            .get(&download.url)
            .header(reqwest::header::ACCEPT_ENCODING, "identity");
        if offset > 0 && previous.url == download.url {
            request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
            if let Some(value) = &previous.validator {
                request = request.header(reqwest::header::IF_RANGE, value);
            }
        }
        let response = tokio::select! {_=job.cancel.cancelled()=>bail!("Operation cancelled"),response=request.send()=>response?};
        let status = response.status();
        if status.as_u16() == 416 {
            let _ = fs::remove_file(part);
            let _ = fs::remove_file(meta);
            bail!("Restarting unsatisfiable partial download");
        }
        if status.as_u16() == 429 || status.is_server_error() || status.as_u16() == 408 {
            let seconds = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(2)
                .min(60);
            return Err(RetryAfter(Duration::from_secs(seconds)).into());
        }
        if !status.is_success() {
            return Err(PermanentStatus(status.as_u16()).into());
        }
        let append = status.as_u16() == 206 && offset > 0 && previous.url == download.url;
        if status.as_u16() == 206 {
            let content_range = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !append || !content_range.starts_with(&format!("bytes {offset}-")) {
                let _ = fs::remove_file(part);
                bail!("Invalid partial response");
            }
        }
        let validator = response
            .headers()
            .get(reqwest::header::ETAG)
            .filter(|v| !v.as_bytes().starts_with(b"W/"))
            .or_else(|| response.headers().get(reqwest::header::LAST_MODIFIED))
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if append && previous.validator.is_some() && previous.validator != validator {
            let _ = fs::remove_file(part);
            bail!("Download changed while resuming");
        }
        storage::write_json(
            meta,
            &Partial {
                url: download.url.clone(),
                validator,
            },
        )?;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(part)
            .await?;
        let mut received = if append { offset } else { 0 };
        let total = download
            .size
            .or_else(|| response.content_length().map(|size| size + received))
            .unwrap_or(0);
        let mut stream = response.bytes_stream();
        let mut last = Instant::now() - Duration::from_secs(1);
        loop {
            let next = tokio::select! {_=job.cancel.cancelled()=>{file.flush().await?;bail!("Operation cancelled");},next=stream.next()=>next};
            let Some(chunk) = next else {
                break;
            };
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            received += chunk.len() as u64;
            if download.size.is_some_and(|size| received > size) {
                drop(file);
                fs::remove_file(part)?;
                bail!("Download exceeds expected size");
            }
            if last.elapsed() > Duration::from_millis(120) {
                (job.emit)(Progress {
                    operation: "download".into(),
                    message: download
                        .target
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into(),
                    bytes: received,
                    total_bytes: total,
                    ..Default::default()
                });
                last = Instant::now();
            }
        }
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        let pending = Download {
            target: part.to_path_buf(),
            ..download.clone()
        };
        if !Self::valid(&pending).await? {
            fs::remove_file(part)?;
            bail!("Downloaded file failed integrity verification");
        }
        fs::rename(part, &download.target)?;
        let _ = fs::remove_file(meta);
        Ok(())
    }
}
#[derive(Debug)]
struct PermanentStatus(u16);
impl std::fmt::Display for PermanentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}", self.0)
    }
}
impl std::error::Error for PermanentStatus {}
#[derive(Debug)]
struct RetryAfter(Duration);
impl std::fmt::Display for RetryAfter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Server requested retry")
    }
}
impl std::error::Error for RetryAfter {}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::get,
        Router,
    };
    #[test]
    fn git_hash_is_not_plain_sha1() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), b"hello\n").unwrap();
        assert!(
            Checksum::GitBlob("ce013625030ba8dba906f756967f9e9ca394464a".into())
                .verify(temp.path())
                .unwrap()
        );
        assert!(
            !Checksum::Sha1("ce013625030ba8dba906f756967f9e9ca394464a".into())
                .verify(temp.path())
                .unwrap()
        );
    }
    async fn serve(State(mode): State<u8>, headers: HeaderMap) -> impl IntoResponse {
        let range = headers
            .get("range")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if mode == 2 && !range.is_empty() {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                HeaderMap::new(),
                "".to_string(),
            );
        }
        let mut response = HeaderMap::new();
        response.insert("etag", "\"fixture\"".parse().unwrap());
        if mode == 0 && range == "bytes=3-" {
            response.insert("content-range", "bytes 3-5/6".parse().unwrap());
            (StatusCode::PARTIAL_CONTENT, response, "lo\n".to_string())
        } else {
            (StatusCode::OK, response, "hello\n".to_string())
        }
    }
    #[tokio::test]
    async fn resumes_and_handles_ignored_ranges_and_416() {
        for mode in 0..3 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(
                    listener,
                    Router::new().route("/file", get(serve)).with_state(mode),
                )
                .await
                .unwrap();
            });
            let temp = tempfile::tempdir().unwrap();
            let layout = Layout::new(temp.path().into()).unwrap();
            let downloader = Downloader::new(&layout).unwrap();
            let item = Download {
                url: format!("http://{address}/file"),
                target: temp.path().join("target"),
                size: Some(6),
                checksum: Checksum::GitBlob("ce013625030ba8dba906f756967f9e9ca394464a".into()),
            };
            let key = hex::encode(sha2::Sha256::digest(
                format!("{}:{:?}", item.url, item.checksum).as_bytes(),
            ));
            fs::write(downloader.partial_dir.join(format!("{key}.part")), b"hel").unwrap();
            storage::write_json(
                &downloader.partial_dir.join(format!("{key}.json")),
                &Partial {
                    url: item.url.clone(),
                    validator: Some("\"fixture\"".into()),
                },
            )
            .unwrap();
            downloader.get(&item, &Job::default()).await.unwrap();
            assert_eq!(fs::read(&item.target).unwrap(), b"hello\n");
            server.abort();
        }
    }
}
