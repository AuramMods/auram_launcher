use super::*;
use crate::pack::{
    distribution::{Bundle, Catalog, Change, Envelope, Member, Transition},
    git_transport::tests::Fixture as GitFixture,
};
use axum::{
    extract::{OriginalUri, State},
    routing::get,
    Router,
};
use base64::Engine;
use ed25519_dalek::Signer;
use sha1::Digest;
use std::{
    io::Write,
    sync::{atomic::Ordering, Arc, Mutex},
};

fn sign(document: &serde_json::Value) -> (Envelope, String) {
    // A deterministic, nonsecret fixture key; never used by production signing.
    let key = ed25519_dalek::SigningKey::from_bytes(&[17; 32]);
    let payload = serde_json::to_vec(document).unwrap();
    let prehash = blake2::Blake2b512::digest(&payload);
    let signature = key.sign(&prehash).to_bytes();
    let comment = "Auram fixture";
    let trusted = key
        .sign(&[signature.as_slice(), comment.as_bytes()].concat())
        .to_bytes();
    let id = [1, 2, 3, 4, 5, 6, 7, 8];
    let encoded = |v: &[u8]| base64::engine::general_purpose::STANDARD.encode(v);
    let public = format!(
        "untrusted comment: fixture\n{}\n",
        encoded(&[b"Ed".as_slice(), &id, &key.verifying_key().to_bytes()].concat())
    );
    let text = format!(
        "untrusted comment: fixture\n{}\ntrusted comment: {comment}\n{}\n",
        encoded(&[b"ED".as_slice(), &id, &signature].concat()),
        encoded(&trusted)
    );
    (
        Envelope {
            payload: encoded(&payload),
            signature: text,
        },
        public,
    )
}
#[derive(Default)]
struct Responses {
    files: BTreeMap<String, Vec<u8>>,
    requests: Vec<String>,
    status: BTreeMap<String, u16>,
}
struct Fixture {
    git: GitFixture,
    state: Arc<Mutex<Responses>>,
    server: tokio::task::JoinHandle<()>,
    public: String,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        Self::with_extra(0).await
    }
    async fn with_extra(extra: usize) -> Self {
        let mut git = GitFixture::with_extra(extra).await;
        let state = Arc::new(Mutex::new(Responses::default()));
        let router=Router::new().fallback(get(|State(state):State<Arc<Mutex<Responses>>>,OriginalUri(uri):OriginalUri| async move {
            let mut state=state.lock().unwrap();let path=uri.path().to_owned();state.requests.push(path.clone());
            let status=*state.status.get(&path).unwrap_or(&200);
            if status!=200 {(axum::http::StatusCode::from_u16(status).unwrap(),Vec::new())}
            else {match state.files.get(&path) {Some(body)=>(axum::http::StatusCode::OK,body.clone()),None=>(axum::http::StatusCode::NOT_FOUND,Vec::new())}}
        })).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/releases", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let (_, public) = sign(&serde_json::to_value(&git.catalog).unwrap());
        git.packs.repository.distribution = Some(base.clone());
        git.packs.repository.api = format!("{base}/api");
        git.packs.repository.raw = format!("{base}/raw/");
        git.packs.repository.git = git.git.remote.clone();
        git.packs.repository.pack_key = public.clone();
        Self {
            git,
            state,
            server,
            public,
        }
    }
    fn publish(&self, catalog: &Catalog) {
        let (envelope, public) = sign(&serde_json::to_value(catalog).unwrap());
        assert_eq!(public, self.public);
        let bytes = serde_json::to_vec(&envelope).unwrap();
        let mut state = self.state.lock().unwrap();
        state.files.insert(
            "/releases/latest/download/auram-pack.json".into(),
            bytes.clone(),
        );
        state.files.insert(
            format!("/releases/download/{}/auram-pack.json", catalog.target.name),
            bytes,
        );
    }
    fn bundle(&self, catalog: &mut Catalog, oids: Vec<String>) {
        self.bundle_named(catalog, oids, |oid| format!("objects/{oid}"));
    }
    fn bundle_named(
        &self,
        catalog: &mut Catalog,
        oids: Vec<String>,
        name: impl Fn(&str) -> String,
    ) {
        let mut data = std::io::Cursor::new(Vec::new());
        let mut members = BTreeMap::new();
        {
            let mut archive = zip::ZipWriter::new(&mut data);
            for oid in oids {
                let (_, entry) = catalog.files.iter().find(|(_, e)| e.sha == oid).unwrap();
                let body = self.git.object_bytes(&entry.sha);
                assert_eq!(hex::encode(sha2::Sha256::digest(&body)), entry.sha256);
                archive
                    .start_file(name(&oid), zip::write::SimpleFileOptions::default())
                    .unwrap();
                archive.write_all(&body).unwrap();
                members.insert(
                    oid,
                    Member {
                        size: entry.size,
                        sha256: entry.sha256.clone(),
                    },
                );
            }
            archive.finish().unwrap();
        }
        let bytes = data.into_inner();
        let sha = hex::encode(sha2::Sha256::digest(&bytes));
        let url = format!(
            "https://github.com/AuramMods/Auram/releases/download/{}/pack-{sha}.zip",
            catalog.target.name
        );
        self.state.lock().unwrap().files.insert(
            format!("/releases/download/{}/pack-{sha}.zip", catalog.target.name),
            bytes.clone(),
        );
        for oid in members.keys() {
            catalog.objects.insert(oid.clone(), sha.clone());
        }
        catalog.bundles.insert(
            sha.clone(),
            Bundle {
                url,
                sha256: sha,
                size: bytes.len() as u64,
                members,
            },
        );
    }
    fn baseline(&self, catalog: &Catalog) -> Manifest {
        let mut base = catalog.manifest();
        base.policy = source::CLIENT_POLICY;
        base.tag = PackTag {
            name: "0".into(),
            sha: "a".repeat(40),
        };
        base.files.clear();
        base
    }
    fn linked(&self, catalog: &mut Catalog, base: &Manifest) {
        catalog.history = vec![Transition {
            target: catalog.target.clone(),
            base: Some(base.tag.clone()),
            changes: catalog
                .files
                .iter()
                .map(|(p, e)| {
                    (
                        p.clone(),
                        Change {
                            before: None,
                            after: Some(e.clone()),
                        },
                    )
                })
                .collect(),
            pack_ignore_before: Some(String::new()),
        }];
    }
}

