use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

pub fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.is_empty() || relative.contains('\\') {
        bail!("Invalid relative path: {relative}");
    }
    let mut out = root.to_path_buf();
    for part in Path::new(relative).components() {
        let Component::Normal(name) = part else {
            bail!("Unsafe path: {relative}");
        };
        let name = name.to_str().context("Non-UTF8 path")?;
        let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        if name.ends_with(['.', ' '])
            || name.chars().any(|c| c < ' ' || ":<>\"|?*".contains(c))
            || ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            bail!("Unsupported path component: {name}");
        }
        out.push(name);
        match fs::symlink_metadata(&out) {
            Ok(meta) if meta.file_type().is_symlink() => {
                bail!("Refusing symlink: {}", out.display())
            }
            #[cfg(windows)]
            Ok(meta)
                if {
                    use std::os::windows::fs::MetadataExt;
                    meta.file_attributes() & 0x400 != 0
                } =>
            {
                bail!("Refusing reparse point: {}", out.display())
            }
            Err(e)
                if !matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Err(e.into())
            }
            _ => {}
        }
    }
    Ok(out)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(path.parent().context("Path has no parent")?)?;
    let temporary = path.with_extension(format!("{}.tmp", rand::random::<u64>()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
pub fn atomic_copy(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination.parent().context("Destination has no parent")?)?;
    let temporary = destination.with_extension(format!("{}.tmp", rand::random::<u64>()));
    let result = (|| {
        let mut input = fs::File::open(source)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        fs::set_permissions(&temporary, input.metadata()?.permissions())?;
        drop(output);
        fs::rename(&temporary, destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?)
}
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(
        &fs::read(path).with_context(|| format!("Read {}", path.display()))?,
    )?)
}
pub fn read_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(data) => {
            Ok(Some(serde_json::from_slice(&data).with_context(|| {
                format!("Invalid state: {}", path.display())
            })?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub root: PathBuf,
}
impl Layout {
    pub fn discover() -> Result<Self> {
        let base = directories::BaseDirs::new().context("Unable to locate application data")?;
        let root = if cfg!(feature = "qa") {
            PathBuf::from(
                std::env::var_os("AURAM_QA_DATA_DIR")
                    .context("QA builds require an isolated AURAM_QA_DATA_DIR")?,
            )
        } else if cfg!(debug_assertions) {
            std::env::var_os("AURAM_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| base.data_local_dir().join("AuramLauncherDev"))
        } else {
            base.data_local_dir().join("AuramLauncher")
        };
        Self::new(root)
    }
    pub fn new(root: PathBuf) -> Result<Self> {
        let root = if root.is_absolute() {
            root
        } else {
            std::env::current_dir()?.join(root)
        };
        for name in [
            "cache",
            "downloads",
            "client",
            "server",
            "state",
            "logs",
            "backups",
        ] {
            fs::create_dir_all(root.join(name))?;
        }
        Ok(Self { root })
    }
    pub fn client(&self) -> PathBuf {
        self.root.join("client")
    }
    pub fn game(&self) -> PathBuf {
        self.client().join("minecraft")
    }
    pub fn server(&self) -> PathBuf {
        self.root.join("server")
    }
    pub fn runtime(&self) -> PathBuf {
        self.root.join("cache/minecraft")
    }
    pub fn state(&self, name: &str) -> PathBuf {
        self.root.join("state").join(name)
    }
    pub fn legacy_candidates() -> Vec<PathBuf> {
        let Some(base) = directories::BaseDirs::new() else {
            return vec![];
        };
        #[cfg(target_os = "macos")]
        return vec![base.data_dir().join("art.arcane.auram.auramLauncher/Auram")];
        #[cfg(target_os = "windows")]
        return vec![
            base.data_dir()
                .join("art.arcane.auram/auram_launcher/Auram"),
            base.data_dir().join("art.arcane.auram/Auram/Auram"),
        ];
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = base;
            vec![]
        }
    }
    pub fn migrate(&self, source: &Path) -> Result<()> {
        let marker = self.state("migration.json");
        if read_optional::<serde_json::Value>(&marker)?.is_some() {
            return Ok(());
        }
        let old_game = source.join("minecraft/minecraft");
        if !old_game.is_dir() {
            bail!("Legacy game folder not found: {}", old_game.display());
        }
        copy_missing(
            &old_game,
            &self.game(),
            &["assets", "libraries", "versions", "natives"],
        )?;
        for name in ["assets", "libraries", "versions"] {
            copy_missing(&old_game.join(name), &self.runtime().join(name), &[])?;
        }
        copy_missing(&source.join("minecraft/server"), &self.server(), &[])?;
        for name in ["mmc-pack.json", "instance.cfg"] {
            let from = source.join("minecraft").join(name);
            let to = self.client().join(name);
            if from.is_file() && !to.exists() {
                atomic_write(&to, &fs::read(from)?)?;
            }
        }
        // Java and imported resource bytes are verified by their provisioners before reuse.
        copy_missing(
            &source.join("jvm"),
            &self.root.join("cache/java/imported"),
            &[],
        )?;
        write_json(
            &marker,
            &serde_json::json!({"schema":1,"source":source,"completed":chrono::Utc::now()}),
        )
    }
}
pub fn copy_missing(source: &Path, destination: &Path, exclude: &[&str]) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0 || !exclude.contains(&e.file_name().to_str().unwrap_or(""))
        })
    {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            continue;
        }
        let relative = entry.path().strip_prefix(source)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let relative = relative
            .to_str()
            .context("Non-UTF8 legacy path")?
            .replace('\\', "/");
        let target = safe_join(destination, &relative)?;
        if entry.file_type().is_dir() {
            fs::create_dir_all(target)?;
        } else if !target.exists() {
            atomic_copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub schema: u32,
    pub memory_mib: u64,
    pub microsoft_client_id: String,
    pub browser_login: bool,
}
impl Default for Settings {
    fn default() -> Self {
        let memory = sysinfo::System::new_all().total_memory() / 1024 / 1024;
        Self {
            schema: 1,
            memory_mib: if memory == 0 {
                4096
            } else {
                (memory / 2).clamp(2048, 8192)
            },
            microsoft_client_id: "cf95d5d7-7830-4ad4-a70e-d54e50e80b40".into(),
            browser_login: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedPath {
    relative: String,
    existed: bool,
}
#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    schema: u32,
    ready: bool,
    paths: Vec<SavedPath>,
}

pub fn recover(root: &Path, transaction: &Path) -> Result<()> {
    let journal_path = transaction.join("journal.json");
    let Some(journal) = read_optional::<Journal>(&journal_path)? else {
        return Ok(());
    };
    if journal.schema != 1 {
        bail!("Unsupported recovery journal schema");
    }
    if journal.ready {
        // Remove new files before restoring old files that may occupy their parents.
        for entry in journal.paths.iter().rev().filter(|entry| !entry.existed) {
            let target = safe_join(root, &entry.relative)?;
            if target.is_dir() {
                // Never remove user-owned contents to make a rollback fit.
                match fs::remove_dir(&target) {
                    Ok(()) => {}
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                        ) => {}
                    Err(e) => return Err(e.into()),
                }
            } else {
                match fs::remove_file(&target) {
                    Ok(()) => {}
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                        ) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            remove_empty_parents(root, &target);
        }
        for entry in &journal.paths {
            let target = safe_join(root, &entry.relative)?;
            if entry.existed {
                if target.is_dir() {
                    fs::remove_dir(&target)?;
                }
                atomic_copy(
                    &safe_join(&transaction.join("original"), &entry.relative)?,
                    &target,
                )?;
            }
        }
    }
    fs::remove_dir_all(transaction)?;
    Ok(())
}

pub fn transact(
    root: &Path,
    transaction: &Path,
    changes: &[(String, Option<PathBuf>)],
) -> Result<()> {
    recover(root, transaction)?;
    if transaction.exists() {
        fs::remove_dir_all(transaction)?;
    }
    fs::create_dir_all(transaction)?;
    let mut journal = Journal {
        schema: 1,
        ready: false,
        paths: vec![],
    };
    for (relative, _) in changes {
        let target = safe_join(root, relative)?;
        let existed = match fs::metadata(&target) {
            Ok(metadata) => metadata.is_file(),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                false
            }
            Err(error) => return Err(error.into()),
        };
        if existed {
            atomic_copy(
                &target,
                &safe_join(&transaction.join("original"), relative)?,
            )?;
        }
        journal.paths.push(SavedPath {
            relative: relative.clone(),
            existed,
        });
    }
    journal.ready = true;
    write_json(&transaction.join("journal.json"), &journal)?;
    let apply = (|| {
        let mut ordered = changes.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|(path, source)| {
            (
                path == ".auram-manifest.json",
                source.is_some(),
                if source.is_none() {
                    std::cmp::Reverse(path.matches('/').count())
                } else {
                    std::cmp::Reverse(0)
                },
            )
        });
        for (relative, source) in ordered {
            let target = safe_join(root, relative)?;
            if let Some(source) = source {
                if target.is_dir() {
                    fs::remove_dir(&target)?;
                }
                atomic_copy(source, &target)?;
            } else if target.try_exists()? {
                fs::remove_file(&target)?;
                remove_empty_parents(root, &target);
            }
        }
        // Removing the journal is the commit point; backups can then be cleaned independently.
        fs::remove_file(transaction.join("journal.json"))?;
        Ok(())
    })();
    if apply.is_err() {
        recover(root, transaction)?;
    } else {
        let _ = fs::remove_dir_all(transaction);
    }
    apply
}

fn remove_empty_parents(root: &Path, file: &Path) {
    let mut current = file.parent();
    while let Some(parent) = current.filter(|p| *p != root && p.starts_with(root)) {
        if fs::remove_dir(parent).is_err() {
            break;
        }
        current = parent.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transactions_handle_file_directory_transitions_and_preserve_blocking_user_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("client");
        let tx = temp.path().join("tx");
        let replacement = temp.path().join("replacement");
        atomic_write(&replacement, b"new").unwrap();
        let manifest = temp.path().join("next-manifest");
        atomic_write(&manifest, b"new baseline").unwrap();
        atomic_write(&root.join("item"), b"old file").unwrap();
        atomic_write(&root.join(".auram-manifest.json"), b"old baseline").unwrap();
        transact(
            &root,
            &tx,
            &[
                ("item/a/b".into(), Some(replacement.clone())),
                ("item".into(), None),
            ],
        )
        .unwrap();
        assert_eq!(fs::read(root.join("item/a/b")).unwrap(), b"new");
        atomic_write(&root.join("item/user.txt"), b"user").unwrap();
        let changes = vec![
            ("item".into(), Some(replacement.clone())),
            ("item/a/b".into(), None),
            (".auram-manifest.json".into(), Some(manifest)),
        ];
        assert!(transact(&root, &tx, &changes).is_err());
        assert_eq!(fs::read(root.join("item/a/b")).unwrap(), b"new");
        assert_eq!(fs::read(root.join("item/user.txt")).unwrap(), b"user");
        assert_eq!(
            fs::read(root.join(".auram-manifest.json")).unwrap(),
            b"old baseline"
        );
        fs::remove_file(root.join("item/user.txt")).unwrap();
        // The now-empty managed parent directory is removed without recursive deletion.
        fs::remove_file(root.join("item/a/b")).unwrap();
        fs::remove_dir(root.join("item/a")).unwrap();
        atomic_write(&root.join("item/owned"), b"old").unwrap();
        transact(
            &root,
            &tx,
            &[
                ("item".into(), Some(replacement)),
                ("item/owned".into(), None),
            ],
        )
        .unwrap();
        assert_eq!(fs::read(root.join("item")).unwrap(), b"new");
    }
    #[test]
    fn interrupted_file_to_directory_update_restores_original_parent_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("client");
        let tx = temp.path().join("tx");
        atomic_write(&root.join("item/a/b"), b"new").unwrap();
        atomic_write(&tx.join("original/item"), b"original").unwrap();
        write_json(
            &tx.join("journal.json"),
            &Journal {
                schema: 1,
                ready: true,
                paths: vec![
                    SavedPath {
                        relative: "item".into(),
                        existed: true,
                    },
                    SavedPath {
                        relative: "item/a/b".into(),
                        existed: false,
                    },
                ],
            },
        )
        .unwrap();
        recover(&root, &tx).unwrap();
        recover(&root, &tx).unwrap();
        assert_eq!(fs::read(root.join("item")).unwrap(), b"original");
    }
    #[test]
    fn atomic_copy_preserves_readonly_files_without_reopening_for_write() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("readonly");
        fs::write(&source, b"Java license").unwrap();
        let mut permissions = fs::metadata(&source).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&source, permissions).unwrap();
        let target = temp.path().join("copy");
        atomic_copy(&source, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"Java license");
        assert!(fs::metadata(&target).unwrap().permissions().readonly());
    }
    #[test]
    fn paths_reject_traversal_windows_names_and_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        for bad in [
            "../outside",
            "/absolute",
            "a/../../b",
            "C:/x",
            "a\\b",
            "CON.txt",
            "a/COM1",
            "name.",
            "name ",
            "a:b",
        ] {
            assert!(safe_join(temp.path(), bad).is_err(), "{bad}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp", temp.path().join("link")).unwrap();
            assert!(safe_join(temp.path(), "link/a").is_err());
        }
    }
    #[test]
    fn interrupted_transaction_restores_originals_and_removes_new_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("instance");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("old"), b"updated").unwrap();
        fs::write(root.join("new"), b"new").unwrap();
        let tx = temp.path().join("tx");
        atomic_write(&tx.join("original/old"), b"original").unwrap();
        write_json(
            &tx.join("journal.json"),
            &Journal {
                schema: 1,
                ready: true,
                paths: vec![
                    SavedPath {
                        relative: "old".into(),
                        existed: true,
                    },
                    SavedPath {
                        relative: "new".into(),
                        existed: false,
                    },
                ],
            },
        )
        .unwrap();
        recover(&root, &tx).unwrap();
        assert_eq!(fs::read(root.join("old")).unwrap(), b"original");
        assert!(!root.join("new").exists());
    }
    #[test]
    fn migration_is_repeatable_and_preserves_custom_data() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("legacy");
        atomic_write(
            &source.join("minecraft/minecraft/saves/world/level.dat"),
            b"world",
        )
        .unwrap();
        atomic_write(
            &source.join("minecraft/minecraft/assets/objects/ab/hash"),
            b"asset",
        )
        .unwrap();
        atomic_write(&source.join("minecraft/server/world/level.dat"), b"server").unwrap();
        let layout = Layout::new(temp.path().join("new")).unwrap();
        layout.migrate(&source).unwrap();
        atomic_write(&layout.game().join("saves/world/level.dat"), b"modified").unwrap();
        layout.migrate(&source).unwrap();
        assert_eq!(
            fs::read(layout.game().join("saves/world/level.dat")).unwrap(),
            b"modified"
        );
        assert!(layout.runtime().join("assets/objects/ab/hash").exists());
        assert!(layout.server().join("world/level.dat").exists());
        assert!(source.exists());
    }
    #[test]
    fn failed_application_rolls_back_all_originals() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("existing"), b"original").unwrap();
        let staged = temp.path().join("staged");
        fs::write(&staged, b"new").unwrap();
        let changes = vec![
            ("existing".into(), Some(staged)),
            ("missing".into(), Some(temp.path().join("not-present"))),
        ];
        assert!(transact(&root, &temp.path().join("tx"), &changes).is_err());
        assert_eq!(fs::read(root.join("existing")).unwrap(), b"original");
        assert!(!root.join("missing").exists());
    }
}
