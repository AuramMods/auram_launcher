use super::*;
use crate::pack::{PackFile, PackTag};
use sha1::Digest;
use std::collections::BTreeMap;

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    blobs: PathBuf,
    old: Manifest,
    desired: Manifest,
    changes: Vec<(String, Option<PathBuf>)>,
}
impl Fixture {
    fn new(server: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("server 世界");
        let blobs = temp.path().join("blobs");
        fs::create_dir_all(&root).unwrap();
        let old = Manifest {
            schema: 1,
            policy: 0,
            tag: PackTag {
                name: "1".into(),
                sha: "a".repeat(40),
            },
            files: BTreeMap::new(),
        };
        let mut desired = Manifest {
            tag: PackTag {
                name: "2".into(),
                sha: "b".repeat(40),
            },
            ..old.clone()
        };
        for directory in directories(server) {
            storage::atomic_write(
                &root.join(&directory).join("custom.txt"),
                b"original user data",
            )
            .unwrap();
            let body = format!("target {directory}").into_bytes();
            let sha = hex::encode(sha1::Sha1::digest(
                [format!("blob {}\0", body.len()).as_bytes(), &body].concat(),
            ));
            storage::atomic_write(&blobs.join(&sha), &body).unwrap();
            desired.files.insert(
                format!("{directory}/new.txt"),
                PackFile {
                    sha,
                    size: body.len() as u64,
                    mode: "100644".into(),
                    source: None,
                },
            );
        }
        let outside = if server {
            "server-icon.png"
        } else {
            "mmc-pack.json"
        };
        let body = b"new managed metadata";
        let sha = hex::encode(sha1::Sha1::digest(
            [format!("blob {}\0", body.len()).as_bytes(), body].concat(),
        ));
        storage::atomic_write(&blobs.join(&sha), body).unwrap();
        storage::atomic_write(&root.join(outside), b"original metadata").unwrap();
        desired.files.insert(
            outside.into(),
            PackFile {
                sha: sha.clone(),
                size: body.len() as u64,
                mode: "100644".into(),
                source: None,
            },
        );
        for path in [
            "world/level.dat",
            "server.properties",
            "ops.json",
            "eula.txt",
            "minecraft/saves/world/level.dat",
            "minecraft/options.txt",
        ] {
            storage::atomic_write(&root.join(path), b"preserved").unwrap();
        }
        storage::write_json(&root.join(".auram-manifest.json"), &old).unwrap();
        Self {
            _temp: temp,
            root,
            blobs: blobs.clone(),
            old,
            desired,
            changes: vec![(outside.into(), Some(blobs.join(sha)))],
        }
    }
    fn assert_state(&self, server: bool, committed: bool) {
        let manifest: Manifest =
            storage::read_json(&self.root.join(".auram-manifest.json")).unwrap();
        assert_eq!(
            manifest.tag.sha,
            if committed {
                &self.desired.tag.sha
            } else {
                &self.old.tag.sha
            }
            .clone()
        );
        for directory in directories(server) {
            assert_eq!(
                self.root.join(&directory).join("new.txt").is_file(),
                committed
            );
            if committed {
                assert!(!self.root.join(&directory).join("custom.txt").exists());
            } else {
                assert_eq!(
                    fs::read(self.root.join(&directory).join("custom.txt")).unwrap(),
                    b"original user data"
                );
            }
        }
        for path in [
            "world/level.dat",
            "server.properties",
            "ops.json",
            "eula.txt",
            "minecraft/saves/world/level.dat",
            "minecraft/options.txt",
        ] {
            assert_eq!(fs::read(self.root.join(path)).unwrap(), b"preserved");
        }
    }
}

#[test]
fn every_directory_and_manifest_activation_boundary_recovers_after_termination() {
    for server in [false, true] {
        let probe = Fixture::new(server);
        let labels = std::cell::RefCell::new(Vec::new());
        apply_checked(
            &probe.root,
            &probe.desired,
            &probe.desired,
            &probe.changes,
            &probe.blobs,
            server,
            &Job::default(),
            &|label| {
                labels.borrow_mut().push(label.to_owned());
                Ok(())
            },
        )
        .unwrap();
        probe.assert_state(server, true);
        for stop in labels.into_inner() {
            let f = Fixture::new(server);
            let terminated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                apply_checked(
                    &f.root,
                    &f.desired,
                    &f.desired,
                    &f.changes,
                    &f.blobs,
                    server,
                    &Job::default(),
                    &|label| {
                        if label == stop {
                            panic!("simulated termination at {label}");
                        }
                        Ok(())
                    },
                )
            }));
            assert!(terminated.is_err());
            let committed = stop == "after-commit";
            assert_eq!(recover(&f.root).unwrap(), !committed);
            assert!(!recover(&f.root).unwrap());
            f.assert_state(server, committed);
        }
    }
}

