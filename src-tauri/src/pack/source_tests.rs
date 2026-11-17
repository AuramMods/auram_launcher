use super::*;
use crate::{
    download::{Checksum, Downloader},
    storage::Layout,
};
use axum::{
    extract::{OriginalUri, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use serde_json::{json, Value};
use sha1::Digest;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Responses {
    bodies: BTreeMap<String, Vec<u8>>,
    requests: Vec<String>,
}
struct Fixture {
    state: Arc<Mutex<Responses>>,
    server: tokio::task::JoinHandle<()>,
    packs: Packs,
    _temp: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(sha1::Sha1::digest(bytes))
}
fn blob(bytes: &[u8]) -> String {
    let mut h = sha1::Sha1::new();
    h.update(format!("blob {}\0", bytes.len()));
    h.update(bytes);
    hex::encode(h.finalize())
}
type Files = BTreeMap<String, Vec<u8>>;
fn files(entries: &[(&str, &str)]) -> Files {
    let mut result = Files::from([
        ("mmc-pack.json".into(), b"{}".to_vec()),
        (".packignore".into(), b"minecraft/ignored\n".to_vec()),
    ]);
    result.extend(
        entries
            .iter()
            .map(|(p, b)| (p.to_string(), b.as_bytes().to_vec())),
    );
    result
}
impl Fixture {
    async fn new() -> Self {
        let state = Arc::new(Mutex::new(Responses::default()));
        async fn serve(
            State(state): State<Arc<Mutex<Responses>>>,
            OriginalUri(uri): OriginalUri,
        ) -> impl IntoResponse {
            let mut state = state.lock().unwrap();
            let path = uri.path_and_query().unwrap().as_str().to_string();
            state.requests.push(path.clone());
            match state.bodies.get(&path) {
                Some(body) => (StatusCode::OK, body.clone()),
                None => (StatusCode::NOT_FOUND, Vec::new()),
            }
        }
        let router = Router::new().fallback(get(serve)).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout::new(temp.path().to_owned()).unwrap();
        let mut packs = Packs::new(layout.clone(), Downloader::new(&layout).unwrap());
        packs.repository = Repository {
            api: format!("http://{addr}/api"),
            raw: format!("http://{addr}/raw/"),
        };
        Self {
            state,
            server,
            packs,
            _temp: temp,
        }
    }
    fn response(&self, path: &str, value: Value) {
        self.state
            .lock()
            .unwrap()
            .bodies
            .insert(path.into(), serde_json::to_vec(&value).unwrap());
    }
    fn snapshot(
        &self,
        name: &str,
        entries: Files,
        previous: Option<&PackTag>,
        changes: Option<Vec<String>>,
    ) -> PackTag {
        let sha = hash(name.as_bytes());
        let content = hash(format!("content-{name}").as_bytes());
        let mut entries = entries;
        if let Some(changes) = changes {
            let n = name.parse::<u64>().unwrap();
            // Exercise metadata path indirection; do not hard-code changelist/<build>.json.
            let path = format!("changelist/build {name}.json");
            entries.insert(
                "auram.json".into(),
                serde_json::to_vec(&json!({"version":n,"changelist":path,"extra":"retained"}))
                    .unwrap(),
            );
            entries.insert(path.clone(),serde_json::to_vec(&json!({"schema":1,"version":n,"previousVersion":previous.map(|p|&p.name),"previousCommit":previous.map(|p|&p.sha),"contentCommit":content,"changes":changes})).unwrap());
        }
        fn tree(f: &Fixture, files: &Files) -> String {
            let mut groups = BTreeMap::<String, Files>::new();
            let mut direct = Vec::new();
            let mut recursive = Vec::new();
            for (path, bytes) in files {
                recursive.push(json!({"path":path,"sha":blob(bytes),"type":"blob","mode":"100644","size":bytes.len()}));
                if let Some((folder, rest)) = path.split_once('/') {
                    groups
                        .entry(folder.into())
                        .or_default()
                        .insert(rest.into(), bytes.clone());
                } else {
                    direct.push(json!({"path":path,"sha":blob(bytes),"type":"blob","mode":"100644","size":bytes.len()}));
                }
            }
            for (folder, files) in groups {
                let sha = tree(f, &files);
                direct.push(
                    json!({"path":folder,"sha":sha,"type":"tree","mode":"040000","size":null}),
                );
            }
            let sha = hash(&serde_json::to_vec(&direct).unwrap());
            f.response(
                &format!("/api/git/trees/{sha}"),
                json!({"tree":direct,"truncated":false}),
            );
            f.response(
                &format!("/api/git/trees/{sha}?recursive=1"),
                json!({"tree":recursive,"truncated":false}),
            );
            sha
        }
        let root = tree(self, &entries);
        let root_body =
            self.state.lock().unwrap().bodies[&format!("/api/git/trees/{root}")].clone();
        self.state
            .lock()
            .unwrap()
            .bodies
            .insert(format!("/api/git/trees/{sha}"), root_body);
        for (path, bytes) in &entries {
            let url = self.packs.repository.raw_url(&sha, path).unwrap();
            self.state
                .lock()
                .unwrap()
                .bodies
                .insert(url.path().into(), bytes.clone());
        }
        let tag_object = hash(format!("annotated-{name}").as_bytes());
        self.response(
            &format!("/api/git/ref/tags/{name}"),
            json!({"object":{"type":"tag","sha":tag_object}}),
        );
        self.response(
            &format!("/api/git/tags/{tag_object}"),
            json!({"object":{"type":"commit","sha":sha}}),
        );
        self.response(
            &format!("/api/git/commits/{sha}"),
            json!({"parents":[{"sha":content}]}),
        );
        PackTag {
            name: name.into(),
            sha,
        }
    }
    fn latest(&self, tag: &PackTag) {
        let bytes =
            self.state.lock().unwrap().bodies[&format!("/raw/{}/auram.json", tag.sha)].clone();
        self.state
            .lock()
            .unwrap()
            .bodies
            .insert("/raw/main/auram.json".into(), bytes);
    }
    fn clear_requests(&self) {
        self.state.lock().unwrap().requests.clear();
    }
    fn requests(&self) -> Vec<String> {
        self.state.lock().unwrap().requests.clone()
    }
    fn payloads(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|p| p.starts_with("/raw/") && p.contains("/minecraft/"))
            .collect()
    }
    async fn install(&self, tag: &PackTag) -> Manifest {
        let source = self.packs.source();
        let selection = source.selection(tag, &Job::default()).await.unwrap();
        let manifest = source
            .inventory(tag.clone(), &selection, &Job::default())
            .await
            .unwrap();
        storage::write_json(&source.snapshot(&tag.sha), &manifest).unwrap();
        self.packs
            .reconcile(
                &self.packs.layout.client(),
                &self.packs.layout.state("tx"),
                None,
                &manifest,
                &Job::default(),
                false,
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn force_reinstall_deletes_all_client_data_and_reuses_verified_shared_content() {
    let f = Fixture::new().await;
    let base = f.snapshot(
        "139",
        files(&[
            ("minecraft/mods/shared.jar", "shared"),
            ("minecraft/mods/old.jar", "old"),
            ("minecraft/config/a.toml", "old config"),
        ]),
        None,
        Some(vec![]),
    );
    f.install(&base).await;
    for path in [
        "saves/world/level.dat",
        "screenshots/screenshot.png",
        "options.txt",
        "resourcepacks/custom.zip",
        "shaderpacks/custom.zip",
        "mods/custom.jar",
    ] {
        storage::atomic_write(&f.packs.layout.game().join(path), b"user file").unwrap();
    }
    // A broken installed-manifest file must not prevent an explicit reset.
    storage::atomic_write(
        &f.packs.layout.client().join(".auram-manifest.json"),
        b"invalid state",
    )
    .unwrap();
    let retained = [
        f.packs.layout.runtime().join("assets/objects/retained"),
        f.packs.layout.server().join("world/level.dat"),
        f.packs.layout.state("settings.json"),
        f.packs.layout.root.join("downloads/retained.part"),
    ];
    for path in &retained {
        storage::atomic_write(path, b"retained").unwrap();
    }
    let target = f.snapshot(
        "145",
        files(&[
            ("minecraft/mods/shared.jar", "shared"),
            ("minecraft/mods/new.jar", "new"),
            ("minecraft/config/a.toml", "fresh config"),
        ]),
        Some(&base),
        Some(vec![
            "-minecraft/mods/old.jar".into(),
            "+minecraft/mods/new.jar".into(),
            "*minecraft/config/a.toml".into(),
        ]),
    );
    f.latest(&target);
    f.clear_requests();
    let installed = f.packs.force_reinstall(&Job::default()).await.unwrap();
    assert_eq!(installed.tag.sha, target.sha);
    assert_eq!(
        fs::read(f.packs.layout.game().join("mods/new.jar")).unwrap(),
        b"new"
    );
    assert_eq!(
        fs::read(f.packs.layout.game().join("config/a.toml")).unwrap(),
        b"fresh config"
    );
    for path in [
        "saves",
        "screenshots",
        "options.txt",
        "resourcepacks",
        "shaderpacks",
        "mods/custom.jar",
        "mods/old.jar",
    ] {
        assert!(
            !f.packs.layout.game().join(path).exists(),
            "{path} survived the reset"
        );
    }
    for path in retained {
        assert_eq!(fs::read(path).unwrap(), b"retained");
    }
    assert!(
        f.payloads().iter().all(|p| !p.ends_with("shared.jar")),
        "Unchanged cached jar was fetched again"
    );
    assert!(!f.packs.layout.client_reset_pending().unwrap());
}

#[tokio::test]
async fn failed_force_reinstall_leaves_no_old_install_pointer_and_can_retry_fresh() {
    let f = Fixture::new().await;
    let base = f.snapshot(
        "139",
        files(&[("minecraft/mods/old.jar", "old")]),
        None,
        Some(vec![]),
    );
    f.install(&base).await;
    storage::atomic_write(
        &f.packs.layout.game().join("saves/world/level.dat"),
        b"world",
    )
    .unwrap();
    let target = f.snapshot(
        "145",
        files(&[("minecraft/mods/new.jar", "new")]),
        Some(&base),
        Some(vec![
            "-minecraft/mods/old.jar".into(),
            "+minecraft/mods/new.jar".into(),
        ]),
    );
    f.latest(&target);
    let path = format!("/raw/{}/minecraft/mods/new.jar", target.sha);
    let body = f.state.lock().unwrap().bodies.remove(&path).unwrap();
    assert!(f.packs.force_reinstall(&Job::default()).await.is_err());
    assert!(f.packs.installed().unwrap().is_none());
    assert!(!f.packs.layout.game().join("saves/world/level.dat").exists());
    f.state.lock().unwrap().bodies.insert(path, body);
    assert_eq!(
        f.packs
            .update(false, &Job::default())
            .await
            .unwrap()
            .tag
            .sha,
        target.sha
    );
    assert_eq!(
        fs::read(f.packs.layout.game().join("mods/new.jar")).unwrap(),
        b"new"
    );
}

#[tokio::test]
async fn cancellation_before_force_reinstall_keeps_existing_client_files() {
    let f = Fixture::new().await;
    storage::atomic_write(
        &f.packs.layout.game().join("saves/world/level.dat"),
        b"world",
    )
    .unwrap();
    let job = Job::default();
    job.cancel.cancel();
    assert!(f.packs.force_reinstall(&job).await.is_err());
    assert_eq!(
        fs::read(f.packs.layout.game().join("saves/world/level.dat")).unwrap(),
        b"world"
    );
    assert!(!f.packs.layout.client_reset_pending().unwrap());
}

#[tokio::test]
async fn server_deploy_composes_skipped_changelists_and_preserves_disabled_jar_renames() {
    use crate::deploy::{self, tests::DiskRemote, Baseline, OwnedFile};
    use crate::server::ServerVersion;
    let f = Fixture::new().await;
    let base = f.snapshot(
        "139",
        files(&[
            ("minecraft/mods/old.jar", "same"),
            ("minecraft/config/stable.toml", "defaults"),
            ("minecraft/config/changed.toml", "before"),
            ("minecraft/scripts/obsolete.zs", "old script"),
            ("server_config.conf", ""),
        ]),
        None,
        Some(vec![]),
    );
    let middle = f.snapshot(
        "143",
        files(&[
            ("minecraft/mods/middle.jar", "same"),
            ("minecraft/config/stable.toml", "defaults"),
            ("minecraft/config/changed.toml", "middle"),
            ("server_config.conf", ""),
        ]),
        Some(&base),
        Some(vec![
            "-minecraft/mods/old.jar".into(),
            "-minecraft/scripts/obsolete.zs".into(),
            "+minecraft/mods/middle.jar".into(),
            "+minecraft/config/changed.toml".into(),
        ]),
    );
    let target = f.snapshot(
        "145",
        files(&[
            ("minecraft/mods/new.jar", "same"),
            ("minecraft/config/stable.toml", "defaults"),
            ("minecraft/config/changed.toml", "after"),
            ("server_config.conf", "clientMods = []"),
        ]),
        Some(&middle),
        Some(vec![
            "-minecraft/mods/middle.jar".into(),
            "+minecraft/mods/new.jar".into(),
            "+minecraft/config/changed.toml".into(),
            "+server_config.conf".into(),
        ]),
    );
    let old = f
        .packs
        .manifest(base.clone(), &Job::default())
        .await
        .unwrap();
    assert!(old.files.contains_key("server_config.conf"));
    let remote = DiskRemote::new();
    let version = ServerVersion {
        schema: 1,
        version: base.name.clone(),
        commit: base.sha.clone(),
        minecraft: "1.20.1".into(),
        forge: "47.4.10".into(),
    };
    let owned = Baseline {
        schema: 1,
        version: version.clone(),
        files: old
            .files
            .iter()
            .filter_map(|(path, file)| {
                let path = path
                    .strip_prefix("minecraft/")
                    .filter(|p| deploy::payload_path(p))?;
                Some((
                    path.into(),
                    OwnedFile {
                        path: path.into(),
                        file: file.clone(),
                    },
                ))
            })
            .collect(),
    };
    remote.put("auram.json", &serde_json::to_vec(&version).unwrap());
    remote.put(".auram-deploy.json", &serde_json::to_vec(&owned).unwrap());
    remote.put("mods/old.jar.client", b"same");
    remote.put("config/stable.toml", b"custom server setting");
    remote.put("config/changed.toml", b"before");
    remote.put("server.properties", b"private");
    remote.put("mods/custom.jar", b"custom");
    remote.put("scripts/obsolete.zs", b"locally edited old script");
    let mut payload = deploy::tests::payload(
        &f.packs,
        "145",
        &[
            ("mods/new.jar", b"same"),
            ("config/stable.toml", b"defaults"),
            ("config/changed.toml", b"after"),
        ],
    );
    payload.tag = target.clone();
    storage::write_json(
        &f.packs.layout.state("server-deploy-payload.json"),
        &payload,
    )
    .unwrap();
    storage::write_json(
        &f.packs.layout.server().join("auram.json"),
        &ServerVersion {
            version: target.name.clone(),
            commit: target.sha.clone(),
            ..version
        },
    )
    .unwrap();
    f.clear_requests();
    let job = Job::default();
    let plan = deploy::inspect(&f.packs, &remote, &job).await.unwrap();
    assert_eq!(plan.strategy, "changelists");
    assert_eq!(plan.changelists, 2);
    assert_eq!(plan.renames, 1);
    assert_eq!(plan.uploads, 1);
    assert!(remote
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|(op, path)| op != "hash" && (op != "list" || path == "mods")));
    deploy::apply(&f.packs, &remote, plan, &job).await.unwrap();
    assert_eq!(remote.get("mods/new.jar.client").unwrap(), b"same");
    assert!(remote.get("mods/old.jar.client").is_none());
    assert_eq!(
        remote.get("config/stable.toml").unwrap(),
        b"custom server setting"
    );
    assert_eq!(remote.get("config/changed.toml").unwrap(), b"after");
    assert_eq!(remote.get("server.properties").unwrap(), b"private");
    assert_eq!(remote.get("mods/custom.jar").unwrap(), b"custom");
    assert!(remote.get("scripts/obsolete.zs").is_none());
    assert!(
        f.payloads().iter().all(|p| !p.ends_with(".jar")),
        "Intermediate or unchanged mod bytes were downloaded"
    );
    let legacy = DiskRemote::new();
    legacy.put(
        "auram.json",
        &serde_json::to_vec(&json!({"version":139})).unwrap(),
    );
    legacy.put("mods/old.jar.client", b"same");
    legacy.put("config/stable.toml", b"custom server setting");
    legacy.put("config/changed.toml", b"before");
    legacy.put("scripts/obsolete.zs", b"locally edited old script");
    let job = Job::default();
    let plan = deploy::inspect(&f.packs, &legacy, &job).await.unwrap();
    assert_eq!(plan.strategy, "changelists");
    assert_eq!(plan.renames, 1);
    assert_eq!(plan.uploads, 1);
    assert_eq!(plan.removals, 1);
    assert!(legacy
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|(op, path)| op != "hash" && (op != "list" || path == "mods")));
    deploy::apply(&f.packs, &legacy, plan, &job).await.unwrap();
    assert_eq!(legacy.get("mods/new.jar.client").unwrap(), b"same");
    assert!(legacy.get("mods/old.jar.client").is_none());
    assert_eq!(
        legacy.get("config/stable.toml").unwrap(),
        b"custom server setting"
    );
}

#[tokio::test]
async fn skipped_releases_compose_final_blobs_and_reuse_renames_without_a_cache() {
    let f = Fixture::new().await;
    let base = f.snapshot(
        "139",
        files(&[
            ("minecraft/mods/A.jar", "A"),
            ("minecraft/mods/edit.jar", "original"),
            ("minecraft/config/untouched.toml", "settings"),
        ]),
        None,
        Some(vec![]),
    );
    f.install(&base).await;
    let middle = f.snapshot(
        "141",
        files(&[
            ("minecraft/mods/B.jar", "A"),
            ("minecraft/mods/edit.jar", "intermediate"),
            ("minecraft/mods/temporary.jar", "temp"),
            ("minecraft/config/untouched.toml", "settings"),
        ]),
        Some(&base),
        Some(vec![
            "-minecraft/mods/A.jar".into(),
            "+minecraft/mods/B.jar".into(),
            "*minecraft/mods/edit.jar".into(),
            "+minecraft/mods/temporary.jar".into(),
        ]),
    );
    let target = f.snapshot(
        "145",
        files(&[
            ("minecraft/mods/C.jar", "A"),
            ("minecraft/mods/edit.jar", "final"),
            ("minecraft/kubejs/名字 #.js", "unicode"),
            ("minecraft/config/untouched.toml", "settings"),
        ]),
        Some(&middle),
        Some(vec![
            "-minecraft/mods/B.jar".into(),
            "+minecraft/mods/C.jar".into(),
            "*minecraft/mods/edit.jar".into(),
            "-minecraft/mods/temporary.jar".into(),
            "+minecraft/kubejs/名字 #.js".into(),
        ]),
    );
    fs::remove_dir_all(f.packs.layout.root.join("cache/blobs")).unwrap();
    storage::atomic_write(&f.packs.layout.game().join("mods/custom.jar"), b"custom").unwrap();
    storage::atomic_write(
        &f.packs.layout.game().join("saves/world/level.dat"),
        b"world",
    )
    .unwrap();
    storage::atomic_write(
        &f.packs.layout.runtime().join("assets/objects/retained"),
        b"asset",
    )
    .unwrap();
    // Unrelated corruption is deliberately left to repair on the small-delta path.
    fs::remove_file(f.packs.layout.game().join("config/untouched.toml")).unwrap();
    f.latest(&target);
    f.clear_requests();
    let installed = f.packs.update(false, &Job::default()).await.unwrap();
    assert_eq!(installed.tag.sha, target.sha);
    assert_eq!(
        fs::read(f.packs.layout.game().join("mods/C.jar")).unwrap(),
        b"A"
    );
    assert!(!f.packs.layout.game().join("mods/A.jar").exists());
    assert!(!f.packs.layout.game().join("mods/B.jar").exists());
    assert!(!f.packs.layout.game().join("config/untouched.toml").exists());
    assert!(f.packs.layout.game().join("mods/custom.jar").exists());
    assert!(f.packs.layout.game().join("saves/world/level.dat").exists());
    assert_eq!(
        fs::read(f.packs.layout.runtime().join("assets/objects/retained")).unwrap(),
        b"asset"
    );
    let requests = f.payloads();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(requests
        .iter()
        .all(|p| p.starts_with(&format!("/raw/{}/", target.sha))));
    assert!(requests
        .iter()
        .all(|p| !p.contains("C.jar") && !p.contains("B.jar") && !p.contains("temporary")));
    assert!(requests.iter().any(|p| p.contains("%23")));
    f.clear_requests();
    f.packs.update(true, &Job::default()).await.unwrap();
    assert_eq!(
        fs::read(f.packs.layout.game().join("config/untouched.toml")).unwrap(),
        b"settings"
    );
}

#[tokio::test]
async fn sparse_inventory_prunes_unrelated_payload_and_walks_truncated_subtrees() {
    let f = Fixture::new().await;
    let tag = f.snapshot(
        "145",
        files(&[
            ("minecraft/mods/a.jar", "a"),
            ("minecraft/config/nested/a.toml", "cfg"),
            ("minecraft/saves/world/data", "world"),
            ("minecraft/libraries/big.jar", "library"),
            ("minecraft/ignored/no", "ignored"),
            ("server_files/server.properties", "properties"),
            ("tools/unrelated", "tool"),
        ]),
        None,
        Some(vec![]),
    );
    // Force a truncated config recursive response, retaining its valid direct subtree.
    let root: Value = serde_json::from_slice(
        &f.state.lock().unwrap().bodies[&format!("/api/git/trees/{}", tag.sha)],
    )
    .unwrap();
    let mc = root["tree"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == "minecraft")
        .unwrap()["sha"]
        .as_str()
        .unwrap()
        .to_string();
    let tree: Value =
        serde_json::from_slice(&f.state.lock().unwrap().bodies[&format!("/api/git/trees/{mc}")])
            .unwrap();
    let config = tree["tree"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == "config")
        .unwrap()["sha"]
        .as_str()
        .unwrap()
        .to_string();
    f.response(
        &format!("/api/git/trees/{config}?recursive=1"),
        json!({"tree":[],"truncated":true}),
    );
    f.clear_requests();
    let manifest = f.install(&tag).await;
    assert!(manifest
        .files
        .contains_key("minecraft/config/nested/a.toml"));
    assert_eq!(manifest.files.len(), 3);
    assert!(f
        .payloads()
        .iter()
        .all(|p| p.contains("mods/") || p.contains("config/")));
    for excluded in ["saves", "libraries", "ignored"] {
        let sha = tree["tree"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["path"] == excluded)
            .unwrap()["sha"]
            .as_str()
            .unwrap();
        assert!(
            !f.requests().iter().any(|p| p.contains(sha)),
            "traversed {excluded}"
        );
    }
    f.clear_requests();
    let source = f.packs.source();
    let selection = source.selection(&tag, &Job::default()).await.unwrap();
    source
        .inventory(tag, &selection, &Job::default())
        .await
        .unwrap();
    assert!(
        f.requests().is_empty(),
        "Immutable tree and blob caches should avoid repeat HTTP reads"
    );
}

#[tokio::test]
async fn missing_or_inconsistent_chain_falls_back_to_complete_target() {
    for broken in [
        "missing",
        "retargeted",
        "cycle",
        "schema",
        "filter",
        "policy",
    ] {
        let f = Fixture::new().await;
        let base = f.snapshot(
            "139",
            files(&[
                ("minecraft/mods/a.jar", "old"),
                ("minecraft/config/unchanged", "retained"),
            ]),
            None,
            Some(vec![]),
        );
        f.install(&base).await;
        if broken == "policy" {
            let mut old = f.packs.installed().unwrap().unwrap();
            old.policy = 0;
            storage::write_json(&f.packs.layout.client().join(".auram-manifest.json"), &old)
                .unwrap();
        }
        let mut next = files(&[
            ("minecraft/mods/b.jar", "new"),
            ("minecraft/config/unchanged", "retained"),
            ("minecraft/ignored/previously.bin", "now included"),
        ]);
        if broken == "filter" {
            next.insert(".packignore".into(), b"".to_vec());
        }
        let target = f.snapshot(
            "145",
            next,
            Some(&base),
            Some(vec![
                "-minecraft/mods/a.jar".into(),
                "+minecraft/mods/b.jar".into(),
                if broken == "filter" {
                    "*.packignore"
                } else {
                    "*auram.json"
                }
                .into(),
            ]),
        );
        let key = format!("/raw/{}/changelist/build%20145.json", target.sha);
        if broken == "missing" {
            f.state.lock().unwrap().bodies.remove(&key);
        }
        if broken == "retargeted" {
            f.response(
                "/api/git/ref/tags/139",
                json!({"object":{"type":"commit","sha":hash(b"wrong")}}),
            );
        }
        if broken == "cycle" || broken == "schema" {
            let mut value: Value =
                serde_json::from_slice(&f.state.lock().unwrap().bodies[&key]).unwrap();
            if broken == "schema" {
                value["schema"] = json!(99);
            } else {
                value["previousVersion"] = json!("145");
                value["previousCommit"] = json!(target.sha);
            }
            // Alter both published blob metadata and its body, keeping integrity verification meaningful.
            let bytes = serde_json::to_vec(&value).unwrap();
            let mut state = f.state.lock().unwrap();
            state.bodies.insert(key.clone(), bytes.clone());
            for body in state.bodies.values_mut() {
                if let Ok(mut tree) = serde_json::from_slice::<Value>(body) {
                    if let Some(entries) = tree.get_mut("tree").and_then(Value::as_array_mut) {
                        for entry in entries {
                            if entry["path"]
                                .as_str()
                                .is_some_and(|p| p.ends_with("build 145.json"))
                            {
                                entry["sha"] = json!(blob(&bytes));
                                entry["size"] = json!(bytes.len());
                            }
                        }
                        *body = serde_json::to_vec(&tree).unwrap();
                    }
                }
            }
        }
        fs::remove_file(f.packs.layout.game().join("config/unchanged")).unwrap();
        f.latest(&target);
        f.clear_requests();
        let installed = f.packs.update(false, &Job::default()).await.unwrap();
        assert_eq!(installed.tag.sha, target.sha, "{broken}");
        assert!(
            f.packs.layout.game().join("config/unchanged").exists(),
            "{broken}: fallback must inspect unchanged files"
        );
        assert!(!f.packs.layout.game().join("mods/a.jar").exists());
    }
}

#[tokio::test]
async fn exact_legacy_baseline_works_without_a_baseline_changelist() {
    let f = Fixture::new().await;
    let base = f.snapshot(
        "1.0.7",
        files(&[("minecraft/mods/legacy.jar", "same")]),
        None,
        None,
    );
    f.install(&base).await;
    let target = f.snapshot(
        "139",
        files(&[("minecraft/mods/new.jar", "same")]),
        Some(&base),
        Some(vec![
            "-minecraft/mods/legacy.jar".into(),
            "+minecraft/mods/new.jar".into(),
        ]),
    );
    f.latest(&target);
    f.clear_requests();
    f.packs.update(false, &Job::default()).await.unwrap();
    assert!(f.payloads().is_empty());
    assert!(!f
        .requests()
        .iter()
        .any(|r| r == &format!("/raw/{}/auram.json", base.sha)));
    assert!(!f.packs.layout.game().join("mods/legacy.jar").exists());
    assert!(f.packs.layout.game().join("mods/new.jar").exists());
}

#[tokio::test]
async fn failed_payload_keeps_the_old_commit_and_all_owned_files() {
    let f = Fixture::new().await;
    let base = f.snapshot(
        "139",
        files(&[("minecraft/mods/old.jar", "old")]),
        None,
        Some(vec![]),
    );
    f.install(&base).await;
    let target = f.snapshot(
        "141",
        files(&[("minecraft/mods/new.jar", "new")]),
        Some(&base),
        Some(vec![
            "-minecraft/mods/old.jar".into(),
            "+minecraft/mods/new.jar".into(),
        ]),
    );
    f.state
        .lock()
        .unwrap()
        .bodies
        .remove(&format!("/raw/{}/minecraft/mods/new.jar", target.sha));
    f.latest(&target);
    assert!(f.packs.update(false, &Job::default()).await.is_err());
    assert_eq!(f.packs.installed().unwrap().unwrap().tag.sha, base.sha);
    assert!(f.packs.layout.game().join("mods/old.jar").exists());
    assert!(!f.packs.layout.game().join("mods/new.jar").exists());
}

#[test]
fn paths_preserve_bytes_and_inventory_rejects_collisions_and_unsupported_modes() {
    let release = Release {
        version: 145,
        changelist: "custom.json".into(),
    };
    let mut list:Changelist=serde_json::from_value(json!({"schema":1,"version":145,"previousVersion":null,"previousCommit":null,"contentCommit":hash(b"content"),"changes":["+minecraft/名前 with\ttab\nand newline"]})).unwrap();
    assert!(list
        .plan(&release)
        .unwrap()
        .contains_key("minecraft/名前 with\ttab\nand newline"));
    for entry in ["", "+../escape", "+/absolute", "?file", "+a\\b", "+a/./b"] {
        list.changes = vec![entry.into()];
        assert!(list.plan(&release).is_err());
    }
    list.changes = vec!["+a".into(), "*a".into()];
    assert!(list.plan(&release).is_err());
    let file = PackFile {
        sha: blob(b"x"),
        size: 1,
        mode: "100644".into(),
        source: None,
    };
    for (a, b) in [
        ("minecraft/mods/A.jar", "minecraft/mods/a.jar"),
        ("minecraft/café", "minecraft/cafe\u{301}"),
        ("minecraft/one", "minecraft/ONE/child"),
    ] {
        let manifest = Manifest {
            schema: 1,
            policy: 1,
            tag: PackTag {
                name: "1".into(),
                sha: hash(b"tag"),
            },
            files: BTreeMap::from([(a.into(), file.clone()), (b.into(), file.clone())]),
        };
        assert!(validate_inventory(&manifest).is_err());
    }
    assert!(!Checksum::GitBlob(blob(b"a"))
        .verify(Path::new("/nonexistent"))
        .unwrap_or(false));
    for (kind, mode) in [("blob", "120000"), ("commit", "160000")] {
        assert!(Entry {
            path: "unsafe".into(),
            sha: hash(b"object"),
            kind: kind.into(),
            mode: mode.into(),
            size: Some(1)
        }
        .file()
        .is_err());
    }
}

#[tokio::test]
async fn legacy_tag_discovery_paginates_and_supports_lightweight_tags() {
    let f = Fixture::new().await;
    let tag = f.snapshot("1.0.10", files(&[]), None, None);
    f.response(
        "/api/git/ref/tags/1.0.10",
        json!({"object":{"type":"commit","sha":tag.sha}}),
    );
    f.response(
        "/api/tags?per_page=100&page=1",
        Value::Array(
            (0..100)
                .map(|n| json!({"name":format!("unrelated-{n}")}))
                .collect(),
        ),
    );
    f.response(
        "/api/tags?per_page=100&page=2",
        json!([{"name":"1.0.9"},{"name":"1.0.10"}]),
    );
    assert_eq!(f.packs.latest().await.unwrap().sha, tag.sha);
    assert!(f.requests().iter().any(|p| p.ends_with("page=2")));
}

#[tokio::test]
async fn case_only_rename_preserves_bytes_and_updates_the_actual_filename() {
    let f = Fixture::new().await;
    let base = f.snapshot(
        "139",
        files(&[("minecraft/mods/A.jar", "same")]),
        None,
        Some(vec![]),
    );
    f.install(&base).await;
    let target = f.snapshot(
        "141",
        files(&[("minecraft/mods/a.jar", "same")]),
        Some(&base),
        Some(vec![
            "-minecraft/mods/A.jar".into(),
            "+minecraft/mods/a.jar".into(),
        ]),
    );
    f.latest(&target);
    f.clear_requests();
    f.packs.update(false, &Job::default()).await.unwrap();
    let names = fs::read_dir(f.packs.layout.game().join("mods"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names, vec![std::ffi::OsString::from("a.jar")]);
    assert!(f.payloads().is_empty());
}

#[tokio::test]
async fn server_source_reads_reuse_verified_client_files_after_cache_eviction() {
    let f = Fixture::new().await;
    let tag = f.snapshot(
        "145",
        files(&[("minecraft/mods/client-and-server.jar", "shared")]),
        None,
        Some(vec![]),
    );
    let manifest = f.install(&tag).await;
    fs::remove_dir_all(f.packs.layout.root.join("cache/blobs")).unwrap();
    f.clear_requests();
    let file = f
        .packs
        .file(
            &manifest,
            "minecraft/mods/client-and-server.jar",
            &Job::default(),
        )
        .await
        .unwrap();
    assert_eq!(fs::read(file).unwrap(), b"shared");
    assert!(f.requests().is_empty());
}

#[tokio::test]
async fn corrupt_metadata_cache_is_replaced_and_repair_still_checks_disk() {
    let f = Fixture::new().await;
    let tag = f.snapshot(
        "145",
        files(&[("minecraft/mods/a.jar", "a")]),
        None,
        Some(vec![]),
    );
    f.install(&tag).await;
    let cache = f
        .packs
        .layout
        .state(&format!("git-trees/{}-direct.json", tag.sha));
    for broken in [
        b"{broken JSON".to_vec(),
        serde_json::to_vec(&json!({"tree":[],"truncated":true})).unwrap(),
    ] {
        storage::atomic_write(&cache, &broken).unwrap();
        f.clear_requests();
        f.packs.update(true, &Job::default()).await.unwrap();
        assert!(f
            .requests()
            .contains(&format!("/api/git/trees/{}", tag.sha)));
        assert!(f.payloads().is_empty());
    }
    fs::remove_file(f.packs.layout.game().join("mods/a.jar")).unwrap();
    storage::atomic_write(&f.packs.source().snapshot(&tag.sha), b"{broken JSON").unwrap();
    f.latest(&tag);
    f.packs.update(false, &Job::default()).await.unwrap();
    assert!(f.packs.layout.game().join("mods/a.jar").exists());
}

#[tokio::test]
async fn cancelling_metadata_discovery_interrupts_the_http_request() {
    let f = Fixture::new().await;
    let tag = f.snapshot(
        "145",
        files(&[("minecraft/mods/installed.jar", "valid")]),
        None,
        Some(vec![]),
    );
    f.install(&tag).await;
    let mut packs = f.packs.clone();
    let started = Arc::new(tokio::sync::Notify::new());
    let signal = started.clone();
    let app = Router::new().route(
        "/main/auram.json",
        get(move || {
            let signal = signal.clone();
            async move {
                signal.notify_one();
                std::future::pending::<String>().await
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    packs.repository.raw = format!("http://{address}/");
    let job = Job::default();
    let cancel = job.cancel.clone();
    let running = tokio::spawn(async move { packs.update(false, &job).await });
    tokio::time::timeout(std::time::Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    cancel.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), running)
        .await
        .unwrap()
        .unwrap();
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn executable_mode_changes_use_private_copies_of_cached_blobs() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new().await;
    let tag = f.snapshot(
        "139",
        files(&[("minecraft/scripts/tool.sh", "original")]),
        None,
        Some(vec![]),
    );
    let base = f.install(&tag).await;
    let mut desired = base.clone();
    let file = desired.files.get_mut("minecraft/scripts/tool.sh").unwrap();
    file.mode = "100755".into();
    let cache = f.packs.blob_path(file);
    f.clear_requests();
    f.packs
        .reconcile(
            &f.packs.layout.client(),
            &f.packs.layout.state("tx"),
            Some(&base),
            &desired,
            &Job::default(),
            false,
        )
        .await
        .unwrap();
    let target = f.packs.layout.game().join("scripts/tool.sh");
    assert_ne!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o111,
        0
    );
    assert_eq!(
        fs::metadata(&cache).unwrap().permissions().mode() & 0o111,
        0
    );
    fs::write(target, b"user edit").unwrap();
    assert_eq!(fs::read(cache).unwrap(), b"original");
    assert!(f.payloads().is_empty());
}

#[tokio::test]
#[ignore = "Live GitHub metadata smoke test; run explicitly on the development machine"]
async fn published_build145_chains_and_jar_rename_identities() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path().to_owned()).unwrap();
    let packs = Packs::new(layout.clone(), Downloader::new(&layout).unwrap());
    let source = packs.source();
    let job = Job::default();
    let old = source.resolve("139", &job).await.unwrap();
    let target = source.resolve("145", &job).await.unwrap();
    assert_eq!(target.sha, "73c6252eb99a6d8cd502e2cdca33f1a06179ce15");
    let selection = source.selection(&old, &job).await.unwrap();
    let base = source.inventory(old, &selection, &job).await.unwrap();
    let selection = source.selection(&target, &job).await.unwrap();
    let (final_manifest, paths, count) = source
        .delta(&base, &target, &selection, &job)
        .await
        .unwrap();
    assert_eq!(count, 3);
    assert!(!paths.is_empty());
    let previous = source.resolve("143", &job).await.unwrap();
    let policy = source.selection(&previous, &job).await.unwrap();
    let baseline = source.inventory(previous, &policy, &job).await.unwrap();
    let (direct, _, count) = source
        .delta(&baseline, &target, &selection, &job)
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(direct.files, final_manifest.files);
    for (old, new) in [
        ("Auram.jar", "AuramCore-1.0.5.jar"),
        ("Imperium.jar", "Imperium-1.0.0.jar"),
        ("Satiscraftory.jar", "Satiscraftory-1.0.0.jar"),
    ] {
        let before = &baseline.files[&format!("minecraft/mods/{old}")];
        let after = &direct.files[&format!("minecraft/mods/{new}")];
        assert_eq!(before.sha, after.sha, "{old} → {new}");
        assert!(
            !packs.blob_path(after).exists(),
            "Planning must never download jar bodies"
        );
        println!("Verified unchanged blob for {old} → {new}: {}", after.sha);
    }
    println!("Live 139 → 145: 3 changelists, {} selected paths, metadata bodies {} bytes, zero jar-body downloads",paths.len(),packs.net.transferred_bytes());
}