#[tokio::test]
async fn signed_archive_with_invalid_member_fails_without_git_fallback_or_activation() {
    let f = Fixture::new().await;
    let mut catalog = f.git.catalog.clone();
    let base = f.baseline(&catalog);
    f.linked(&mut catalog, &base);
    let oids = catalog
        .files
        .values()
        .map(|entry| entry.sha.clone())
        .collect();
    f.bundle_named(&mut catalog, oids, |oid| format!("../{oid}"));
    f.publish(&catalog);
    let packs = &f.git.packs;
    let root = packs.layout.client();
    storage::write_json(&root.join(".auram-manifest.json"), &base).unwrap();
    storage::atomic_write(&root.join("minecraft/saves/world/level.dat"), b"original").unwrap();
    let error = packs.update(false, &Job::default()).await.unwrap_err();
    assert!(format!("{error:#}").contains("archive") || format!("{error:#}").contains("member"));
    assert_eq!(packs.installed().unwrap().unwrap().tag.sha, base.tag.sha);
    assert_eq!(
        fs::read(root.join("minecraft/saves/world/level.dat")).unwrap(),
        b"original"
    );
    assert_eq!(f.git.requests.load(Ordering::SeqCst), 0);
    assert!(f
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .all(|path| !path.contains("/raw/")));
}

