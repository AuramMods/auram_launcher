use super::*;
use crate::{download::Downloader, pack::PackTag, storage::Layout};
use axum::{
    body::Bytes,
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use sha1::Digest;
use std::{
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

pub(crate) struct Fixture {
    _temp: tempfile::TempDir,
    pub(crate) packs: Packs,
    pub(crate) catalog: Catalog,
    pub(crate) git: GitCache,
    pub(crate) requests: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    server: tokio::task::JoinHandle<()>,
    excluded: String,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    pub(crate) async fn new() -> Self {
        Self::with_extra(0).await
    }
    pub(crate) fn object_bytes(&self, oid: &str) -> Vec<u8> {
        let repo = git2::Repository::open_bare(self._temp.path().join("source.git")).unwrap();
        let blob = repo.find_blob(git2::Oid::from_str(oid).unwrap()).unwrap();
        blob.content().to_vec()
    }
    pub(crate) async fn with_extra(extra: usize) -> Self {
        Self::with_options(extra, false).await
    }
    async fn with_options(extra: usize, auth: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.git");
        let repo = git2::Repository::init_bare(&source).unwrap();
        let mut files = BTreeMap::new();
        let mut blobs = BTreeMap::new();
        let mut payloads = vec![
            ("mmc-pack.json", b"{}\n".to_vec()),
            ("server_config.conf", b"clientMods=[]\n".to_vec()),
            ("minecraft/config/a.toml", b"enabled=true\n".to_vec()),
            ("minecraft/mods/a.jar", vec![b'x'; 100_000]),
            ("minecraft/libraries/excluded.jar", vec![b'z'; 2_000_000]),
        ]
        .into_iter()
        .map(|(p, b)| (p.to_owned(), b))
        .collect::<Vec<_>>();
        for index in 0..extra {
            payloads.push((
                format!("minecraft/config/extra-{index}.txt"),
                format!("extra-{index}\n").into_bytes(),
            ));
        }
        for (name, body) in payloads {
            let oid = repo.blob(&body).unwrap();
            blobs.insert(name.clone(), oid);
            if !name.contains("libraries/") {
                files.insert(
                    name.to_owned(),
                    Entry {
                        sha: oid.to_string(),
                        sha256: hex::encode(sha2::Sha256::digest(&body)),
                        size: body.len() as u64,
                        mode: "100644".into(),
                    },
                );
            }
        }
        let make_tree = |entries: Vec<(&str, git2::Oid, i32)>| {
            let mut tree = repo.treebuilder(None).unwrap();
            for (name, id, mode) in entries {
                tree.insert(name, id, mode).unwrap();
            }
            tree.write().unwrap()
        };
        let config = make_tree(
            blobs
                .iter()
                .filter_map(|(p, oid)| {
                    p.strip_prefix("minecraft/config/")
                        .map(|p| (p, *oid, 0o100644))
                })
                .collect(),
        );
        let mods = make_tree(vec![("a.jar", blobs["minecraft/mods/a.jar"], 0o100644)]);
        let libraries = make_tree(vec![(
            "excluded.jar",
            blobs["minecraft/libraries/excluded.jar"],
            0o100644,
        )]);
        let minecraft = make_tree(vec![
            ("config", config, 0o040000),
            ("mods", mods, 0o040000),
            ("libraries", libraries, 0o040000),
        ]);
        let root = make_tree(vec![
            ("mmc-pack.json", blobs["mmc-pack.json"], 0o100644),
            ("server_config.conf", blobs["server_config.conf"], 0o100644),
            ("minecraft", minecraft, 0o040000),
        ]);
        let signature = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
        let commit = repo
            .commit(
                Some("refs/heads/main"),
                &signature,
                &signature,
                "Fixture",
                &repo.find_tree(root).unwrap(),
                &[],
            )
            .unwrap();
        repo.tag_lightweight("1", repo.find_commit(commit).unwrap().as_object(), false)
            .unwrap();
        let catalog = Catalog {
            schema: 1,
            policy: 1,
            repository: "AuramMods/Auram".into(),
            target: PackTag {
                name: "1".into(),
                sha: commit.to_string(),
            },
            producer: commit.to_string(),
            pack_ignore: String::new(),
            files,
            history: vec![],
            bundles: BTreeMap::new(),
            objects: BTreeMap::new(),
        };
        let requests = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let count = requests.clone();
        let recorded = bodies.clone();
        let advertisement_source = source.clone();
        let router = Router::new()
            .route(
                "/info/refs",
                get(move |headers: HeaderMap| {
                    let source = advertisement_source.clone();
                    async move {
                        if let Some(response) = challenge(auth, &headers) {
                            return response;
                        }
                        let body = tokio::task::spawn_blocking(move || {
                            let output = upload_pack(&source)
                                .arg("--advertise-refs")
                                .output()
                                .unwrap();
                            assert!(output.status.success());
                            output.stdout
                        })
                        .await
                        .unwrap();
                        (
                            [(
                                header::CONTENT_TYPE,
                                "application/x-git-upload-pack-advertisement",
                            )],
                            body,
                        )
                            .into_response()
                    }
                }),
            )
            .route(
                "/git-upload-pack",
                post(move |headers: HeaderMap, body: Bytes| {
                    let source = source.clone();
                    let count = count.clone();
                    let recorded = recorded.clone();
                    async move {
                        if let Some(response) = challenge(auth, &headers) {
                            return response;
                        }
                        count.fetch_add(1, Ordering::SeqCst);
                        recorded.lock().unwrap().push(body.to_vec());
                        let body = tokio::task::spawn_blocking(move || {
                            let mut command = upload_pack(&source);
                            command
                                .stdin(Stdio::piped())
                                .stdout(Stdio::piped())
                                .stderr(Stdio::piped());
                            let mut child = command.spawn().unwrap();
                            child.stdin.take().unwrap().write_all(&body).unwrap();
                            let output = child.wait_with_output().unwrap();
                            assert!(
                                output.status.success(),
                                "{}",
                                String::from_utf8_lossy(&output.stderr)
                            );
                            output.stdout
                        })
                        .await
                        .unwrap();
                        (
                            [(header::CONTENT_TYPE, "application/x-git-upload-pack-result")],
                            body,
                        )
                            .into_response()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let layout = Layout::new(temp.path().join("Auram 世界 data")).unwrap();
        let packs = Packs::new(layout.clone(), Downloader::new(&layout).unwrap());
        let git = GitCache {
            directory: layout.root.join("cache/git"),
            remote: format!("http://{address}"),
        };
        Self {
            _temp: temp,
            packs,
            catalog,
            git,
            requests,
            bodies,
            server,
            excluded: blobs["minecraft/libraries/excluded.jar"].to_string(),
        }
    }
}

fn upload_pack(source: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "uploadpack.allowFilter=true",
            "-c",
            "uploadpack.allowAnySHA1InWant=true",
            "upload-pack",
            "--stateless-rpc",
        ])
        .arg(source)
        .env("GIT_PROTOCOL", "version=2");
    command
}

fn challenge(auth: bool, headers: &HeaderMap) -> Option<axum::response::Response> {
    (auth
        && headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            != Some("Basic Zml4dHVyZTpmaXh0dXJlLXBhc3N3b3Jk"))
    .then(|| {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"fixture\"")],
        )
            .into_response()
    })
}

fn executable() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join(if cfg!(windows) { "git.exe" } else { "git" }))
        .find(|path| path.is_absolute() && path.is_file())
        .expect("smart HTTP tests require a local Git executable")
}

