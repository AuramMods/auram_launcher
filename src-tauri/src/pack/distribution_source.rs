use super::{
    distribution::{Catalog, Envelope},
    git_transport::GitCache,
    Manifest, PackTag, Packs,
};
use crate::{
    download::{Checksum, Download, Downloader},
    jobs::Job,
    local_work, storage,
};
use anyhow::{bail, Context, Result};
use sha1::Digest;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
};

fn remember(packs: &Packs, catalog: &Catalog) {
    let mut cache = packs.catalogs.lock().unwrap();
    if cache.len() >= 16 {
        if let Some(first) = cache.keys().next().cloned() {
            cache.remove(&first);
        }
    }
    cache.insert(catalog.target.sha.clone(), catalog.clone());
}
fn verified(packs: &Packs, envelope: &Envelope) -> Result<Catalog> {
    envelope.verify(&packs.repository.pack_key)
}
fn missing(packs: &Packs, name: &str) -> bool {
    packs
        .catalog_misses
        .lock()
        .unwrap()
        .get(name)
        .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(30))
}
fn remember_missing(packs: &Packs, name: &str) {
    let mut misses = packs.catalog_misses.lock().unwrap();
    if misses.len() > 128 {
        misses.clear();
    }
    misses.insert(name.into(), std::time::Instant::now());
}
fn absent(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .and_then(|e| e.status())
        == Some(reqwest::StatusCode::NOT_FOUND)
}
pub(super) async fn latest(packs: &Packs, job: &Job) -> Result<Option<Catalog>> {
    let Some(root) = &packs.repository.distribution else {
        return Ok(None);
    };
    if missing(packs, "latest") {
        return Ok(None);
    }
    let cache = packs.layout.state("pack-distribution-latest.json");
    match packs
        .net
        .json_with_job::<Envelope>(&format!("{root}/latest/download/auram-pack.json"), job)
        .await
    {
        Ok(envelope) => {
            let catalog = verified(packs, &envelope)?;
            storage::write_json(&cache, &envelope)?;
            storage::write_json(
                &packs
                    .layout
                    .state(&format!("pack-distributions/{}.json", catalog.target.sha)),
                &envelope,
            )?;
            remember(packs, &catalog);
            Ok(Some(catalog))
        }
        Err(error) if absent(&error) => {
            remember_missing(packs, "latest");
            Ok(None)
        }
        Err(error) if crate::download::unavailable(&error) => {
            job.check()?;
            if let Some(envelope) = storage::read_optional::<Envelope>(&cache).ok().flatten() {
                let catalog = verified(packs, &envelope)?;
                job.detail(
                    "pack-plan",
                    "Distribution service unavailable; using verified cached metadata",
                );
                remember(packs, &catalog);
                return Ok(Some(catalog));
            }
            Err(error)
        }
        Err(error) => Err(error),
    }
}
pub(super) async fn named(packs: &Packs, name: &str, job: &Job) -> Result<Option<Catalog>> {
    let Some(root) = &packs.repository.distribution else {
        return Ok(None);
    };
    if missing(packs, name) {
        return Ok(None);
    }
    if super::version(name).is_none()
        || !name
            .trim_start_matches('v')
            .bytes()
            .all(|c| c.is_ascii_digit() || c == b'.')
    {
        bail!("Invalid pack distribution version");
    }
    let url = format!("{root}/download/{name}/auram-pack.json");
    match packs.net.json_with_job::<Envelope>(&url, job).await {
        Ok(envelope) => {
            let catalog = verified(packs, &envelope)?;
            if catalog.target.name != name {
                bail!("Pack distribution version disagrees with the selected tag");
            }
            storage::write_json(
                &packs
                    .layout
                    .state(&format!("pack-distributions/{}.json", catalog.target.sha)),
                &envelope,
            )?;
            remember(packs, &catalog);
            Ok(Some(catalog))
        }
        Err(error) if absent(&error) => {
            remember_missing(packs, name);
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
pub(super) async fn catalog(packs: &Packs, tag: &PackTag, job: &Job) -> Result<Option<Catalog>> {
    if packs.repository.distribution.is_none() {
        return Ok(None);
    }
    if let Some(catalog) = packs.catalogs.lock().unwrap().get(&tag.sha).cloned() {
        if catalog.target.name != tag.name {
            bail!("Pack tag identity mismatch");
        }
        return Ok(Some(catalog));
    }
    let cache = packs
        .layout
        .state(&format!("pack-distributions/{}.json", tag.sha));
    let cached = storage::read_optional::<Envelope>(&cache).ok().flatten();
    if let Some(catalog) = cached.as_ref().and_then(|e| verified(packs, e).ok()) {
        if catalog.target.name != tag.name || catalog.target.sha != tag.sha {
            bail!("Cached pack identity mismatch");
        }
        remember(packs, &catalog);
        return Ok(Some(catalog));
    }
    let remote = named(packs, &tag.name, job).await?;
    if let Some(catalog) = &remote {
        if catalog.target.sha != tag.sha {
            bail!("Pack distribution commit disagrees with the pinned target");
        }
    } else if cached.is_some() {
        bail!("Cached pack signature is invalid and no verified replacement is available");
    }
    Ok(remote)
}

pub(super) fn selection(catalog: &Catalog) -> Result<super::source::Selection> {
    let mut builder = ignore::gitignore::GitignoreBuilder::new("");
    for line in catalog.pack_ignore.lines() {
        builder.add_line(None, line)?;
    }
    Ok(super::source::Selection::Client(builder.build()?))
}
fn asset_url(packs: &Packs, url: &str) -> Result<String> {
    // Only internally configured repositories can mirror transport in fixtures;
    // the signed URL itself must still validate as an immutable Auram asset.
    let Some(root) = &packs.repository.distribution else {
        return Ok(url.into());
    };
    let prefix = "https://github.com/AuramMods/Auram/releases";
    Ok(format!(
        "{root}{}",
        url.strip_prefix(prefix)
            .context("Invalid signed bundle source")?
    ))
}
fn bundle_download(packs: &Packs, id: &str, catalog: &Catalog) -> Result<Download> {
    let bundle = &catalog.bundles[id];
    Ok(Download {
        label: Some("Partial pack bundle".into()),
        url: asset_url(packs, &bundle.url)?,
        target: packs
            .layout
            .root
            .join("cache/pack-bundles")
            .join(format!("{id}.zip")),
        size: Some(bundle.size),
        checksum: Checksum::Sha256(id.into()),
    })
}
fn extract(packs: &Packs, id: &str, catalog: &Catalog, job: &Job) -> Result<(u64, u64)> {
    let bundle = &catalog.bundles[id];
    let download = bundle_download(packs, id, catalog)?;
    if !Downloader::valid_local(&download)? {
        bail!("Pack archive failed integrity verification");
    }
    let mut archive = zip::ZipArchive::new(fs::File::open(&download.target)?)?;
    if archive.len() != bundle.members.len() {
        bail!("Pack archive has an unexpected member count");
    }
    let mut seen = BTreeSet::new();
    let mut unpacked = 0u64;
    let maximum = bundle
        .members
        .values()
        .try_fold(0u64, |sum, m| sum.checked_add(m.size))
        .context("Pack archive size overflow")?;
    if maximum >= super::distribution::policy().maximum_bundle_bytes {
        bail!("Pack archive expansion exceeds its bound");
    }
    for index in 0..archive.len() {
        job.check()?;
        let mut member = archive.by_index(index)?;
        let oid = member
            .name()
            .strip_prefix("objects/")
            .context("Unsafe pack archive member")?
            .to_owned();
        let expected = bundle
            .members
            .get(&oid)
            .context("Unknown pack archive member")?;
        if !seen.insert(oid.clone())
            || member.name() != format!("objects/{oid}")
            || member.is_dir()
            || member
                .unix_mode()
                .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o100000))
            || member.size() != expected.size
        {
            bail!("Unsafe/duplicate pack archive member");
        }
        let target = packs.layout.root.join("cache/blobs").join(&oid);
        fs::create_dir_all(target.parent().unwrap())?;
        let pending = target.with_extension(format!("bundle-{}.part", rand::random::<u64>()));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&pending)?;
            let mut sha1 = sha1::Sha1::new();
            sha1.update(format!("blob {}\0", expected.size));
            let mut sha256 = sha2::Sha256::new();
            let mut bytes = 0u64;
            let mut buffer = [0u8; 65536];
            loop {
                job.check()?;
                let count = member.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                bytes = bytes
                    .checked_add(count as u64)
                    .context("Archive member size overflow")?;
                if bytes > expected.size {
                    bail!("Pack archive member exceeds its declared size");
                }
                file.write_all(&buffer[..count])?;
                sha1.update(&buffer[..count]);
                sha256.update(&buffer[..count]);
            }
            if bytes != expected.size
                || hex::encode(sha1.finalize()) != oid
                || hex::encode(sha256.finalize()) != expected.sha256
            {
                bail!("Pack member failed content verification");
            }
            file.flush()?;
            file.sync_all()?;
            drop(file);
            fs::rename(&pending, &target)?;
            Ok::<u64, anyhow::Error>(bytes)
        })();
        let _ = fs::remove_file(pending);
        unpacked = unpacked
            .checked_add(result?)
            .context("Archive size overflow")?;
    }
    Ok((bundle.size, unpacked))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Prepared {
    Legacy,
    Bundles,
    Sparse,
    Http,
}

