use super::*;
use crate::{deploy::connection::ConnectionSettings, jobs::PhaseSpec};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
fn fixture() -> (tempfile::TempDir, Locations, Layout, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let default_root = temp.path().join("default/AuramLauncher");
    let locations = Locations {
        default_root: default_root.clone(),
        control: temp.path().join("control"),
        folder_name: "AuramLauncher".into(),
    };
    let source = Layout::new(default_root).unwrap();
    let parent = temp.path().join("Other drive with spaces 世界");
    fs::create_dir_all(&parent).unwrap();
    for (path, body) in [
        (
            "client/minecraft/saves/world/level.dat",
            b"world".as_slice(),
        ),
        ("client/minecraft/options.txt", b"user options"),
        ("client/minecraft/resourcepacks/custom.zip", b"user pack"),
        ("client/minecraft/shaderpacks/custom.zip", b"shader"),
        ("cache/minecraft/assets/objects/aa/blob", b"verified asset"),
        ("downloads/partial.part", b"partial body"),
        ("server/world/level.dat", b"server world"),
        ("server/server.properties", b"private"),
        ("server/eula.txt", b"eula=true\n"),
    ] {
        storage::atomic_write(&source.root.join(path), body).unwrap();
    }
    storage::write_json(
        &source.state("settings.json"),
        &crate::storage::Settings::default(),
    )
    .unwrap();
    (temp, locations, source, parent)
}
fn tracked_job(action: impl Fn(crate::jobs::Progress) + Send + Sync + 'static) -> Job {
    Job::new(
        "Move data",
        &[
            PhaseSpec("data-plan", "Inspect", false),
            PhaseSpec("data-copy", "Copy", true),
            PhaseSpec("data-verify", "Verify", true),
            PhaseSpec("data-activate", "Activate", false),
            PhaseSpec("data-cleanup", "Cleanup", true),
        ],
        Arc::new(action),
    )
}
#[test]
fn move_and_reset_preserve_userdata_shared_resources_partials_and_sftp_identity() {
    let (_temp, locations, source, parent) = fixture();
    let settings = ConnectionSettings {
        host: "example.test".into(),
        username: "test".into(),
        authentication: "key".into(),
        key_path: source
            .root
            .join("keys/private.key")
            .to_string_lossy()
            .into_owned(),
        ..Default::default()
    };
    storage::atomic_write(&source.root.join("keys/private.key"), b"fixture-only-key").unwrap();
    storage::write_json(&source.state("servers.json"),&serde_json::json!({"schema":1,"servers":[{"id":"fixture","name":"Server","target":{"kind":"sftp","connection":settings},"knownVersion":null,"checkedAt":null,"lastError":null}]})).unwrap();
    let identity = settings.instance_identity(&source, "fixture");
    let target = locations.destination(Some(&parent)).unwrap();
    assert_eq!(target.file_name().unwrap(), "AuramLauncher");
    let result = locations
        .relocate(&source, target.clone(), &Job::default())
        .unwrap();
    assert!(result.restart);
    assert!(result.warning.is_none());
    assert!(!source.root.exists());
    assert_eq!(locations.resolve().unwrap(), target);
    assert!(!locations.status().unwrap().is_default);
    let installed = Layout::new(target.clone()).unwrap();
    assert_eq!(installed.credential_root(), source.credential_root());
    assert_eq!(
        fs::read(installed.game().join("saves/world/level.dat")).unwrap(),
        b"world"
    );
    assert_eq!(
        fs::read(installed.root.join("downloads/partial.part")).unwrap(),
        b"partial body"
    );
    assert_eq!(
        fs::read(installed.server().join("server.properties")).unwrap(),
        b"private"
    );
    assert_eq!(
        fs::read(installed.server().join("eula.txt")).unwrap(),
        b"eula=true\n"
    );
    let registry = crate::server_instances::Registry::load(&installed).unwrap();
    let crate::server_instances::Target::Sftp { connection } = &registry.servers[0].target else {
        panic!("SFTP instance lost");
    };
    assert_eq!(
        connection.key_path,
        installed.root.join("keys/private.key").to_string_lossy()
    );
    assert_eq!(
        connection.instance_identity(&installed, "fixture"),
        identity
    );
    locations
        .relocate(
            &installed,
            locations.destination(None).unwrap(),
            &Job::default(),
        )
        .unwrap();
    assert!(locations.status().unwrap().is_default);
    assert!(!target.exists());
    assert_eq!(
        fs::read(source.game().join("saves/world/level.dat")).unwrap(),
        b"world"
    );
    assert!(locations.pending().unwrap().is_none());
}
#[test]
fn cancellation_retains_originals_and_retry_recovers_verified_staging() {
    let (_temp, locations, source, parent) = fixture();
    let target = locations.destination(Some(&parent)).unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let cancel = cancellation.clone();
    let mut job = tracked_job(move |progress| {
        if progress
            .phases
            .iter()
            .any(|p| p.id == "data-copy" && p.completed > 0)
        {
            cancel.cancel();
        }
    });
    job.cancel = cancellation;
    assert!(locations.relocate(&source, target.clone(), &job).is_err());
    assert_eq!(locations.resolve().unwrap(), source.root);
    assert!(locations.pending().unwrap().is_some());
    assert_eq!(
        fs::read(source.game().join("saves/world/level.dat")).unwrap(),
        b"world"
    );
    let result = locations
        .relocate(&source, target.clone(), &Job::default())
        .unwrap();
    assert!(result.restart);
    assert_eq!(locations.resolve().unwrap(), target);
}
#[test]
fn crash_after_directory_promotion_recovers_without_recopying_or_losing_originals() {
    let (_temp, locations, source, parent) = fixture();
    let target = locations.destination(Some(&parent)).unwrap();
    let path = locations.control.join("location.json");
    let blocked = Arc::new(AtomicBool::new(false));
    let flag = blocked.clone();
    let failed_path = path.clone();
    let job = tracked_job(move |progress| {
        if progress
            .phases
            .get(progress.active_phase)
            .is_some_and(|p| p.id == "data-activate")
            && !flag.swap(true, Ordering::SeqCst)
        {
            fs::create_dir(&failed_path).unwrap();
        }
    });
    assert!(locations.relocate(&source, target.clone(), &job).is_err());
    assert!(blocked.load(Ordering::SeqCst));
    assert!(target.join(SNAPSHOT).is_file());
    assert!(source.game().join("saves/world/level.dat").exists());
    fs::remove_dir(path).unwrap();
    locations
        .relocate(&source, target.clone(), &Job::default())
        .unwrap();
    assert_eq!(locations.resolve().unwrap(), target);
    assert!(!source.root.exists());
}
#[test]
fn corrupt_prepared_data_is_never_adopted_and_cancellation_retains_its_contents() {
    let (_temp, locations, source, parent) = fixture();
    let target = locations.destination(Some(&parent)).unwrap();
    let config = locations.control.join("location.json");
    let fail = config.clone();
    let once = AtomicBool::new(false);
    let job = tracked_job(move |p| {
        if p.phases
            .get(p.active_phase)
            .is_some_and(|p| p.id == "data-activate")
            && !once.swap(true, Ordering::SeqCst)
        {
            fs::create_dir(&fail).unwrap();
        }
    });
    assert!(locations.relocate(&source, target.clone(), &job).is_err());
    fs::remove_dir(config).unwrap();
    storage::atomic_write(
        &target.join("client/minecraft/saves/world/level.dat"),
        b"changed in prepared copy",
    )
    .unwrap();
    assert!(locations
        .relocate(&source, target.clone(), &Job::default())
        .is_err());
    assert_eq!(locations.resolve().unwrap(), source.root);
    let pending = locations.pending().unwrap().unwrap();
    locations.cancel().unwrap();
    let retained = target
        .parent()
        .unwrap()
        .join(format!(".auram-data-cancelled-{}", pending.id));
    assert_eq!(
        fs::read(retained.join("client/minecraft/saves/world/level.dat")).unwrap(),
        b"changed in prepared copy"
    );
    assert_eq!(
        fs::read(source.game().join("saves/world/level.dat")).unwrap(),
        b"world"
    );
    assert!(locations.pending().unwrap().is_none());
}
#[test]
fn overlapping_and_nonempty_destinations_are_rejected_without_overwriting_files() {
    let (_temp, locations, source, parent) = fixture();
    let nested = source.root.join("nested");
    fs::create_dir(&nested).unwrap();
    assert!(locations
        .relocate(
            &source,
            locations.destination(Some(&nested)).unwrap(),
            &Job::default()
        )
        .is_err());
    let target = locations.destination(Some(&parent)).unwrap();
    fs::create_dir(&target).unwrap();
    storage::atomic_write(&target.join("unknown"), b"untouched").unwrap();
    assert!(locations
        .relocate(&source, target.clone(), &Job::default())
        .is_err());
    assert_eq!(fs::read(target.join("unknown")).unwrap(), b"untouched");
}
#[test]
fn late_user_writes_are_retained_and_reported_after_the_verified_copy_is_activated() {
    let (_temp, locations, source, parent) = fixture();
    let target = locations.destination(Some(&parent)).unwrap();
    let original = source.game().join("saves/world/level.dat");
    let changed = original.clone();
    let once = AtomicBool::new(false);
    let job = tracked_job(move |p| {
        if p.phases
            .get(p.active_phase)
            .is_some_and(|p| p.id == "data-cleanup")
            && !once.swap(true, Ordering::SeqCst)
        {
            storage::atomic_write(&changed, b"new external write").unwrap();
        }
    });
    let outcome = locations.relocate(&source, target.clone(), &job).unwrap();
    assert!(outcome.warning.is_some());
    assert_eq!(fs::read(original).unwrap(), b"new external write");
    assert_eq!(
        fs::read(target.join("client/minecraft/saves/world/level.dat")).unwrap(),
        b"world"
    );
    assert!(locations.status().unwrap().notice.is_some());
}
#[test]
fn missing_custom_drive_does_not_create_an_empty_install_and_can_reset_explicitly() {
    let (_temp, locations, source, parent) = fixture();
    let target = locations.destination(Some(&parent)).unwrap();
    locations
        .relocate(&source, target.clone(), &Job::default())
        .unwrap();
    let absent = target.with_extension("disconnected");
    fs::rename(&target, &absent).unwrap();
    assert!(locations.resolve().is_err());
    assert!(!target.exists());
    assert!(locations.status().unwrap().unavailable.is_some());
    locations.reset_unavailable().unwrap();
    assert_eq!(locations.resolve().unwrap(), locations.default_root);
    assert!(absent
        .join("client/minecraft/saves/world/level.dat")
        .exists());
}
#[cfg(unix)]
#[test]
fn symlinks_are_preserved_without_following_or_deleting_external_worlds() {
    let (temp, locations, source, parent) = fixture();
    let external = temp.path().join("outside");
    fs::create_dir(&external).unwrap();
    storage::atomic_write(&external.join("world"), b"outside").unwrap();
    let link = source.game().join("external-world");
    std::os::unix::fs::symlink(&external, &link).unwrap();
    std::os::unix::fs::symlink(
        source.root.join("cache/minecraft/assets/objects/aa/blob"),
        source.root.join("asset-link"),
    )
    .unwrap();
    let target = locations.destination(Some(&parent)).unwrap();
    locations
        .relocate(&source, target.clone(), &Job::default())
        .unwrap();
    assert_eq!(
        fs::read_link(target.join("client/minecraft/external-world")).unwrap(),
        external
    );
    assert_eq!(fs::read(external.join("world")).unwrap(), b"outside");
    assert_eq!(
        fs::read(target.join("asset-link")).unwrap(),
        b"verified asset"
    );
}