#[tokio::test]
async fn configured_git_uses_saved_credentials_in_the_same_sparse_object_cache() {
    let f = Fixture::with_options(0, true).await;
    let repo = repository(&f.git.directory).unwrap();
    repo.config()
        .unwrap()
        .set_str("credential.helper", "store --file=fixture-credentials")
        .unwrap();
    let credentials = f
        .git
        .remote
        .replace("http://", "http://fixture:fixture-password@");
    fs::write(
        f.git.directory.join("fixture-credentials"),
        format!("{credentials}\n"),
    )
    .unwrap();
    let job = Job::default();
    f.git
        .ensure_with(
            &f.packs,
            &f.catalog,
            &f.catalog.files,
            Some(executable()),
            &job,
        )
        .await
        .unwrap();
    assert_eq!(
        f.packs.net.transferred_bytes(),
        0,
        "requests used the external Git client"
    );
    assert!(!repo
        .odb()
        .unwrap()
        .exists(git2::Oid::from_str(&f.excluded).unwrap()));
    let requests = f.requests.load(Ordering::SeqCst);
    assert!(
        requests >= 2,
        "filtered tree and blob requests reached the authenticated fixture"
    );
    f.git
        .ensure_with(&f.packs, &f.catalog, &f.catalog.files, None, &job)
        .await
        .unwrap();
    assert_eq!(f.requests.load(Ordering::SeqCst), requests);
    for file in f.catalog.files.values() {
        assert!(Checksum::Sha256(file.sha256.clone())
            .verify(&f.packs.layout.root.join("cache/blobs").join(&file.sha))
            .unwrap());
    }
}