async fn signed_http(
    packs: &Packs,
    catalog: &Catalog,
    files: &BTreeMap<String, super::distribution::Entry>,
    job: &Job,
) -> Result<()> {
    let downloads = files
        .iter()
        .map(|(path, file)| {
            Ok(Download {
                label: Some(path.clone()),
                url: packs.repository.raw_url(&catalog.target.sha, path)?.into(),
                target: packs.layout.root.join("cache/blobs").join(&file.sha),
                size: Some(file.size),
                checksum: Checksum::Sha256(file.sha256.clone()),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    packs
        .net
        .many_named(downloads, job, "Verified pack files")
        .await?;
    for file in files.values() {
        job.check()?;
        if !Checksum::GitBlob(file.sha.clone())
            .verify(&packs.layout.root.join("cache/blobs").join(&file.sha))?
        {
            bail!("HTTP file disagrees with signed Git identity");
        }
    }
    Ok(())
}
pub(super) async fn sparse(
    packs: &Packs,
    catalog: &Catalog,
    files: &BTreeMap<String, super::distribution::Entry>,
    job: &Job,
) -> Result<()> {
    let git = GitCache {
        directory: packs.layout.root.join("cache/git-native"),
        remote: packs.repository.git.clone(),
    };
    match git.ensure(packs, catalog, files, job).await {
        Ok(()) => Ok(()),
        Err(error)
            if error
                .downcast_ref::<super::git_transport::Unavailable>()
                .is_some()
                || error.downcast_ref::<reqwest::Error>().is_some()
                || crate::download::unavailable(&error) =>
        {
            job.check()?;
            job.detail("pack-files",format!("Git transport unavailable ({error}); retrieving verified files from the same pinned snapshot"));
            signed_http(packs, catalog, files, job).await
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn prepare_replacement(
    packs: &Packs,
    manifest: &Manifest,
    root: &std::path::Path,
    old: Option<&Manifest>,
    server: bool,
    job: &Job,
) -> Result<()> {
    let catalog = catalog(packs, &manifest.tag, job)
        .await?
        .context("Replacement requires a verified target catalog")?;
    let mut wanted = BTreeMap::new();
    for (path, file) in &manifest.files {
        if !super::replacement::inside(path, server) {
            continue;
        }
        let original = file.source.as_deref().unwrap_or(path);
        let entry = catalog
            .files
            .get(original)
            .filter(|e| e.sha == file.sha)
            .context("Replacement entry is not in the signed target")?;
        let item = packs.download(manifest, path, file)?;
        if Downloader::valid(&item).await?
            && Checksum::Sha256(entry.sha256.clone()).verify(&item.target)?
        {
            continue;
        }
        let existing = storage::safe_join(root, path)?;
        if existing.is_file()
            && Checksum::GitBlob(file.sha.clone()).verify(&existing)?
            && Checksum::Sha256(entry.sha256.clone()).verify(&existing)?
        {
            storage::atomic_copy(&existing, &item.target)?;
        } else {
            packs.reuse_owned(root, old, &item, job)?;
        }
        if !Downloader::valid(&item).await?
            || !Checksum::Sha256(entry.sha256.clone()).verify(&item.target)?
        {
            wanted.insert(original.to_owned(), entry.clone());
        }
    }
    sparse(packs, &catalog, &wanted, job).await
}

/// Acquire only final target objects. The caller stages and activates through
/// its existing journal; no live instance file is written by this provider.
pub(super) async fn prepare(
    packs: &Packs,
    manifest: &Manifest,
    downloads: &[Download],
    old: Option<&Manifest>,
    job: &Job,
) -> Result<Prepared> {
    let Some(catalog) = catalog(packs, &manifest.tag, job).await? else {
        return Ok(Prepared::Legacy);
    };
    let mut wanted = BTreeMap::new();
    for download in downloads {
        let Checksum::GitBlob(oid) = &download.checksum else {
            return Ok(Prepared::Legacy);
        };
        let Some((name, entry)) = catalog.files.iter().find(|(_, file)| &file.sha == oid) else {
            return Ok(Prepared::Legacy);
        };
        if Downloader::valid(download).await?
            && Checksum::Sha256(entry.sha256.clone()).verify(&download.target)?
        {
            continue;
        }
        wanted.insert(name.clone(), entry.clone());
    }
    let route = old
        .filter(|m| m.schema == 1 && m.policy == manifest.policy)
        .and_then(|m| catalog.route(&m.tag))
        .is_some();
    let mut ids = BTreeSet::new();
    let mut complete = true;
    for file in wanted.values() {
        if let Some(id) = catalog.objects.get(&file.sha) {
            ids.insert(id.clone());
        } else {
            complete = false;
        }
    }
    let mut required = 0;
    for id in &ids {
        if !Downloader::valid(&bundle_download(packs, id, &catalog)?).await? {
            required += 1;
        }
    }
    if route && complete && required <= super::distribution::policy().maximum_update_bundles {
        job.detail(
            "pack-files",
            format!("Updating through {required} partial pack bundle(s)"),
        );
        let downloads = ids
            .iter()
            .map(|id| bundle_download(packs, id, &catalog))
            .collect::<Result<Vec<_>>>()?;
        if let Err(error) = packs
            .net
            .many_named(downloads, job, "Partial pack bundles")
            .await
        {
            if absent(&error) {
                job.detail(
                    "pack-files",
                    "A partial bundle is absent; retrieving verified target files over HTTP",
                );
                signed_http(packs, &catalog, &wanted, job).await?;
                return Ok(Prepared::Http);
            }
            return Err(error);
        }
        let packs = packs.clone();
        let catalog = catalog.clone();
        let job = job.clone();
        local_work::blocking(move || {
            let progress=local_work::Progress::new(&job,"Verifying partial pack bundles",ids.len());
            let mut archive_bytes=0;let mut unpacked=0;
            for id in ids {let (a,b)=extract(&packs,&id,&catalog,&job)?;archive_bytes+=a;unpacked+=b;progress.complete();}
            storage::write_json(&packs.layout.state("last-pack-transfer.json"),&serde_json::json!({"strategy":"bundles","archiveBytes":archive_bytes,"unpackedBytes":unpacked,"requiredBundles":required}))?;
            Ok(())
        }).await?;
        return Ok(Prepared::Bundles);
    }
    job.detail("pack-files", "Preparing the pinned sparse Git snapshot");
    sparse(packs, &catalog, &wanted, job).await?;
    storage::write_json(
        &packs.layout.state("last-pack-transfer.json"),
        &serde_json::json!({"strategy":"sparse","requiredBundles":required,"route":route,"completePayloads":complete}),
    )?;
    Ok(Prepared::Sparse)
}