#[cfg(unix)]
#[test]
fn moved_and_external_server_start_scripts_keep_permissions_and_follow_the_new_java_path() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, locations, source, parent) = fixture();
    let external = temp.path().join("external server");
    fs::create_dir(&external).unwrap();
    let script=format!("#!/bin/sh\nset -eu\ncd -- \"$(dirname -- \"$0\")\"\nJAVA='{}/cache/java/bin/java'\n# user tuning kept\n",source.root.display());
    for path in [source.server().join("run.sh"), external.join("run.sh")] {
        storage::atomic_write(&path, script.as_bytes()).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    storage::write_json(&source.state("servers.json"),&serde_json::json!({"schema":1,"servers":[{"id":"local","name":"Local","target":{"kind":"local","directory":external},"knownVersion":null,"checkedAt":null,"lastError":null}]})).unwrap();
    let target = locations.destination(Some(&parent)).unwrap();
    locations
        .relocate(&source, target.clone(), &Job::default())
        .unwrap();
    for path in [target.join("server/run.sh"), external.join("run.sh")] {
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(&format!("{}/cache/java", target.display())));
        assert!(text.contains("user tuning kept"));
        assert_ne!(fs::metadata(path).unwrap().permissions().mode() & 0o111, 0);
    }
    assert!(walkdir::WalkDir::new(target.join("backups/data-location"))
        .into_iter()
        .filter_map(Result::ok)
        .any(|e| e.file_type().is_file() && fs::read(e.path()).unwrap() == script.as_bytes()));
}
#[cfg(unix)]
#[test]
fn cleanup_failure_keeps_new_location_usable_and_can_finish_after_restart() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, locations, source, parent) = fixture();
    let target = locations.destination(Some(&parent)).unwrap();
    let locked = source.game().join("saves/world");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    let outcome = locations
        .relocate(&source, target.clone(), &Job::default())
        .unwrap();
    assert!(outcome.restart);
    assert!(outcome.warning.is_some());
    assert_eq!(locations.resolve().unwrap(), target);
    assert!(locations.status().unwrap().cleanup_pending);
    fs::set_permissions(locked, fs::Permissions::from_mode(0o755)).unwrap();
    let restarted = Layout::new(target.clone()).unwrap();
    let outcome = locations
        .relocate(&restarted, target.clone(), &Job::default())
        .unwrap();
    assert!(!outcome.restart);
    assert!(outcome.warning.is_none());
    assert!(!locations.status().unwrap().cleanup_pending);
    assert!(!source.root.exists());
}

