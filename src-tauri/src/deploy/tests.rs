use super::*;
use crate::{
    download::Downloader,
    jobs::Job,
    pack::{PackFile, PackTag},
    storage::Layout,
};
use anyhow::{bail, Result};
use connection::{Attrs, Remote};
use sha1::Digest;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
};

pub(crate) struct DiskRemote {
    pub root: tempfile::TempDir,
    mutations: AtomicUsize,
    fault: Mutex<Option<(usize, bool, bool)>>,
    offline: AtomicBool,
    pub requests: Mutex<Vec<(String, String)>>,
    slow_uploads: AtomicBool,
    active_uploads: AtomicUsize,
    peak_uploads: AtomicUsize,
}
impl DiskRemote {
    pub fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
            mutations: AtomicUsize::new(0),
            fault: Mutex::new(None),
            offline: AtomicBool::new(false),
            requests: Mutex::new(Vec::new()),
            slow_uploads: AtomicBool::new(false),
            active_uploads: AtomicUsize::new(0),
            peak_uploads: AtomicUsize::new(0),
        }
    }
    fn path(&self, p: &str) -> Result<PathBuf> {
        if self.offline.load(Ordering::SeqCst) {
            bail!("Fixture disconnected");
        }
        safe_path(p)?;
        let mut path = self.root.path().to_path_buf();
        for part in p.split('/') {
            path.push(part);
            if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
                bail!("Fixture refuses symlinks");
            }
        }
        Ok(path)
    }
    fn mutate(&self, operation: impl FnOnce() -> Result<()>) -> Result<()> {
        let n = self.mutations.fetch_add(1, Ordering::SeqCst) + 1;
        let fault = *self.fault.lock().unwrap();
        if fault.is_some_and(|(at, after, _)| at == n && !after) {
            return self.fail(fault.unwrap().2);
        }
        operation()?;
        if fault.is_some_and(|(at, after, _)| at == n && after) {
            return self.fail(fault.unwrap().2);
        }
        Ok(())
    }
    fn fail(&self, offline: bool) -> Result<()> {
        self.offline.store(offline, Ordering::SeqCst);
        bail!("Injected fixture failure")
    }
    pub fn put(&self, p: &str, b: &[u8]) {
        storage::atomic_write(&self.root.path().join(p), b).unwrap();
    }
    pub fn get(&self, p: &str) -> Option<Vec<u8>> {
        fs::read(self.root.path().join(p)).ok()
    }
    fn arm(&self, n: usize, after: bool, offline: bool) {
        self.mutations.store(0, Ordering::SeqCst);
        *self.fault.lock().unwrap() = Some((n, after, offline));
    }
    fn online(&self) {
        self.offline.store(false, Ordering::SeqCst);
        *self.fault.lock().unwrap() = None;
    }
}
impl Remote for DiskRemote {
    async fn stat(&self, p: &str) -> Result<Option<Attrs>> {
        self.requests
            .lock()
            .unwrap()
            .push(("stat".into(), p.into()));
        match fs::symlink_metadata(self.path(p)?) {
            Ok(m) => Ok(Some(Attrs {
                size: m.len(),
                directory: m.is_dir(),
                symlink: m.file_type().is_symlink(),
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    async fn list(&self, p: &str) -> Result<Vec<(String, Attrs)>> {
        self.requests
            .lock()
            .unwrap()
            .push(("list".into(), p.into()));
        fs::read_dir(self.path(p)?)?
            .map(|entry| {
                let entry = entry?;
                let m = entry.file_type()?;
                Ok((
                    entry.file_name().to_string_lossy().into(),
                    Attrs {
                        size: entry.metadata()?.len(),
                        directory: m.is_dir(),
                        symlink: m.is_symlink(),
                    },
                ))
            })
            .collect::<Result<Vec<_>>>()
    }
    async fn read(&self, p: &str, limit: u64) -> Result<Vec<u8>> {
        self.requests
            .lock()
            .unwrap()
            .push(("read".into(), p.into()));
        let b = fs::read(self.path(p)?)?;
        if b.len() as u64 > limit {
            bail!("Fixture file too large");
        }
        Ok(b)
    }
    async fn hash(&self, p: &str) -> Result<String> {
        self.requests
            .lock()
            .unwrap()
            .push(("hash".into(), p.into()));
        Ok(blob(&fs::read(self.path(p)?)?))
    }
    async fn upload(&self, local: &Path, p: &str, label: &str, job: &Job) -> Result<()> {
        job.check()?;
        let b = fs::read(local)?;
        if self.slow_uploads.load(Ordering::SeqCst) {
            let active = self.active_uploads.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak_uploads.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            self.active_uploads.fetch_sub(1, Ordering::SeqCst);
        }
        let mut task = job.task(label, label, Some(b.len() as u64));
        self.mutate(|| Ok(fs::write(self.path(p)?, &b)?))?;
        task.upload(b.len() as u64, Some(b.len() as u64), b.len() as u64);
        task.complete(false);
        Ok(())
    }
    async fn write(&self, p: &str, b: &[u8]) -> Result<()> {
        self.mutate(|| Ok(fs::write(self.path(p)?, b)?))
    }
    async fn mkdir(&self, p: &str) -> Result<()> {
        self.mutate(|| Ok(fs::create_dir(self.path(p)?)?))
    }
    async fn rename(&self, a: &str, b: &str) -> Result<()> {
        self.mutate(|| Ok(fs::rename(self.path(a)?, self.path(b)?)?))
    }
    async fn remove(&self, p: &str) -> Result<()> {
        self.mutate(|| Ok(fs::remove_file(self.path(p)?)?))
    }
    async fn rmdir(&self, p: &str) -> Result<()> {
        self.mutate(|| Ok(fs::remove_dir(self.path(p)?)?))
    }
}
fn blob(bytes: &[u8]) -> String {
    let mut h = sha1::Sha1::new();
    h.update(format!("blob {}\0", bytes.len()));
    h.update(bytes);
    hex::encode(h.finalize())
}
fn version(name: &str) -> ServerVersion {
    ServerVersion {
        schema: 1,
        version: name.into(),
        commit: blob(name.as_bytes()),
        minecraft: "1.20.1".into(),
        forge: "47.4.10".into(),
    }
}
pub(crate) fn payload(packs: &Packs, name: &str, files: &[(&str, &[u8])]) -> Manifest {
    let mut manifest = Manifest {
        schema: 1,
        policy: 0,
        tag: PackTag {
            name: name.into(),
            sha: blob(name.as_bytes()),
        },
        files: BTreeMap::new(),
    };
    for (path, b) in files {
        let file = PackFile {
            sha: blob(b),
            size: b.len() as u64,
            mode: "100644".into(),
            source: Some(format!("minecraft/{path}")),
        };
        storage::atomic_write(&packs.blob_path(&file), b).unwrap();
        manifest.files.insert(path.to_string(), file);
    }
    storage::write_json(&packs.layout.state("server-deploy-payload.json"), &manifest).unwrap();
    storage::write_json(&packs.layout.server().join("auram.json"), &version(name)).unwrap();
    manifest
}
fn packs() -> (tempfile::TempDir, Packs) {
    let t = tempfile::tempdir().unwrap();
    let l = Layout::new(t.path().into()).unwrap();
    let p = Packs::new(l.clone(), Downloader::new(&l).unwrap());
    (t, p)
}
fn baseline(remote: &DiskRemote, manifest: &Manifest, name: &str) {
    let baseline = Baseline {
        schema: 1,
        version: version(name),
        files: manifest
            .files
            .iter()
            .filter(|(p, _)| payload_path(p))
            .map(|(p, f)| {
                (
                    canonical(p),
                    OwnedFile {
                        path: p.clone(),
                        file: f.clone(),
                    },
                )
            })
            .collect(),
    };
    remote.put(
        "auram.json",
        &serde_json::to_vec(&baseline.version).unwrap(),
    );
    remote.put(
        ".auram-deploy.json",
        &serde_json::to_vec(&baseline).unwrap(),
    );
}

#[tokio::test]
async fn unversioned_deploy_replaces_only_four_directories_without_hashing() {
    let (_t, packs) = packs();
    let remote = DiskRemote::new();
    payload(
        &packs,
        "new",
        &[
            ("mods/client.jar", b"client"),
            ("scripts/a.zs", b"script"),
            ("config/a.toml", b"config"),
            ("kubejs/server_scripts/a.js", b"js"),
            ("server.properties", b"wrong"),
            ("world/level.dat", b"wrong"),
            ("defaultconfigs/a", b"wrong"),
        ],
    );
    remote.put("mods/client.jar.client", b"client");
    remote.put("mods/custom.jar", b"custom");
    remote.put("server.properties", b"private");
    remote.put("world/level.dat", b"world");
    let job = Job::default();
    let plan = inspect(&packs, &remote, &job).await.unwrap();
    assert!(plan.remote_version.is_none());
    assert_eq!(plan.reused, 0);
    assert_eq!(plan.uploads, 4);
    assert_eq!(plan.replacements, 4);
    assert!(remote
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|(op, p)| op != "hash" && (op != "list" || p == "mods")));
    assert_eq!(plan.removals, 0);
    apply(&packs, &remote, plan, &job).await.unwrap();
    assert_eq!(remote.get("mods/client.jar.client").unwrap(), b"client");
    assert!(remote.get("mods/client.jar").is_none());
    assert!(remote.get("mods/custom.jar").is_none());
    assert!(
        walkdir::WalkDir::new(remote.root.path().join(".auram-deploy/transactions"))
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .any(|e| fs::read(e.path()).unwrap() == b"custom")
    );
    assert_eq!(remote.get("server.properties").unwrap(), b"private");
    assert_eq!(remote.get("world/level.dat").unwrap(), b"world");
    assert!(remote.get("defaultconfigs/a").is_none());
    assert_eq!(
        serde_json::from_slice::<ServerVersion>(&remote.get("auram.json").unwrap())
            .unwrap()
            .version,
        "new"
    );
    assert!(!remote.root.path().join(".auram-deploy/lock").exists());
    assert!(job.snapshot().uploaded_bytes > 0);
}

#[tokio::test]
async fn missing_changelist_route_replaces_custom_configuration_and_retains_backups() {
    let (_t, packs) = packs();
    let remote = DiskRemote::new();
    let old = payload(
        &packs,
        "old",
        &[
            ("mods/a.jar", b"aaaa"),
            ("mods/renamed-old.jar", b"same"),
            ("mods/obsolete.jar", b"old"),
            ("config/custom.toml", b"defaults"),
            ("config/changed.toml", b"before"),
            ("scripts/obsolete.zs", b"original"),
        ],
    );
    for (path, b) in [
        ("mods/a.jar.client", b"aaaa".as_slice()),
        ("mods/renamed-old.jar.client", b"same"),
        ("mods/obsolete.jar.client", b"old"),
        ("config/custom.toml", b"custom local value"),
        ("config/changed.toml", b"user edited"),
        ("scripts/obsolete.zs", b"local edit"),
    ] {
        remote.put(path, b);
    }
    baseline(&remote, &old, "old");
    payload(
        &packs,
        "new",
        &[
            ("mods/a.jar", b"bbbb"),
            ("mods/renamed-new.jar", b"same"),
            ("config/custom.toml", b"defaults"),
            ("config/changed.toml", b"after!"),
        ],
    );
    let job = Job::default();
    let plan = inspect(&packs, &remote, &job).await.unwrap();
    assert_eq!(plan.renames, 0);
    assert_eq!(plan.removals, 0);
    assert_eq!(plan.uploads, 4);
    assert_eq!(plan.reused, 0);
    assert_eq!(plan.replacements, 4);
    assert!(plan
        .notes
        .iter()
        .any(|note| note.contains("No usable changelist route")));
    apply(&packs, &remote, plan, &job).await.unwrap();
    assert_eq!(remote.get("mods/a.jar.client").unwrap(), b"bbbb");
    assert!(remote.get("mods/a.jar").is_none());
    assert_eq!(remote.get("mods/renamed-new.jar").unwrap(), b"same");
    assert!(remote.get("mods/renamed-old.jar.client").is_none());
    assert!(remote.get("mods/obsolete.jar.client").is_none());
    assert_eq!(remote.get("config/custom.toml").unwrap(), b"defaults");
    assert_eq!(remote.get("config/changed.toml").unwrap(), b"after!");
    assert!(remote.get("scripts/obsolete.zs").is_none());
    let backups = walkdir::WalkDir::new(remote.root.path().join(".auram-deploy/transactions"))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| fs::read(e.path()).unwrap())
        .collect::<Vec<_>>();
    assert!(backups.iter().any(|b| b == b"user edited"));
    assert!(backups.iter().any(|b| b == b"old"));
}

async fn transaction_fixture() -> (tempfile::TempDir, Packs, DiskRemote, DeploymentPlan) {
    let (t, packs) = packs();
    let remote = DiskRemote::new();
    let old = payload(
        &packs,
        "old",
        &[("mods/old.jar", b"old"), ("config/a.toml", b"before")],
    );
    baseline(&remote, &old, "old");
    remote.put("mods/old.jar", b"old");
    remote.put("config/a.toml", b"before");
    remote.put("world/level.dat", b"world");
    remote.put("server.properties", b"private");
    payload(
        &packs,
        "new",
        &[("mods/new.jar", b"new"), ("config/a.toml", b"after")],
    );
    let plan = inspect(&packs, &remote, &Job::default()).await.unwrap();
    (t, packs, remote, plan)
}
#[tokio::test]
async fn every_remote_mutation_can_be_interrupted_before_or_after_acknowledgement() {
    let (_t, packs, remote, plan) = transaction_fixture().await;
    apply(&packs, &remote, plan, &Job::default()).await.unwrap();
    let count = remote.mutations.load(Ordering::SeqCst);
    for after in [false, true] {
        for n in 1..=count {
            let (_t, packs, remote, plan) = transaction_fixture().await;
            remote.arm(n, after, true);
            let _ = apply(&packs, &remote, plan, &Job::default()).await;
            remote.online();
            recover(&remote, &Job::default())
                .await
                .unwrap_or_else(|e| panic!("cut {n}, after {after}: {e:#}"));
            let version: ServerVersion =
                serde_json::from_slice(&remote.get("auram.json").unwrap()).unwrap();
            match version.version.as_str() {
                "old" => {
                    assert_eq!(remote.get("mods/old.jar").unwrap(), b"old");
                    assert!(remote.get("mods/new.jar").is_none());
                    assert_eq!(remote.get("config/a.toml").unwrap(), b"before");
                }
                "new" => {
                    assert_eq!(remote.get("mods/new.jar").unwrap(), b"new");
                    assert!(remote.get("mods/old.jar").is_none());
                    assert_eq!(remote.get("config/a.toml").unwrap(), b"after");
                }
                _ => panic!("Unexpected version"),
            }
            let owned: Baseline =
                serde_json::from_slice(&remote.get(".auram-deploy.json").unwrap()).unwrap();
            assert_eq!(owned.version.version, version.version);
            assert_eq!(remote.get("server.properties").unwrap(), b"private");
            assert_eq!(remote.get("world/level.dat").unwrap(), b"world");
            assert!(!remote.root.path().join(".auram-deploy/lock").exists());
            recover(&remote, &Job::default()).await.unwrap();
        }
    }
}
#[tokio::test]
async fn cancellation_and_live_lock_leave_the_remote_version_unchanged() {
    let (_t, packs, remote, plan) = transaction_fixture().await;
    let job = Job::default();
    job.cancel.cancel();
    assert!(apply(&packs, &remote, plan.clone(), &job).await.is_err());
    assert_eq!(
        serde_json::from_slice::<ServerVersion>(&remote.get("auram.json").unwrap())
            .unwrap()
            .version,
        "old"
    );
    remote.put(".auram-deploy/lock/journal.json", b"another deployment");
    assert!(apply(&packs, &remote, plan, &Job::default()).await.is_err());
    assert_eq!(
        remote.get(".auram-deploy/lock/journal.json").unwrap(),
        b"another deployment"
    );
}
#[test]
fn paths_and_ownership_cannot_escape_the_four_payload_roots() {
    for p in [
        "../world/a",
        "mods/../../world/a",
        "mods\\a",
        "/mods/a",
        "mods/a:stream",
    ] {
        assert!(safe_path(p).is_err());
    }
    let b = Baseline {
        schema: 1,
        version: version("x"),
        files: BTreeMap::from([(
            "world/a".into(),
            OwnedFile {
                path: "world/a".into(),
                file: PackFile {
                    sha: "a".repeat(40),
                    size: 1,
                    mode: "100644".into(),
                    source: None,
                },
            },
        )]),
    };
    assert!(plan::validate_baseline(&b).is_err());
    assert_eq!(canonical("mods/a.jar.client"), "mods/a.jar");
    assert_eq!(canonical("config/a.jar.client"), "config/a.jar.client");
    let settings = connection::ConnectionSettings::default();
    assert!(!serde_json::to_string(&settings)
        .unwrap()
        .contains("password\":"));
}

#[tokio::test]
async fn same_version_reads_only_version_and_lock_without_listing_or_hashing() {
    let (_temp, packs) = packs();
    let remote = DiskRemote::new();
    let manifest = payload(&packs, "145", &[("config/a.toml", b"default")]);
    baseline(&remote, &manifest, "145");
    remote.put("config/a.toml", b"custom");
    let plan = inspect(&packs, &remote, &Job::default()).await.unwrap();
    assert_eq!(
        plan.uploads + plan.removals + plan.renames + plan.replacements,
        0
    );
    assert_eq!(plan.reused, 1);
    let requests = remote.requests.lock().unwrap();
    assert!(requests.iter().all(|(op, path)| op != "list"
        && op != "hash"
        && (path == "auram.json" || path == ".auram-deploy/lock")));
    assert_eq!(remote.get("config/a.toml").unwrap(), b"custom");
}

#[tokio::test]
async fn directory_replacement_schedules_twenty_uploads_and_reports_preparation_and_activation() {
    let (_temp, packs) = packs();
    let remote = DiskRemote::new();
    let names = (0..20)
        .map(|i| format!("config/nested/{i}.toml"))
        .collect::<Vec<_>>();
    let files = names
        .iter()
        .map(|name| (name.as_str(), b"config".as_slice()))
        .collect::<Vec<_>>();
    payload(&packs, "new", &files);
    remote.put("scripts/old.zs", b"old");
    remote.put("kubejs/old.js", b"old");
    remote.slow_uploads.store(true, Ordering::SeqCst);
    let plan = inspect(&packs, &remote, &Job::default()).await.unwrap();
    let snapshots = std::sync::Arc::new(Mutex::new(Vec::new()));
    let sink = snapshots.clone();
    let job = Job::new(
        "Deploying server",
        &[],
        std::sync::Arc::new(move |progress| sink.lock().unwrap().push(progress)),
    );
    apply(&packs, &remote, plan, &job).await.unwrap();
    assert_eq!(remote.peak_uploads.load(Ordering::SeqCst), MAX_CONNECTIONS);
    let snapshots = snapshots.lock().unwrap();
    for id in [
        "sftp-destination",
        "sftp-stage",
        "sftp-validate",
        "sftp-apply",
    ] {
        assert!(
            snapshots
                .iter()
                .any(|snapshot| snapshot.phases.iter().any(|phase| phase.id == id
                    && phase.total > 0
                    && phase.completed == phase.total)),
            "No complete count emitted for {id}"
        );
    }
    let prepared = snapshots
        .iter()
        .position(|snapshot| {
            snapshot.phases.iter().any(|phase| {
                phase.id == "sftp-stage" && phase.total > 0 && phase.completed == phase.total
            })
        })
        .unwrap();
    let uploading = snapshots
        .iter()
        .position(|snapshot| snapshot.uploaded_bytes > 0)
        .unwrap();
    assert!(
        prepared < uploading,
        "Directory preparation must be visible before file uploads"
    );
    assert!(remote.get("scripts/old.zs").is_none());
    assert!(remote.get("kubejs/old.js").is_none());
    assert!(remote.root.path().join("mods").is_dir());
    for name in names {
        assert_eq!(remote.get(&name).unwrap(), b"config");
    }
}

#[tokio::test]
async fn stalled_sftp_requests_timeout_with_context_and_cancel_promptly() {
    use std::time::Duration;
    let job = Job::default();
    let error = connection::wait(
        &job,
        "listing mods",
        Duration::from_millis(10),
        std::future::pending::<Result<()>>(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("listing mods"));
    assert!(error.to_string().contains("timed out"));
    let cancel = job.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel();
    });
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        connection::wait(
            &job,
            "reading auram.json",
            Duration::from_secs(30),
            std::future::pending::<Result<()>>(),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
}

#[tokio::test]
async fn remote_version_accepts_pack_numbers_and_unrecognized_metadata_is_unversioned() {
    let remote = DiskRemote::new();
    assert!(read_version(&remote).await.unwrap().is_none());
    remote.put(
        "auram.json",
        br#"{"version":145,"changelist":"changelist/145.json"}"#,
    );
    let version = read_version(&remote).await.unwrap().unwrap();
    assert_eq!(version.version, "145");
    assert!(version.commit.is_empty());
    for bytes in [
        b"invalid json".as_slice(),
        br#"{"version":false}"#,
        br#"{"version":""}"#,
    ] {
        remote.put("auram.json", bytes);
        assert!(read_version(&remote).await.unwrap().is_none());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn directory_replacement_retains_nested_symlinks_without_following_them() {
    let (_t, packs, remote, plan) = transaction_fixture().await;
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), b"protected").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret"),
        remote.root.path().join("mods/new.jar"),
    )
    .unwrap();
    apply(&packs, &remote, plan, &Job::default()).await.unwrap();
    assert!(
        !fs::symlink_metadata(remote.root.path().join("mods/new.jar"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read(outside.path().join("secret")).unwrap(),
        b"protected"
    );
}
