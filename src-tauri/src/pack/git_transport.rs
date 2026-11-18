//! Sparse smart-HTTP Git v2 using an embedded native object database. No Git
//! executable, working-tree checkout, shell, hook or credential extraction needed.
use super::{
    distribution::{Catalog, Entry},
    Packs,
};
use crate::{download::Checksum, jobs::Job, local_work, storage};
use anyhow::{bail, Context, Result};
use futures_util::{StreamExt, TryStreamExt};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_OBJECT: u64 = 512 * 1024 * 1024;
const BATCH_BYTES: u64 = 32 * 1024 * 1024;
const EXTRA_BYTES: u64 = 32 * 1024 * 1024;
#[derive(Debug)]
pub struct Unavailable(pub String);
impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for Unavailable {}

#[derive(Clone)]
pub struct GitCache {
    pub directory: PathBuf,
    pub remote: String,
}
struct SystemGit {
    executable: PathBuf,
    disabled: AtomicBool,
}
fn repository(path: &Path) -> Result<git2::Repository> {
    match git2::Repository::open_bare(path) {
        Ok(repo) => Ok(repo),
        Err(error) if error.code() == git2::ErrorCode::NotFound => {
            fs::create_dir_all(path)?;
            let mut options = git2::RepositoryInitOptions::new();
            options.bare(true).external_template(false);
            Ok(git2::Repository::init_opts(path, &options)?)
        }
        Err(error) => Err(error).context("Open the private pack Git cache"),
    }
}
fn packet(body: &mut Vec<u8>, line: &str) {
    body.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
    body.extend_from_slice(line.as_bytes());
}
pub fn fetch_request(oids: &[String], metadata: bool) -> Result<Vec<u8>> {
    if oids.is_empty() || oids.len() > 256 {
        bail!("Invalid Git fetch batch size");
    }
    let mut body = Vec::new();
    packet(&mut body, "command=fetch\n");
    packet(&mut body, "agent=AuramLauncher/2\n");
    body.extend_from_slice(b"0001");
    packet(&mut body, "no-progress\n");
    packet(&mut body, "ofs-delta\n");
    if metadata {
        packet(&mut body, "deepen 1\n");
        packet(&mut body, "filter blob:none\n");
    }
    for oid in oids {
        let parsed = git2::Oid::from_str(oid)?;
        if parsed.to_string() != *oid {
            bail!("Noncanonical Git object ID");
        }
        packet(&mut body, &format!("want {oid}\n"));
    }
    packet(&mut body, "done\n");
    body.extend_from_slice(b"0000");
    Ok(body)
}
/// A complete framed response is indexed atomically by libgit2. Its pack checksum
/// and object/delta identities are checked before objects become reusable.
pub fn index_response(directory: &Path, response: &Path, job: &Job) -> Result<()> {
    let repo = repository(directory)?;
    let odb = repo.odb()?;
    let mut writer = odb.packwriter()?;
    let mut input = fs::File::open(response)?;
    let mut pack = false;
    let mut wrote = false;
    loop {
        job.check()?;
        let mut header = [0u8; 4];
        input
            .read_exact(&mut header)
            .context("Truncated Git packet header")?;
        let size = usize::from_str_radix(std::str::from_utf8(&header)?, 16)
            .context("Invalid Git packet length")?;
        if size == 0 || size == 2 {
            if pack {
                break;
            }
            continue;
        }
        if size == 1 {
            continue;
        }
        if !(5..=65520).contains(&size) {
            bail!("Invalid Git packet size");
        }
        let mut data = vec![0u8; size - 4];
        input
            .read_exact(&mut data)
            .context("Truncated Git packet")?;
        if data.starts_with(b"ERR ") {
            bail!(
                "Git server: {}",
                String::from_utf8_lossy(&data[..data.len().min(1024)])
            );
        }
        if !pack {
            if data == b"packfile\n" {
                pack = true;
            } else if !(data == b"shallow-info\n"
                || data.starts_with(b"shallow ")
                || data.starts_with(b"unshallow ")
                || data == b"acknowledgments\n"
                || data == b"NAK\n"
                || data.starts_with(b"ACK ")
                || data == b"ready\n")
            {
                bail!("Unexpected Git response section");
            }
            continue;
        }
        match data[0] {
            1 => {
                writer.write_all(&data[1..])?;
                wrote = true;
            }
            2 => {}
            3 => bail!(
                "Git transfer failed: {}",
                String::from_utf8_lossy(&data[1..data.len().min(1024)])
            ),
            _ => bail!("Invalid Git sideband channel"),
        }
    }
    if !wrote {
        bail!("Git response did not contain a complete pack");
    }
    let mut extra = [0u8; 1];
    if input.read(&mut extra)? != 0 {
        bail!("Trailing bytes after Git pack response");
    }
    writer.commit().context("Verify and index the Git pack")?;
    Ok(())
}
impl GitCache {
    async fn fetch(
        &self,
        packs: &Packs,
        oids: Vec<String>,
        metadata: bool,
        limit: u64,
        system: Option<&SystemGit>,
        job: &Job,
    ) -> Result<()> {
        job.check()?;
        if let Some(system) = system.filter(|git| !git.disabled.load(Ordering::Relaxed)) {
            match super::system_git::fetch(
                &system.executable,
                &self.directory,
                &self.remote,
                &oids,
                metadata,
                job,
            )
            .await
            {
                Ok(()) => return Ok(()),
                Err(error) => {
                    job.check()?;
                    system.disabled.store(true, Ordering::Relaxed);
                    job.detail(
                        "pack-git",
                        format!("{error}. Using the bundled Git transport."),
                    );
                }
            }
        }
        fs::create_dir_all(&self.directory)?;
        let response = self
            .directory
            .join(format!("incoming-{}.gitpart", rand::random::<u64>()));
        let body = fetch_request(&oids, metadata)?;
        packs
            .net
            .git_post(
                &format!("{}/git-upload-pack", self.remote.trim_end_matches('/')),
                body,
                &response,
                limit,
                job,
            )
            .await?;
        let directory = self.directory.clone();
        let pending = response.clone();
        let running = job.clone();
        let result =
            local_work::blocking(move || index_response(&directory, &pending, &running)).await;
        if result.is_ok() {
            let _ = fs::remove_file(response);
        }
        result
    }
    pub async fn ensure(
        &self,
        packs: &Packs,
        catalog: &Catalog,
        files: &BTreeMap<String, Entry>,
        job: &Job,
    ) -> Result<()> {
        self.ensure_with(packs, catalog, files, super::system_git::configured(), job)
            .await
    }
    async fn ensure_with(
        &self,
        packs: &Packs,
        catalog: &Catalog,
        files: &BTreeMap<String, Entry>,
        executable: Option<PathBuf>,
        job: &Job,
    ) -> Result<()> {
        job.check()?;
        let relative = self
            .directory
            .strip_prefix(&packs.layout.root)
            .context("Git cache must remain inside the launcher data directory")?;
        storage::safe_join(
            &packs.layout.root,
            &relative
                .to_str()
                .context("Git cache path is not UTF-8")?
                .replace('\\', "/"),
        )?;
        let system = executable.map(|executable| SystemGit {
            executable,
            disabled: AtomicBool::new(false),
        });
        match self
            .ensure_once(packs, catalog, files, system.as_ref(), job)
            .await
        {
            Err(error) if error.downcast_ref::<git2::Error>().is_some() => {
                job.check()?;
                if self.directory == packs.layout.root.join("cache")
                    || !self.directory.starts_with(packs.layout.root.join("cache"))
                {
                    return Err(error);
                }
                let quarantine = self
                    .directory
                    .with_extension(format!("invalid-{}", rand::random::<u64>()));
                fs::rename(&self.directory, quarantine)
                    .context("Retain the damaged private Git cache before recovery")?;
                job.detail(
                    "pack-git",
                    "Recovering the Git cache; verified content files remain reusable",
                );
                self.ensure_once(packs, catalog, files, system.as_ref(), job)
                    .await
            }
            result => result,
        }
    }
    async fn ensure_once(
        &self,
        packs: &Packs,
        catalog: &Catalog,
        files: &BTreeMap<String, Entry>,
        system: Option<&SystemGit>,
        job: &Job,
    ) -> Result<()> {
        let path = self.directory.clone();
        let target = catalog.target.sha.clone();
        let exists = local_work::blocking(move || {
            Ok(repository(&path)?
                .odb()?
                .exists(git2::Oid::from_str(&target)?))
        })
        .await?;
        if !exists {
            job.detail(
                "pack-git",
                "Fetching the pinned Git snapshot without historical file contents",
            );
            self.fetch(
                packs,
                vec![catalog.target.sha.clone()],
                true,
                EXTRA_BYTES,
                system,
                job,
            )
            .await?;
        }
        let path = self.directory.clone();
        let target = catalog.target.sha.clone();
        let expected = files.clone();
        local_work::blocking(move || {
            let repo = repository(&path)?;
            let tree = repo.find_commit(git2::Oid::from_str(&target)?)?.tree()?;
            for (name, file) in &expected {
                let entry = tree
                    .get_path(Path::new(name))
                    .with_context(|| format!("Pinned Git snapshot is missing {name}"))?;
                if entry.id().to_string() != file.sha
                    || format!("{:o}", entry.filemode()) != file.mode
                {
                    bail!("Signed inventory disagrees with the pinned Git tree: {name}");
                }
            }
            Ok(())
        })
        .await?;
        let path = self.directory.clone();
        let inspect = files.clone();
        let layout = packs.layout.clone();
        let running = job.clone();
        let missing = local_work::blocking(move || {
            let repo = repository(&path)?;
            let odb = repo.odb()?;
            let mut missing = BTreeMap::new();
            for file in inspect.values() {
                running.check()?;
                let cached = layout.root.join("cache/blobs").join(&file.sha);
                if cached.is_file()
                    && Checksum::GitBlob(file.sha.clone()).verify(&cached)?
                    && Checksum::Sha256(file.sha256.clone()).verify(&cached)?
                {
                    continue;
                }
                if file.size > MAX_OBJECT {
                    return Err(Unavailable(
                        "Selected Git object exceeds the native memory bound".into(),
                    )
                    .into());
                }
                if !odb.exists(git2::Oid::from_str(&file.sha)?) {
                    missing.insert(file.sha.clone(), file.size);
                }
            }
            Ok(missing)
        })
        .await?;
        let mut batches = Vec::new();
        let mut ids = Vec::new();
        let mut bytes = 0;
        for (oid, size) in missing {
            if !ids.is_empty() && (ids.len() == 256 || bytes + size > BATCH_BYTES) {
                batches.push((ids, bytes));
                ids = Vec::new();
                bytes = 0;
            }
            ids.push(oid);
            bytes += size;
        }
        if !ids.is_empty() {
            batches.push((ids, bytes));
        }
        job.detail(
            "pack-git",
            format!("Retrieving {} sparse Git object batches", batches.len()),
        );
        let progress =
            local_work::Progress::new(job, "Retrieving sparse Git batches", batches.len());
        let completed = &progress;
        futures_util::stream::iter(batches.into_iter().map(|(oids, size)| async move {
            self.fetch(packs, oids, false, size + EXTRA_BYTES, system, job)
                .await?;
            completed.complete();
            Ok::<_, anyhow::Error>(())
        }))
        .buffer_unordered(4)
        .try_collect::<Vec<_>>()
        .await?;
        let path = self.directory.clone();
        let extract = files.clone();
        let layout = packs.layout.clone();
        let running = job.clone();
        local_work::blocking(move || {
            let repo = repository(&path)?;
            let odb = repo.odb()?;
            let progress =
                local_work::Progress::new(&running, "Verifying sparse Git content", extract.len());
            for file in extract.values() {
                running.check()?;
                let target = layout.root.join("cache/blobs").join(&file.sha);
                if target.is_file()
                    && Checksum::GitBlob(file.sha.clone()).verify(&target)?
                    && Checksum::Sha256(file.sha256.clone()).verify(&target)?
                {
                    progress.complete();
                    continue;
                }
                let object = odb.read(git2::Oid::from_str(&file.sha)?)?;
                if object.kind() != git2::ObjectType::Blob || object.len() as u64 != file.size {
                    bail!("Git object size/type mismatch");
                }
                // Atomic promotion uses destination-local staging; game-writable
                // files are always private copies made by reconciliation.
                let pending = target.with_extension("git-staging");
                storage::atomic_write(&pending, object.data())?;
                if !Checksum::GitBlob(file.sha.clone()).verify(&pending)?
                    || !Checksum::Sha256(file.sha256.clone()).verify(&pending)?
                {
                    fs::remove_file(&pending)?;
                    bail!("Git object failed signed content verification");
                }
                fs::rename(pending, target)?;
                progress.complete();
            }
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
#[path = "git_transport_tests.rs"]
pub(crate) mod tests;