#[cfg(unix)]
#[test]
fn relocation_refuses_a_linked_state_directory_without_writing_outside_the_data_root() {
    let (temp, locations, source, parent) = fixture();
    let external = temp.path().join("outside state");
    fs::create_dir(&external).unwrap();
    fs::remove_dir_all(source.root.join("state")).unwrap();
    std::os::unix::fs::symlink(&external, source.root.join("state")).unwrap();
    let target = locations.destination(Some(&parent)).unwrap();
    assert!(locations
        .relocate(&source, target, &Job::default())
        .is_err());
    assert!(!external.join("data-identity.json").exists());
    assert_eq!(
        fs::read(source.game().join("saves/world/level.dat")).unwrap(),
        b"world"
    );
}
#[test]
fn retry_replaces_readonly_staged_files_when_the_original_content_changes() {
    let (_temp, locations, source, parent) = fixture();
    let target = locations.destination(Some(&parent)).unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let cancel = cancellation.clone();
    let mut job = tracked_job(move |p| {
        if p.phases
            .iter()
            .any(|p| p.id == "data-copy" && p.completed > 0)
        {
            cancel.cancel();
        }
    });
    job.cancel = cancellation;
    assert!(locations.relocate(&source, target.clone(), &job).is_err());
    let staged = locations
        .pending()
        .unwrap()
        .unwrap()
        .staging()
        .unwrap()
        .join("cache/minecraft/assets/objects/aa/blob");
    storage::atomic_write(&staged, b"old staged bytes").unwrap();
    let mut permissions = fs::metadata(&staged).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&staged, permissions).unwrap();
    storage::atomic_write(
        &source.root.join("cache/minecraft/assets/objects/aa/blob"),
        b"updated original",
    )
    .unwrap();
    locations
        .relocate(&source, target.clone(), &Job::default())
        .unwrap();
    assert_eq!(
        fs::read(target.join("cache/minecraft/assets/objects/aa/blob")).unwrap(),
        b"updated original"
    );
}

#[test]
fn startup_script_rebinding_handles_a_sibling_folder_with_the_old_name_as_a_prefix() {
    let (_temp, locations, source, _parent) = fixture();
    let sibling = source
        .root
        .parent()
        .unwrap()
        .join("AuramLauncher relocated");
    fs::create_dir(&sibling).unwrap();
    let script = format!(
        "#!/bin/sh\nset -eu\ncd -- \"$(dirname -- \"$0\")\"\nJAVA='{}/cache/java/bin/java'\n",
        source.root.display()
    );
    storage::atomic_write(&source.server().join("run.sh"), script.as_bytes()).unwrap();
    let target = locations.destination(Some(&sibling)).unwrap();
    locations
        .relocate(&source, target.clone(), &Job::default())
        .unwrap();
    assert_eq!(
        fs::read_to_string(target.join("server/run.sh")).unwrap(),
        format!(
            "#!/bin/sh\nset -eu\ncd -- \"$(dirname -- \"$0\")\"\nJAVA='{}/cache/java/bin/java'\n",
            target.display()
        )
    );
}
