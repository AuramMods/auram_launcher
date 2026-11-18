use super::*;
use crate::{download::Downloader, pack::PackTag, storage::Layout};
use axum::{body::Bytes, routing::post, Router};
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
        let router = Router::new().route(
            "/git-upload-pack",
            post(move |body: Bytes| {
                let source = source.clone();
                let count = count.clone();
                let recorded = recorded.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    recorded.lock().unwrap().push(body.to_vec());
                    tokio::task::spawn_blocking(move || {
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
                            .env("GIT_PROTOCOL", "version=2")
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
                    .unwrap()
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