#[tokio::test]
async fn configured_git_start_failure_uses_bundled_transport_without_changing_target() {
    let f = Fixture::new().await;
    f.git
        .ensure_with(
            &f.packs,
            &f.catalog,
            &f.catalog.files,
            Some(f.git.directory.join("unavailable-git")),
            &Job::default(),
        )
        .await
        .unwrap();
    assert_eq!(f.requests.load(Ordering::SeqCst), 2);
    assert!(f.packs.net.transferred_bytes() > 0);
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_configured_git_does_not_fall_back_or_promote_objects() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new().await;
    let executable = f._temp.path().join("slow-git");
    fs::write(
        &executable,
        b"#!/bin/sh\ntouch git-started\nsleep 120 &\nwait\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let job = Job::default();
    let fetch = f.git.ensure_with(
        &f.packs,
        &f.catalog,
        &f.catalog.files,
        Some(executable),
        &job,
    );
    let cancel = async {
        while !f.git.directory.join("git-started").is_file() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        job.cancel.cancel();
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(fetch, cancel)
    })
    .await
    .unwrap();
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    assert_eq!(f.requests.load(Ordering::SeqCst), 0);
    assert!(fs::read_dir(f.packs.layout.root.join("cache/blobs"))
        .map(|mut entries| entries.next().is_none())
        .unwrap_or_else(|error| error.kind() == std::io::ErrorKind::NotFound));
}

#[tokio::test]
async fn sparse_native_fetch_batches_blobs_reuses_caches_and_omits_runtime_content() {
    let f = Fixture::new().await;
    let job = Job::default();
    f.git
        .ensure(&f.packs, &f.catalog, &f.catalog.files, &job)
        .await
        .unwrap();
    assert_eq!(f.requests.load(Ordering::SeqCst), 2);
    let repo = repository(&f.git.directory).unwrap();
    assert!(!repo
        .odb()
        .unwrap()
        .exists(git2::Oid::from_str(&f.excluded).unwrap()));
    for file in f.catalog.files.values() {
        assert!(Checksum::Sha256(file.sha256.clone())
            .verify(&f.packs.layout.root.join("cache/blobs").join(&file.sha))
            .unwrap());
    }
    f.git
        .ensure(&f.packs, &f.catalog, &f.catalog.files, &job)
        .await
        .unwrap();
    assert_eq!(f.requests.load(Ordering::SeqCst), 2);
    let file = &f.catalog.files["minecraft/config/a.toml"];
    fs::write(
        f.packs.layout.root.join("cache/blobs").join(&file.sha),
        b"damaged",
    )
    .unwrap();
    f.git
        .ensure(&f.packs, &f.catalog, &f.catalog.files, &job)
        .await
        .unwrap();
    assert_eq!(
        f.requests.load(Ordering::SeqCst),
        2,
        "valid native Git objects should repair the raw cache without another network body"
    );
    let bodies = f.bodies.lock().unwrap();
    assert!(String::from_utf8_lossy(&bodies[0]).contains("filter blob:none\n"));
    assert!(!bodies
        .iter()
        .any(|b| String::from_utf8_lossy(b).contains(&f.excluded)));
}

#[test]
fn malformed_or_truncated_git_packets_never_create_valid_objects() {
    let temp = tempfile::tempdir().unwrap();
    let response = temp.path().join("response");
    let directory = temp.path().join("git");
    for data in [
        b"000dpackfile\n0009\x01PACK".as_slice(),
        b"zzzz",
        b"0004",
        b"000dpackfile\n0006\x03x0000",
    ] {
        fs::write(&response, data).unwrap();
        assert!(index_response(&directory, &response, &Job::default()).is_err());
    }
    assert!(fetch_request(&["bad\nwant injected".into()], false).is_err());
}

#[tokio::test]
async fn cancelled_git_fetch_never_promotes_or_changes_an_instance() {
    let f = Fixture::new().await;
    let job = Job::default();
    job.cancel.cancel();
    assert!(f
        .git
        .ensure(&f.packs, &f.catalog, &f.catalog.files, &job)
        .await
        .is_err());
    assert_eq!(f.requests.load(Ordering::SeqCst), 0);
    assert!(fs::read_dir(f.packs.layout.client())
        .unwrap()
        .next()
        .is_none());
}

#[tokio::test]
async fn corrupted_native_pack_is_retained_and_rebuilt_using_verified_blob_caches() {
    let f = Fixture::new().await;
    let job = Job::default();
    f.git
        .ensure(&f.packs, &f.catalog, &f.catalog.files, &job)
        .await
        .unwrap();
    let file = &f.catalog.files["minecraft/config/a.toml"];
    fs::remove_file(f.packs.layout.root.join("cache/blobs").join(&file.sha)).unwrap();
    for entry in fs::read_dir(f.git.directory.join("objects/pack")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "pack") {
            let mut bytes = fs::read(&path).unwrap();
            let end = bytes.len() - 20;
            bytes[12..end].fill(0xff);
            storage::atomic_write(&path, &bytes).unwrap();
        }
    }
    f.git
        .ensure(&f.packs, &f.catalog, &f.catalog.files, &job)
        .await
        .unwrap();
    assert!(Checksum::Sha256(file.sha256.clone())
        .verify(&f.packs.layout.root.join("cache/blobs").join(&file.sha))
        .unwrap());
    assert_eq!(f.requests.load(Ordering::SeqCst), 4);
    assert!(fs::read_dir(f.packs.layout.root.join("cache"))
        .unwrap()
        .any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("git.invalid-")));
}