#[tokio::test]
async fn signed_discovery_and_partial_update_use_no_git_or_rest_and_repeat_downloads_zero() {
    let f = Fixture::new().await;
    let mut catalog = f.git.catalog.clone();
    let base = f.baseline(&catalog);
    f.linked(&mut catalog, &base);
    let oids = catalog.files.values().map(|e| e.sha.clone()).collect();
    f.bundle(&mut catalog, oids);
    f.publish(&catalog);
    let packs = &f.git.packs;
    let job = Job::default();
    let target = packs.source().discover(&job).await.unwrap();
    assert_eq!(target.sha, catalog.target.sha);
    let (manifest, _) = packs
        .source()
        .client_target(target, Some(&base), false, &job)
        .await
        .unwrap();
    packs
        .reconcile(
            &packs.layout.client(),
            &packs.layout.state("test-transaction"),
            Some(&base),
            &manifest,
            &job,
            false,
        )
        .await
        .unwrap();
    let first = packs.net.transferred_bytes();
    assert!(first > 0);
    assert_eq!(f.git.requests.load(Ordering::SeqCst), 0);
    let installed = packs.installed().unwrap().unwrap();
    packs
        .reconcile(
            &packs.layout.client(),
            &packs.layout.state("test-transaction"),
            Some(&installed),
            &manifest,
            &job,
            false,
        )
        .await
        .unwrap();
    assert_eq!(packs.net.transferred_bytes(), first);
    let requests = &f.state.lock().unwrap().requests;
    assert!(requests.iter().all(|p| p.starts_with("/releases/")));
    assert_eq!(requests.iter().filter(|p| p.ends_with(".zip")).count(), 1);
    assert_eq!(
        fs::read(packs.layout.game().join("config/a.toml")).unwrap(),
        b"enabled=true\n"
    );
}

#[tokio::test]
async fn missing_distributions_are_cached_but_permission_and_signature_errors_do_not_downgrade() {
    let f = Fixture::new().await;
    let packs = &f.git.packs;
    let job = Job::default();
    assert!(distribution_source::named(packs, "7", &job)
        .await
        .unwrap()
        .is_none());
    assert!(distribution_source::named(packs, "7", &job)
        .await
        .unwrap()
        .is_none());
    assert_eq!(f.state.lock().unwrap().requests.len(), 1);
    f.state
        .lock()
        .unwrap()
        .status
        .insert("/releases/download/8/auram-pack.json".into(), 403);
    assert!(distribution_source::named(packs, "8", &job).await.is_err());
    f.publish(&f.git.catalog);
    let path = "/releases/latest/download/auram-pack.json";
    {
        let mut state = f.state.lock().unwrap();
        let mut envelope: Envelope = serde_json::from_slice(&state.files[path]).unwrap();
        envelope.payload.push('A');
        state
            .files
            .insert(path.into(), serde_json::to_vec(&envelope).unwrap());
    }
    assert!(packs.source().discover(&job).await.is_err());
    assert!(f
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .all(|p| p.starts_with("/releases/")));
}

#[tokio::test]
async fn unsupported_signed_catalog_reports_compatibility_without_rest_fallback() {
    let f = Fixture::new().await;
    let mut future = f.git.catalog.clone();
    future.schema = 100;
    f.publish(&future);
    let error = f
        .git
        .packs
        .source()
        .discover(&Job::default())
        .await
        .unwrap_err();
    assert!(error
        .downcast_ref::<distribution::UnsupportedDistribution>()
        .is_some());
    assert_eq!(f.state.lock().unwrap().requests.len(), 1);
}

