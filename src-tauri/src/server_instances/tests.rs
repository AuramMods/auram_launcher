use super::*;
use crate::deploy::connection::{ConnectionSettings, Remote};
fn version(n: &str) -> server::ServerVersion {
    server::ServerVersion {
        schema: 1,
        version: n.into(),
        commit: n.repeat(40),
        minecraft: "1.20.1".into(),
        forge: "47.4.10".into(),
    }
}
#[test]
fn saved_instances_start_empty_persist_independently_and_keep_only_nonsecret_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path().join("data")).unwrap();
    let local = temp.path().join("Server 世界 with spaces");
    fs::create_dir(&local).unwrap();
    let mut registry = Registry::load(&layout).unwrap();
    assert!(registry.servers.is_empty());
    let a = registry
        .prepare(
            &layout,
            ServerInput {
                id: None,
                name: " Local ".into(),
                target: Target::Local {
                    directory: local.to_string_lossy().into_owned(),
                },
            },
        )
        .unwrap();
    let b = registry
        .prepare(
            &layout,
            ServerInput {
                id: None,
                name: "Hosted".into(),
                target: Target::Sftp {
                    connection: ConnectionSettings {
                        host: "sftp.example.com".into(),
                        username: "server".into(),
                        ..Default::default()
                    },
                },
            },
        )
        .unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(a.name, "Local");
    registry.put(a.clone());
    registry.put(b.clone());
    registry.observation(&a.id, Some(version("139"))).unwrap();
    registry.observation(&b.id, Some(version("145"))).unwrap();
    registry.write(&layout).unwrap();
    let mut loaded = Registry::load(&layout).unwrap();
    assert_eq!(
        loaded.get(&a.id).unwrap().known_version.unwrap().version,
        "139"
    );
    assert_eq!(
        loaded.get(&b.id).unwrap().known_version.unwrap().version,
        "145"
    );
    loaded.failure(&b.id, "Connection refused").unwrap();
    loaded.write(&layout).unwrap();
    let loaded = Registry::load(&layout).unwrap();
    assert_eq!(
        loaded.get(&b.id).unwrap().known_version.unwrap().version,
        "145"
    );
    assert_eq!(
        loaded.get(&b.id).unwrap().last_error.as_deref(),
        Some("Connection refused")
    );
    let body = fs::read_to_string(layout.state("servers.json")).unwrap();
    assert!(!body.contains("password\":") && !body.contains("passphrase"));
}
#[test]
fn destination_edits_invalidate_versions_and_cannot_forge_host_pins_or_share_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path().join("data")).unwrap();
    let mut registry = Registry::load(&layout).unwrap();
    let settings = ConnectionSettings {
        host: "a.example.com".into(),
        username: "user".into(),
        fingerprint: "FORGED".into(),
        ..Default::default()
    };
    let mut saved = registry
        .prepare(
            &layout,
            ServerInput {
                id: None,
                name: "A".into(),
                target: Target::Sftp {
                    connection: settings.clone(),
                },
            },
        )
        .unwrap();
    let Target::Sftp { connection } = &mut saved.target else {
        panic!()
    };
    assert!(connection.fingerprint.is_empty());
    connection.fingerprint = "VERIFIED".into();
    saved.known_version = Some(version("145"));
    registry.put(saved.clone());
    let changed = registry
        .prepare(
            &layout,
            ServerInput {
                id: Some(saved.id.clone()),
                name: "Renamed".into(),
                target: saved.target.clone(),
            },
        )
        .unwrap();
    assert!(changed.known_version.is_some());
    let changed = registry
        .prepare(
            &layout,
            ServerInput {
                id: Some(saved.id.clone()),
                name: "A".into(),
                target: Target::Sftp {
                    connection: ConnectionSettings {
                        host: "other.example.com".into(),
                        ..settings.clone()
                    },
                },
            },
        )
        .unwrap();
    let Target::Sftp { connection } = changed.target else {
        panic!()
    };
    assert!(connection.fingerprint.is_empty());
    assert!(changed.known_version.is_none());
    assert_ne!(
        settings.instance_identity(&layout, "instance-a"),
        settings.instance_identity(&layout, "instance-b")
    );
    assert_ne!(
        settings.instance_identity(&layout, "instance-a"),
        connection.instance_identity(&layout, "instance-a")
    );
    assert_eq!(
        settings.instance_identity(&layout, "instance-a"),
        ConnectionSettings {
            directory: "/another/root".into(),
            ..settings.clone()
        }
        .instance_identity(&layout, "instance-a")
    );
}
#[test]
fn legacy_saved_connection_is_adopted_without_creating_a_default_local_server() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path().into()).unwrap();
    let settings = ConnectionSettings {
        host: "previous.example.com".into(),
        username: "player".into(),
        fingerprint: "trusted".into(),
        ..Default::default()
    };
    settings.save(&layout, None).unwrap();
    let mut registry = Registry::load(&layout).unwrap();
    assert_eq!(registry.servers.len(), 1);
    assert_eq!(registry.servers[0].id, "legacy-sftp");
    assert!(matches!(registry.servers[0].target, Target::Sftp { .. }));
    registry.servers.clear();
    registry.write(&layout).unwrap();
    assert!(Registry::load(&layout).unwrap().servers.is_empty());
}
#[test]
fn local_targets_exclude_launcher_data_and_its_ancestors() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path().join("data")).unwrap();
    for forbidden in [
        &layout.root,
        &layout.client(),
        &temp.path().to_owned(),
        &PathBuf::from("/"),
    ] {
        assert!(local_directory(&layout, &forbidden.to_string_lossy()).is_err());
    }
    let allowed = temp.path().join("actual server");
    fs::create_dir(&allowed).unwrap();
    assert_eq!(
        local_directory(&layout, &allowed.to_string_lossy()).unwrap(),
        fs::canonicalize(&allowed).unwrap()
    );
    assert_ne!(
        local_journal(&layout, &allowed),
        local_journal(&layout, &temp.path().join("another"))
    );
}
#[tokio::test]
async fn local_transport_checks_version_files_limits_paths_and_cancelled_transfers() {
    let temp = tempfile::tempdir().unwrap();
    let remote = deploy::local::LocalRemote::new(temp.path().into()).unwrap();
    assert!(deploy::read_version(&remote).await.unwrap().is_none());
    remote
        .write("auram.json", br#"{"version":145}"#)
        .await
        .unwrap();
    assert_eq!(
        deploy::read_version(&remote)
            .await
            .unwrap()
            .unwrap()
            .version,
        "145"
    );
    assert!(remote.read("auram.json", 2).await.is_err());
    remote.write("auram.json", b"invalid json").await.unwrap();
    assert!(deploy::read_version(&remote).await.unwrap().is_none());
    assert!(remote.write("../outside", b"bad").await.is_err());
    remote.mkdir("stage").await.unwrap();
    let source = temp.path().join("source");
    fs::write(&source, b"payload").unwrap();
    remote
        .upload(&source, "stage/file", "file", &Job::default())
        .await
        .unwrap();
    assert_eq!(remote.hash("stage/file").await.unwrap().len(), 40);
    let job = Job::default();
    job.cancel.cancel();
    assert!(remote
        .upload(&source, "stage/cancelled", "cancelled", &job)
        .await
        .is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(temp.path(), temp.path().join("link")).unwrap();
        assert!(remote.write("link/escaped", b"bad").await.is_err());
    }
}