#[test]
fn failed_staging_or_cancel_preserves_the_original_without_an_active_journal() {
    let f = Fixture::new(false);
    let file = f.desired.files.values().next().unwrap();
    fs::write(f.blobs.join(&file.sha), b"damaged").unwrap();
    assert!(apply(
        &f.root,
        &f.desired,
        &f.desired,
        &f.changes,
        &f.blobs,
        false,
        &Job::default()
    )
    .is_err());
    f.assert_state(false, false);
    assert!(!f.root.join(JOURNAL).exists());
    let f = Fixture::new(true);
    let job = Job::default();
    job.cancel.cancel();
    assert!(apply(&f.root, &f.desired, &f.desired, &f.changes, &f.blobs, true, &job).is_err());
    f.assert_state(true, false);
}

#[test]
fn actual_activation_error_rolls_back_all_previously_replaced_folders() {
    let f = Fixture::new(false);
    let error = apply_checked(
        &f.root,
        &f.desired,
        &f.desired,
        &f.changes,
        &f.blobs,
        false,
        &Job::default(),
        &|label| {
            if label == "after-activate-1" {
                return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into());
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(error.downcast_ref::<std::io::Error>().is_some());
    f.assert_state(false, false);
    assert!(!f.root.join(JOURNAL).exists());
}

#[test]
fn forged_directory_scope_and_traversal_cannot_mutate_other_folders() {
    let f = Fixture::new(true);
    for relative in ["world", "../outside"] {
        let journal = Journal {
            schema: 1,
            id: "0123456789abcdef".into(),
            server: true,
            paths: vec![Saved {
                relative: relative.into(),
                existed: false,
                directory: true,
                install: true,
            }],
        };
        storage::write_json(&f.root.join(JOURNAL), &journal).unwrap();
        assert!(recover(&f.root).is_err());
        assert_eq!(
            fs::read(f.root.join("world/level.dat")).unwrap(),
            b"preserved"
        );
    }
}

#[test]
fn scoped_replacement_handles_owned_path_transitions_and_keeps_unknown_contents() {
    for blocked in [false, true] {
        let f = Fixture::new(false);
        let changes = vec![
            ("minecraft/extension".into(), f.changes[0].1.clone()),
            ("minecraft/other/new.txt".into(), f.changes[0].1.clone()),
            ("minecraft/extension/old.txt".into(), None),
            ("minecraft/other".into(), None),
        ];
        storage::atomic_write(&f.root.join("minecraft/extension/old.txt"), b"owned").unwrap();
        storage::atomic_write(&f.root.join("minecraft/other"), b"owned file").unwrap();
        if blocked {
            storage::atomic_write(&f.root.join("minecraft/extension/user.txt"), b"user").unwrap();
        }
        let outcome = apply(
            &f.root,
            &f.desired,
            &f.desired,
            &changes,
            &f.blobs,
            false,
            &Job::default(),
        );
        if blocked {
            assert!(outcome.is_err());
            assert_eq!(
                fs::read(f.root.join("minecraft/extension/old.txt")).unwrap(),
                b"owned"
            );
            assert_eq!(
                fs::read(f.root.join("minecraft/extension/user.txt")).unwrap(),
                b"user"
            );
            assert_eq!(
                fs::read(f.root.join("minecraft/other")).unwrap(),
                b"owned file"
            );
            f.assert_state(false, false);
        } else {
            outcome.unwrap();
            assert_eq!(
                fs::read(f.root.join("minecraft/extension")).unwrap(),
                b"new managed metadata"
            );
            assert_eq!(
                fs::read(f.root.join("minecraft/other/new.txt")).unwrap(),
                b"new managed metadata"
            );
            f.assert_state(false, true);
        }
    }
}