#[tokio::test]
async fn runtime_only_reconciliation_never_replaces_managed_pack_folders() {
    let f = Fixture::new().await;
    let catalog = &f.git.catalog;
    f.publish(catalog);
    let packs = &f.git.packs;
    let root = packs.layout.server();
    fs::create_dir_all(root.join("mods")).unwrap();
    fs::write(root.join("mods/custom.jar"), b"preserve").unwrap();
    let file = &catalog.files["mmc-pack.json"];
    storage::atomic_write(
        &packs.layout.root.join("cache/blobs").join(&file.sha),
        b"{}\n",
    )
    .unwrap();
    let mut runtime = catalog.manifest();
    runtime.files = BTreeMap::from([("run.sh".into(), file.file())]);
    let job = Job::default();
    packs
        .reconcile_server_runtime(
            &root,
            &packs.layout.state("runtime-only"),
            None,
            &runtime,
            &job,
        )
        .await
        .unwrap();
    assert_eq!(fs::read(root.join("mods/custom.jar")).unwrap(), b"preserve");
    assert_eq!(fs::read(root.join("run.sh")).unwrap(), b"{}\n");
    assert_eq!(f.git.requests.load(Ordering::SeqCst), 0);
    assert!(f.state.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn fresh_signed_install_uses_native_sparse_git_instead_of_delta_archives() {
    let f = Fixture::new().await;
    let mut catalog = f.git.catalog.clone();
    let oids = catalog.files.values().map(|e| e.sha.clone()).collect();
    f.bundle(&mut catalog, oids);
    f.publish(&catalog);
    let packs = &f.git.packs;
    let job = Job::default();
    let target = packs.source().discover(&job).await.unwrap();
    let (manifest, _) = packs
        .source()
        .client_target(target, None, false, &job)
        .await
        .unwrap();
    packs
        .reconcile(
            &packs.layout.client(),
            &packs.layout.state("test-transaction"),
            None,
            &manifest,
            &job,
            false,
        )
        .await
        .unwrap();
    assert_eq!(f.git.requests.load(Ordering::SeqCst), 2);
    assert!(!f
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .any(|p| p.ends_with(".zip")));
}

#[tokio::test]
async fn eight_missing_payloads_use_bundles_and_nine_replace_only_four_folders_with_backups() {
    for required in [8, 9] {
        let f = Fixture::with_extra(6).await;
        let mut catalog = f.git.catalog.clone();
        let base = f.baseline(&catalog);
        f.linked(&mut catalog, &base);
        let selection = distribution_source::selection(&catalog).unwrap();
        let oids = catalog
            .files
            .iter()
            .filter(|(path, _)| selection.includes(path, false))
            .map(|(_, f)| f.sha.clone())
            .collect::<Vec<_>>();
        assert_eq!(oids.len(), 9);
        for oid in oids {
            f.bundle(&mut catalog, vec![oid]);
        }
        let packs = &f.git.packs;
        if required == 8 {
            let sha = &catalog.objects[&catalog.files["mmc-pack.json"].sha];
            let body = f.state.lock().unwrap().files
                [&format!("/releases/download/{}/pack-{sha}.zip", catalog.target.name)]
                .clone();
            storage::atomic_write(
                &packs
                    .layout
                    .root
                    .join("cache/pack-bundles")
                    .join(format!("{sha}.zip")),
                &body,
            )
            .unwrap();
        }
        f.publish(&catalog);
        let custom = packs.layout.game().join("mods/custom.jar");
        storage::atomic_write(&custom, b"custom mod").unwrap();
        let custom_config = packs.layout.game().join("config/custom.toml");
        storage::atomic_write(&custom_config, b"custom config").unwrap();
        for path in [
            "saves/world/level.dat",
            "options.txt",
            "server.properties",
            "resourcepacks/user.zip",
            "shaderpacks/user.zip",
        ] {
            storage::atomic_write(&packs.layout.game().join(path), b"user data").unwrap();
        }
        let job = Job::default();
        let target = packs.source().discover(&job).await.unwrap();
        let (manifest, _) = packs
            .source()
            .client_target(target, Some(&base), false, &job)
            .await
            .unwrap();
        packs
            .reconcile(
                &packs.layout.client(),
                &packs.layout.state("boundary-tx"),
                Some(&base),
                &manifest,
                &job,
                false,
            )
            .await
            .unwrap();
        let requests = &f.state.lock().unwrap().requests;
        if required == 8 {
            assert_eq!(requests.iter().filter(|p| p.ends_with(".zip")).count(), 8);
            assert_eq!(f.git.requests.load(Ordering::SeqCst), 0);
            assert_eq!(fs::read(&custom).unwrap(), b"custom mod");
            assert_eq!(fs::read(&custom_config).unwrap(), b"custom config");
        } else {
            assert!(!requests.iter().any(|p| p.ends_with(".zip")));
            assert_eq!(f.git.requests.load(Ordering::SeqCst), 2);
            assert!(!custom.exists());
            assert!(!custom_config.exists());
            let backup = walkdir::WalkDir::new(packs.layout.client().join(".auram-backups"))
                .into_iter()
                .filter_map(Result::ok)
                .find(|e| e.file_name() == "custom.jar")
                .unwrap();
            assert_eq!(fs::read(backup.path()).unwrap(), b"custom mod");
        }
        for path in [
            "saves/world/level.dat",
            "options.txt",
            "server.properties",
            "resourcepacks/user.zip",
            "shaderpacks/user.zip",
        ] {
            assert_eq!(
                fs::read(packs.layout.game().join(path)).unwrap(),
                b"user data"
            );
        }
        assert_eq!(
            packs.installed().unwrap().unwrap().tag.sha,
            catalog.target.sha
        );
        assert!(!packs
            .layout
            .client()
            .join(".auram-replacement.json")
            .exists());
    }
}

#[tokio::test]
async fn absent_declared_bundle_uses_signed_pinned_http_without_resetting_user_files() {
    let f = Fixture::new().await;
    let mut catalog = f.git.catalog.clone();
    let base = f.baseline(&catalog);
    f.linked(&mut catalog, &base);
    let oids = catalog.files.values().map(|e| e.sha.clone()).collect();
    f.bundle(&mut catalog, oids);
    f.publish(&catalog);
    {
        let mut state = f.state.lock().unwrap();
        state.files.retain(|path, _| !path.ends_with(".zip"));
        for (path, file) in &catalog.files {
            state.files.insert(
                format!("/releases/raw/{}/{path}", catalog.target.sha),
                f.git.object_bytes(&file.sha),
            );
        }
    }
    let packs = &f.git.packs;
    let custom = packs.layout.game().join("mods/custom.jar");
    storage::atomic_write(&custom, b"custom").unwrap();
    let job = Job::default();
    let target = packs.source().discover(&job).await.unwrap();
    let (manifest, _) = packs
        .source()
        .client_target(target, Some(&base), false, &job)
        .await
        .unwrap();
    packs
        .reconcile(
            &packs.layout.client(),
            &packs.layout.state("absent-tx"),
            Some(&base),
            &manifest,
            &job,
            false,
        )
        .await
        .unwrap();
    assert_eq!(fs::read(custom).unwrap(), b"custom");
    assert_eq!(f.git.requests.load(Ordering::SeqCst), 0);
    assert!(f
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .all(|p| p.starts_with("/releases/") && !p.contains("/api/")));
}

#[tokio::test]
async fn server_baseline_and_jar_rename_are_reconstructed_from_signed_history_without_rest() {
    let f = Fixture::new().await;
    let mut catalog = f.git.catalog.clone();
    let old = PackTag {
        name: "0".into(),
        sha: "a".repeat(40),
    };
    let jar = catalog.files["minecraft/mods/a.jar"].clone();
    catalog.history = vec![Transition {
        target: catalog.target.clone(),
        base: Some(old.clone()),
        pack_ignore_before: Some(String::new()),
        changes: BTreeMap::from([
            (
                "minecraft/mods/old.jar".into(),
                Change {
                    before: Some(jar.clone()),
                    after: None,
                },
            ),
            (
                "minecraft/mods/a.jar".into(),
                Change {
                    before: None,
                    after: Some(jar),
                },
            ),
            (
                "minecraft/config/a.toml".into(),
                Change {
                    before: None,
                    after: Some(catalog.files["minecraft/config/a.toml"].clone()),
                },
            ),
        ]),
    }];
    let oids = catalog.files.values().map(|f| f.sha.clone()).collect();
    f.bundle(&mut catalog, oids);
    f.publish(&catalog);
    let packs = &f.git.packs;
    let job = Job::default();
    let target = packs.source().discover(&job).await.unwrap();
    let base = catalog.at(&old).unwrap().manifest();
    let mut client_base = base.clone();
    client_base.policy = source::CLIENT_POLICY;
    let (manifest, _) = packs
        .source()
        .client_target(target.clone(), Some(&client_base), false, &job)
        .await
        .unwrap();
    packs
        .reconcile(
            &packs.layout.client(),
            &packs.layout.state("server-baseline-tx"),
            Some(&client_base),
            &manifest,
            &job,
            false,
        )
        .await
        .unwrap();
    let (server, _, count) = packs
        .server_changelists(&old.name, &old.sha, &target, &job)
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert!(server.files.contains_key("mods/old.jar"));
    assert!(!server.files.contains_key("mods/a.jar"));
    assert_eq!(f.git.requests.load(Ordering::SeqCst), 0);
    assert!(f
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .all(|p| !p.contains("/api/")));
    assert!(packs
        .server_changelists(&old.name, &"f".repeat(40), &target, &job)
        .await
        .is_err());

    // Drive the same version inspection, stage and activate functions used by
    // SFTP, with a disposable directory implementing the destination protocol.
    use crate::{
        deploy,
        server::{ServerConfig, ServerVersion},
    };
    let payload = ServerConfig::default()
        .manifest(&catalog.manifest())
        .unwrap();
    let version = |tag: &PackTag| ServerVersion {
        schema: 1,
        version: tag.name.clone(),
        commit: tag.sha.clone(),
        minecraft: "1.20.1".into(),
        forge: "47.4.10".into(),
    };
    storage::write_json(&packs.layout.state("server-deploy-payload.json"), &payload).unwrap();
    storage::write_json(&packs.layout.server().join("auram.json"), &version(&target)).unwrap();
    let remote = tempfile::tempdir().unwrap();
    let root = remote.path();
    storage::write_json(&root.join("auram.json"), &version(&old)).unwrap();
    let jar = &payload.files["mods/a.jar"];
    storage::atomic_write(
        &root.join("mods/old.jar.client"),
        &f.git.object_bytes(&jar.sha),
    )
    .unwrap();
    storage::atomic_write(&root.join("server.properties"), b"motd=Preserve").unwrap();
    let remote = deploy::local::LocalRemote::new(root.to_owned()).unwrap();
    let plan = deploy::inspect(packs, &remote, &job).await.unwrap();
    assert_eq!((plan.changelists, plan.renames, plan.uploads), (1, 1, 1));
    fs::remove_file(packs.blob_path(&payload.files["config/a.toml"])).unwrap();
    for id in catalog.bundles.keys() {
        fs::remove_file(
            packs
                .layout
                .root
                .join("cache/pack-bundles")
                .join(format!("{id}.zip")),
        )
        .unwrap();
    }
    deploy::apply(packs, &remote, plan, &job).await.unwrap();
    assert!(!root.join("mods/old.jar.client").exists());
    assert_eq!(
        fs::read(root.join("mods/a.jar.client")).unwrap(),
        f.git.object_bytes(&jar.sha)
    );
    assert_eq!(
        fs::read(root.join("config/a.toml")).unwrap(),
        b"enabled=true\n"
    );
    assert_eq!(
        fs::read(root.join("server.properties")).unwrap(),
        b"motd=Preserve"
    );
    assert_eq!(
        deploy::read_version(&remote).await.unwrap().unwrap().commit,
        target.sha
    );
    assert_eq!(f.git.requests.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|path| path.ends_with(".zip"))
            .count(),
        2
    );
}
