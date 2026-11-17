//! Verified, restartable relocation of launcher-owned data across volumes.
//! The bootstrap pointer stays in a stable per-profile directory. No live data
//! is removed until a complete copy has been verified and selected atomically.
use crate::{
    jobs::Job,
    local_work,
    storage::{self, Layout},
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const IDENTITY: &str = "state/data-identity.json";
const MARKER: &str = "state/data-location-move.json";
const SNAPSHOT: &str = "state/data-move-snapshot.json";
#[derive(Clone)]
pub struct Locations {
    pub default_root: PathBuf,
    pub control: PathBuf,
    pub folder_name: String,
}
#[derive(Default, Serialize, Deserialize)]
struct Selection {
    schema: u32,
    root: Option<PathBuf>,
    identity: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    schema: u32,
    id: String,
    identity: String,
    source: PathBuf,
    target: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Content {
    Directory,
    File { size: u64, sha256: String },
    Link { target: PathBuf, directory: bool },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Entry {
    path: String,
    content: Content,
}
#[derive(Serialize, Deserialize)]
struct Snapshot {
    schema: u32,
    id: String,
    source: Vec<Entry>,
    target: Vec<Entry>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationStatus {
    pub current_path: String,
    pub default_path: String,
    pub folder_name: String,
    pub is_default: bool,
    pub pending_path: Option<String>,
    pub cleanup_pending: bool,
    pub unavailable: Option<String>,
    pub notice: Option<String>,
}
pub struct MoveOutcome {
    pub restart: bool,
    pub warning: Option<String>,
}
impl Pending {
    fn staging(&self) -> Result<PathBuf> {
        Ok(self
            .target
            .parent()
            .context("Destination has no parent")?
            .join(format!(".auram-data-move-{}", self.id)))
    }
}
impl Locations {
    pub fn discover() -> Result<Self> {
        let base = directories::BaseDirs::new().context("Unable to locate application data")?;
        let folder_name = if cfg!(feature = "qa") {
            "AuramLauncherQA"
        } else if cfg!(debug_assertions) {
            "AuramLauncherDev"
        } else {
            "AuramLauncher"
        };
        let default_root = if cfg!(feature = "qa") {
            PathBuf::from(
                std::env::var_os("AURAM_QA_DATA_DIR")
                    .context("QA builds require an isolated AURAM_QA_DATA_DIR")?,
            )
        } else if cfg!(debug_assertions) {
            std::env::var_os("AURAM_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| base.data_local_dir().join(folder_name))
        } else {
            base.data_local_dir().join(folder_name)
        };
        let default_root = if default_root.is_absolute() {
            default_root
        } else {
            std::env::current_dir()?.join(default_root)
        };
        let basename = default_root
            .file_name()
            .context("Default data directory has no name")?
            .to_string_lossy();
        let control = default_root
            .parent()
            .context("Default data directory has no parent")?
            .join(format!(".{basename}-location"));
        Ok(Self {
            default_root,
            control,
            folder_name: folder_name.into(),
        })
    }
    fn selection(&self) -> Result<Selection> {
        let selected = storage::read_optional::<Selection>(&self.control.join("location.json"))?
            .unwrap_or(Selection {
                schema: 1,
                ..Default::default()
            });
        if selected.schema != 1
            || selected
                .root
                .as_ref()
                .is_some_and(|root| !root.is_absolute())
            || selected.identity.as_ref().is_some_and(|id| !valid_id(id))
        {
            bail!("Unsupported data-location settings");
        }
        Ok(selected)
    }
    fn pending(&self) -> Result<Option<Pending>> {
        let pending = storage::read_optional::<Pending>(&self.control.join("move.json"))?;
        if let Some(p) = &pending {
            if p.schema != 1
                || !valid_id(&p.id)
                || !valid_id(&p.identity)
                || !p.source.is_absolute()
                || !p.target.is_absolute()
                || overlaps(&p.source, &p.target)
                || overlaps(&p.target, &self.control)
            {
                bail!("Invalid data relocation journal; existing data has been retained");
            }
        }
        Ok(pending)
    }
    pub fn resolve(&self) -> Result<PathBuf> {
        let selection = self.selection()?;
        let Some(root) = selection.root else {
            return Ok(self.default_root.clone());
        };
        fs::read_dir(&root).with_context(||format!("Data folder unavailable: {}. Reconnect its drive or reset the location in Settings.",root.display()))?;
        if let Some(identity) = selection.identity {
            if storage::read_optional::<String>(&root.join(IDENTITY))?.as_deref() != Some(&identity)
            {
                bail!(
                    "The selected drive does not contain the expected Auram data folder: {}",
                    root.display()
                );
            }
        }
        Ok(root)
    }
    pub fn status(&self) -> Result<LocationStatus> {
        let selection = self.selection()?;
        let current = selection.root.unwrap_or_else(|| self.default_root.clone());
        let pending = self.pending()?;
        Ok(LocationStatus {
            current_path: current.to_string_lossy().into_owned(),
            default_path: self.default_root.to_string_lossy().into_owned(),
            folder_name: self.folder_name.clone(),
            is_default: same_path(&current, &self.default_root),
            pending_path: pending
                .as_ref()
                .map(|p| p.target.to_string_lossy().into_owned()),
            cleanup_pending: pending
                .as_ref()
                .is_some_and(|p| same_path(&p.target, &current)),
            unavailable: self.resolve().err().map(|e| format!("{e:#}")),
            notice: storage::read_optional::<Option<String>>(&self.control.join("notice.json"))?
                .flatten(),
        })
    }
    pub fn destination(&self, parent: Option<&Path>) -> Result<PathBuf> {
        match parent {
            Some(parent) => {
                fs::read_dir(parent).context("Open the selected parent directory")?;
                Ok(normalize(fs::canonicalize(parent)?).join(&self.folder_name))
            }
            None => Ok(self.default_root.clone()),
        }
    }
    pub fn retry_destination(&self) -> Result<PathBuf> {
        Ok(self.pending()?.context("No unfinished data move")?.target)
    }
    pub fn cancel(&self) -> Result<()> {
        let Some(pending) = self.pending()? else {
            return Ok(());
        };
        if self
            .selection()?
            .root
            .as_ref()
            .is_some_and(|root| same_path(root, &pending.target))
        {
            bail!("The data location has already changed; finish the remaining cleanup instead");
        }
        let staging = pending.staging()?;
        if let Ok(metadata) = fs::symlink_metadata(&staging) {
            if !metadata.is_dir() || reparse(&metadata) {
                bail!("The relocation staging directory was replaced; it has been retained");
            }
            // A private copy is retained rather than deleting files changed after interruption.
            fs::rename(&staging, staging.with_extension("cancelled"))?;
        }
        if fs::symlink_metadata(&pending.target).is_ok()
            && self.check_candidate(&pending.target, &pending).is_ok()
        {
            let backup = pending
                .target
                .parent()
                .unwrap()
                .join(format!(".auram-data-cancelled-{}", pending.id));
            fs::rename(&pending.target, backup)?;
        }
        fs::remove_file(self.control.join("move.json"))?;
        Ok(())
    }
    /// Explicit reset when the selected drive is unavailable. Never removes or
    /// invents a replacement for that drive's data; the default starts separately.
    pub fn reset_unavailable(&self) -> Result<()> {
        if self.resolve().is_ok() {
            bail!("The current data folder is available; move its files normally");
        }
        if self.pending()?.is_some() {
            bail!("Reconnect the drive to finish or cancel its unfinished data move");
        }
        storage::write_json(
            &self.control.join("location.json"),
            &Selection {
                schema: 1,
                root: None,
                identity: None,
            },
        )
    }
    pub fn relocate(&self, source: &Layout, target: PathBuf, job: &Job) -> Result<MoveOutcome> {
        if let Some(p) = self.pending()? {
            if self
                .selection()?
                .root
                .as_ref()
                .is_some_and(|root| same_path(root, &p.target))
            {
                let warning = self.cleanup(&p, job).unwrap_or_else(|e| {
                    Some(format!(
                        "The data location is active; old-file cleanup can be retried: {e:#}"
                    ))
                });
                storage::write_json(&self.control.join("notice.json"), &warning)?;
                return Ok(MoveOutcome {
                    restart: false,
                    warning,
                });
            }
        }
        if same_path(&source.root, &target) {
            return Ok(MoveOutcome {
                restart: false,
                warning: None,
            });
        }
        job.check()?;
        let pending = match self.pending()? {
            Some(p) if same_path(&p.source, &source.root) && same_path(&p.target, &target) => p,
            Some(_) => bail!("Finish or cancel the previous data move first"),
            None => {
                self.validate_destination(source, &target)?;
                let identity = storage::read_optional::<String>(&source.root.join(IDENTITY))?
                    .unwrap_or_else(|| format!("{:032x}", rand::random::<u128>()));
                if !valid_id(&identity) {
                    bail!("Invalid Auram data-folder identity");
                }
                storage::write_json(&join(&source.root, IDENTITY)?, &identity)?;
                let p = Pending {
                    schema: 1,
                    id: format!("{:032x}", rand::random::<u128>()),
                    identity,
                    source: source.root.clone(),
                    target,
                };
                storage::write_json(&self.control.join("move.json"), &p)?;
                p
            }
        };
        let staging = pending.staging()?;
        if !staging.try_exists()? && self.check_candidate(&pending.target, &pending).is_ok() {
            // A crash can occur between publishing the directory and committing
            // the pointer. Only adopt that exact, fully verified prepared copy.
            job.phase("data-verify", "Verify copy", true);
            self.check_candidate(&pending.target, &pending)?;
            self.verify_snapshot(&pending, &pending.target, job)?;
        } else {
            job.phase("data-plan", "Inspect files", false);
            job.progress("data-plan", "Inspecting existing launcher data", 0, 0);
            let source_entries = scan(&pending.source, job)?;
            match fs::create_dir(&staging) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let meta = fs::symlink_metadata(&staging)?;
                    if !meta.is_dir() || reparse(&meta) {
                        bail!("Invalid relocation staging directory");
                    }
                }
                Err(error) => return Err(error.into()),
            }
            self.copy(&pending, &staging, &source_entries, job)?;
            job.phase("data-verify", "Verify copy", true);
            transform(source, &staging, &pending.target)?;
            storage::write_json(&staging.join(MARKER), &pending.id)?;
            let target_entries = scan(&staging, job)?;
            let snapshot = Snapshot {
                schema: 1,
                id: pending.id.clone(),
                source: source_entries,
                target: target_entries,
            };
            storage::write_json(&staging.join(SNAPSHOT), &snapshot)?;
            self.verify_snapshot(&pending, &staging, job)?;
            job.check()?;
            job.phase("data-activate", "Activate location", false);
            job.progress("data-activate", "Selecting the verified data folder", 0, 0);
            if pending.target.try_exists()? {
                fs::remove_dir(&pending.target)
                    .context("The destination is no longer empty; existing files were retained")?;
            }
            fs::rename(&staging, &pending.target)
                .context("Publish the verified copy on its destination drive")?;
            sync_parent(&pending.target)?;
        }
        // Cancellation is deferred from this point, just as in pack activation.
        let selected = Selection {
            schema: 1,
            root: Some(pending.target.clone()),
            identity: Some(pending.identity.clone()),
        };
        if let Err(error) = storage::write_json(&self.control.join("location.json"), &selected) {
            // A failed acknowledgement must not switch back after a real commit.
            if !self
                .selection()
                .ok()
                .and_then(|s| s.root)
                .is_some_and(|root| same_path(&root, &pending.target))
            {
                return Err(error);
            }
        }
        let warning = self.cleanup(&pending, job).unwrap_or_else(|e| {
            Some(format!(
                "The new data folder is active. Old-file cleanup can be retried: {e:#}"
            ))
        });
        // A notice-write failure cannot undo a committed location change.
        let _ = storage::write_json(&self.control.join("notice.json"), &warning);
        Ok(MoveOutcome {
            restart: true,
            warning,
        })
    }
    fn validate_destination(&self, source: &Layout, target: &Path) -> Result<()> {
        check_root(&source.root)?;
        let source_real = normalize(fs::canonicalize(&source.root)?);
        let target_parent = normalize(fs::canonicalize(
            target.parent().context("Destination has no parent")?,
        )?);
        let target_real =
            target_parent.join(target.file_name().context("Destination has no name")?);
        if overlaps(&source_real, &target_real) || overlaps(&target_real, &self.control) {
            bail!("Choose a data folder outside the current data and launcher location settings");
        }
        if let Ok(executable) = std::env::current_exe() {
            if executable.starts_with(&source_real) || executable.starts_with(&target_real) {
                bail!("Keep the installed application outside its data folder; choose its location in the installer");
            }
        }
        if let Ok(meta) = fs::symlink_metadata(target) {
            if !meta.is_dir() || reparse(&meta) || fs::read_dir(target)?.next().is_some() {
                bail!("The destination {} already contains files. Choose an empty location; nothing has been overwritten.",target.display());
            }
        }
        for server in crate::server_instances::Registry::load(source)?.servers {
            if let crate::server_instances::Target::Local { directory } = server.target {
                let directory = PathBuf::from(directory);
                if overlaps(&target_real, &directory) {
                    bail!("Keep launcher data separate from saved local server directories");
                }
            }
        }
        Ok(())
    }
    fn copy(&self, pending: &Pending, staging: &Path, entries: &[Entry], job: &Job) -> Result<()> {
        job.phase("data-copy", "Copy files", true);
        // Prune only this private staging tree; live/original user data is untouched.
        let expected = paths(entries);
        let mut stale = walkdir::WalkDir::new(staging)
            .follow_links(false)
            .min_depth(1)
            .into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()?;
        stale.sort_by_key(|entry| std::cmp::Reverse(entry.depth()));
        for entry in stale {
            let relative = relative(staging, entry.path())?;
            if !expected.contains(&relative) {
                remove_leaf(entry.path(), entry.file_type().is_dir())?;
            }
        }
        for entry in entries
            .iter()
            .filter(|e| matches!(e.content, Content::Directory))
        {
            let destination = join(staging, &entry.path)?;
            if let Ok(meta) = fs::symlink_metadata(&destination) {
                if !meta.is_dir() || reparse(&meta) {
                    remove_leaf(&destination, meta.is_dir())?;
                }
            }
            fs::create_dir_all(destination)?;
        }
        let files = entries
            .iter()
            .filter(|e| !matches!(e.content, Content::Directory))
            .collect::<Vec<_>>();
        let progress =
            local_work::Progress::new(job, "Copying and verifying launcher data", files.len());
        local_work::map(&files, |entry| {
            job.check()?;
            let destination = join(staging, &entry.path)?;
            match &entry.content {
                Content::File { sha256, .. } => {
                    if !fs::symlink_metadata(&destination)
                        .is_ok_and(|m| m.is_file() && !reparse(&m))
                        || !crate::download::Checksum::Sha256(sha256.clone())
                            .verify(&destination)
                            .unwrap_or(false)
                    {
                        if let Ok(meta) = fs::symlink_metadata(&destination) {
                            if !meta.is_file() || reparse(&meta) {
                                remove_leaf(&destination, meta.is_dir())?;
                            }
                        }
                        copy_staged(&join(&pending.source, &entry.path)?, &destination)?;
                    }
                    if !crate::download::Checksum::Sha256(sha256.clone()).verify(&destination)? {
                        bail!("Copied file failed verification: {}", entry.path);
                    }
                }
                Content::Link { target, directory } => {
                    let target = rebase(target, &pending.source, &pending.target);
                    if fs::read_link(&destination).ok().as_ref() != Some(&target) {
                        if let Ok(meta) = fs::symlink_metadata(&destination) {
                            remove_leaf(&destination, meta.is_dir())?;
                        }
                        symlink(&target, &destination, *directory)?;
                    }
                }
                Content::Directory => {}
            }
            progress.complete();
            Ok(())
        })?;
        Ok(())
    }
    fn check_candidate(&self, root: &Path, p: &Pending) -> Result<()> {
        check_root(root)?;
        if storage::read_optional::<String>(&root.join(MARKER))?.as_deref() != Some(&p.id)
            || storage::read_optional::<String>(&root.join(IDENTITY))?.as_deref()
                != Some(&p.identity)
        {
            bail!("The prepared data folder changed; both original and destination files have been retained");
        }
        Ok(())
    }
    fn verify_snapshot(&self, p: &Pending, root: &Path, job: &Job) -> Result<()> {
        self.check_candidate(root, p)?;
        let snapshot: Snapshot = storage::read_json(&root.join(SNAPSHOT))?;
        if snapshot.schema != 1 || snapshot.id != p.id {
            bail!("Invalid relocation snapshot");
        }
        let progress = local_work::Progress::new(
            job,
            "Verifying original and copied files",
            snapshot.source.len() + snapshot.target.len(),
        );
        for (directory, entries) in [
            (&p.source, &snapshot.source),
            (&root.to_path_buf(), &snapshot.target),
        ] {
            local_work::map(entries, |entry| {
                job.check()?;
                if !matches_entry(directory, entry)? {
                    bail!(
                        "File changed or failed verification: {}",
                        directory.join(&entry.path).display()
                    );
                }
                progress.complete();
                Ok(())
            })?;
        }
        if paths(&inventory(root, job)?) != paths(&snapshot.target)
            || paths(&inventory(&p.source, job)?) != paths(&snapshot.source)
        {
            bail!("Data changed after verification; originals and prepared copy have been retained. Cancel this move and retry.");
        }
        Ok(())
    }
    fn cleanup(&self, p: &Pending, job: &Job) -> Result<Option<String>> {
        self.check_candidate(&p.target, p)?;
        let snapshot: Snapshot = storage::read_json(&p.target.join(SNAPSHOT))?;
        match fs::symlink_metadata(&p.source) {
            Ok(_) => check_root(&p.source)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        if snapshot.schema != 1 || snapshot.id != p.id {
            bail!("Invalid cleanup snapshot");
        }
        refresh_local_server_scripts(p)?;
        job.phase("data-cleanup", "Finish move", true);
        let progress = local_work::Progress::new(
            job,
            "Removing verified originals; retaining changed files",
            snapshot.source.len(),
        );
        let mut retained = 0;
        // Do not cancel after activation: either location must stay recoverable.
        let target_entries = snapshot
            .target
            .iter()
            .map(|entry| (entry.path.as_str(), entry))
            .collect::<BTreeMap<_, _>>();
        let files = snapshot
            .source
            .iter()
            .filter(|e| !matches!(e.content, Content::Directory))
            .collect::<Vec<_>>();
        let kept = std::sync::atomic::AtomicUsize::new(0);
        local_work::map(&files, |entry| {
            let original = join(&p.source, &entry.path)?;
            if fs::symlink_metadata(&original)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                progress.complete();
                return Ok(());
            }
            let target = target_entries
                .get(entry.path.as_str())
                .context("Copied file missing from cleanup snapshot")?;
            if matches_entry(&p.source, entry)? && matches_entry(&p.target, target)? {
                let directory = matches!(
                    entry.content,
                    Content::Link {
                        directory: true,
                        ..
                    }
                );
                remove_leaf(&original, directory)?;
            } else {
                kept.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            progress.complete();
            Ok(())
        })?;
        retained += kept.load(std::sync::atomic::Ordering::Relaxed);
        let mut directories = snapshot
            .source
            .iter()
            .filter(|e| matches!(e.content, Content::Directory))
            .collect::<Vec<_>>();
        directories.sort_by_key(|e| std::cmp::Reverse(e.path.split('/').count()));
        for entry in directories {
            match fs::remove_dir(join(&p.source, &entry.path)?) {
                Ok(()) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(e) => return Err(e.into()),
            }
            progress.complete();
        }
        match fs::remove_dir(&p.source) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => retained += 1,
            Err(e) => return Err(e.into()),
        }
        fs::remove_file(self.control.join("move.json"))?;
        fs::remove_file(p.target.join(SNAPSHOT))?;
        Ok((retained > 0).then(|| {
            format!(
                "Data moved. Changed or newly added files were retained in {}",
                p.source.display()
            )
        }))
    }
}
fn check_root(root: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(root)?;
    if !meta.is_dir() || reparse(&meta) {
        bail!(
            "The data directory was replaced by a file or link: {}",
            root.display()
        );
    }
    Ok(())
}
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|c| c.is_ascii_hexdigit())
}
fn same_path(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}
fn overlaps(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        let a = PathBuf::from(a.to_string_lossy().to_lowercase());
        let b = PathBuf::from(b.to_string_lossy().to_lowercase());
        a.starts_with(&b) || b.starts_with(&a)
    }
    #[cfg(not(windows))]
    {
        a.starts_with(b) || b.starts_with(a)
    }
}
fn normalize(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let mut components = path.components();
        if let Some(Component::Prefix(prefix)) = components.next() {
            let mut out = match prefix.kind() {
                Prefix::VerbatimDisk(disk) => PathBuf::from(format!("{}:\\", disk as char)),
                Prefix::VerbatimUNC(server, share) => PathBuf::from(format!(
                    "\\\\{}\\{}",
                    server.to_string_lossy(),
                    share.to_string_lossy()
                )),
                _ => return path,
            };
            for component in components {
                if let Component::Normal(name) = component {
                    out.push(name);
                }
            }
            return out;
        }
    }
    path
}
fn reparse(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
fn relative(root: &Path, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)?
        .to_str()
        .context("Data filename is not UTF-8")?
        .replace('\\', "/"))
}
fn join(root: &Path, path: &str) -> Result<PathBuf> {
    storage::validate_relative(path)?;
    if let Some((parent, leaf)) = path.rsplit_once('/') {
        Ok(storage::safe_join(root, parent)?.join(leaf))
    } else {
        Ok(root.join(path))
    }
}
fn paths(entries: &[Entry]) -> BTreeSet<String> {
    entries.iter().map(|e| e.path.clone()).collect()
}
fn hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = sha2::Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(hex::encode(hash.finalize()))
}
fn inventory(root: &Path, job: &Job) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for entry in walkdir::WalkDir::new(root).min_depth(1).follow_links(false) {
        job.check()?;
        let entry = entry?;
        let path = relative(root, entry.path())?;
        if path == SNAPSHOT {
            continue;
        }
        let meta = fs::symlink_metadata(entry.path())?;
        let content = if reparse(&meta) {
            #[cfg(windows)]
            let directory = {
                use std::os::windows::fs::MetadataExt;
                meta.file_attributes() & 0x10 != 0
            };
            #[cfg(not(windows))]
            let directory = false;
            Content::Link {
                target: fs::read_link(entry.path())
                    .context("Read data symlink/reparse point without following it")?,
                directory,
            }
        } else if meta.is_dir() {
            Content::Directory
        } else if meta.is_file() {
            Content::File {
                size: meta.len(),
                sha256: String::new(),
            }
        } else {
            bail!("Unsupported data file: {}", entry.path().display());
        };
        entries.push(Entry { path, content });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}
fn scan(root: &Path, job: &Job) -> Result<Vec<Entry>> {
    let entries = inventory(root, job)?;
    local_work::map(&entries, |entry| {
        job.check()?;
        let mut entry = entry.clone();
        if let Content::File { sha256, .. } = &mut entry.content {
            *sha256 = hash(&join(root, &entry.path)?)?;
        }
        Ok(entry)
    })
}
fn matches_entry(root: &Path, entry: &Entry) -> Result<bool> {
    let path = join(root, &entry.path)?;
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match &entry.content {
        Content::Directory => Ok(meta.is_dir() && !reparse(&meta)),
        Content::File { size, sha256 } => {
            Ok(meta.is_file() && !reparse(&meta) && meta.len() == *size && hash(&path)? == *sha256)
        }
        Content::Link { target, .. } => Ok(reparse(&meta) && fs::read_link(path)? == *target),
    }
}
fn rebase(path: &Path, source: &Path, target: &Path) -> PathBuf {
    path.strip_prefix(source)
        .map(|relative| target.join(relative))
        .unwrap_or_else(|_| path.into())
}
fn transform(source: &Layout, stage: &Path, target: &Path) -> Result<()> {
    storage::write_credential_scope(stage, source.credential_root())?;
    let registry = stage.join("state/servers.json");
    if let Some(mut value) = storage::read_optional::<crate::server_instances::Registry>(&registry)?
    {
        for server in &mut value.servers {
            if let crate::server_instances::Target::Sftp { connection } = &mut server.target {
                connection.key_path = rebase(Path::new(&connection.key_path), &source.root, target)
                    .to_string_lossy()
                    .into_owned();
            }
        }
        storage::write_json(&registry, &value)?;
    }
    let legacy = stage.join("state/sftp-connection.json");
    if let Some(mut value) =
        storage::read_optional::<crate::deploy::connection::ConnectionSettings>(&legacy)?
    {
        value.key_path = rebase(Path::new(&value.key_path), &source.root, target)
            .to_string_lossy()
            .into_owned();
        storage::write_json(&legacy, &value)?;
    }
    // Only launcher-generated server startup scripts are path-bound; worlds,
    // properties, EULA, user configs and unrelated file contents remain byte-exact.
    for entry in walkdir::WalkDir::new(stage).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file()
            || !matches!(entry.file_name().to_str(), Some("run.sh" | "run.bat"))
        {
            continue;
        }
        let path = relative(stage, entry.path())?;
        if !path.starts_with("server/") && !path.starts_with("cache/server/") {
            continue;
        }
        let bytes = fs::read(entry.path())?;
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if !text.starts_with("#!/bin/sh\nset -eu\ncd --")
            && !text.starts_with("@echo off\r\nchcp 65001 >nul")
        {
            continue;
        }
        rewrite_script(entry.path(), &source.root, target, None)?;
    }
    Ok(())
}
fn rewrite_script(path: &Path, source: &Path, target: &Path, backup: Option<&Path>) -> Result<()> {
    let bytes = fs::read(path)?;
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(());
    };
    if !text.starts_with("#!/bin/sh\nset -eu\ncd --")
        && !text.starts_with("@echo off\r\nchcp 65001 >nul")
    {
        return Ok(());
    }
    let old = source.to_string_lossy();
    let new = target.to_string_lossy();
    let (old, new) = if text.starts_with("@echo off\r\n") {
        (old.replace('%', "%%"), new.replace('%', "%%"))
    } else {
        (old.replace('\'', "'\\''"), new.replace('\'', "'\\''"))
    };
    // Replace once: the new folder may contain the old folder's name as a
    // string prefix, without being a descendant of that folder.
    let updated = text.replace(&old, &new);
    if updated == text {
        return Ok(());
    }
    if let Some(backup) = backup {
        if !backup.try_exists()? {
            storage::atomic_copy(path, backup)?;
        }
    }
    let permissions = fs::metadata(path)?.permissions();
    storage::atomic_write(path, updated.as_bytes())?;
    fs::set_permissions(path, permissions)?;
    Ok(())
}
fn refresh_local_server_scripts(p: &Pending) -> Result<()> {
    let layout = Layout::new(p.target.clone())?;
    for server in crate::server_instances::Registry::load(&layout)?.servers {
        let crate::server_instances::Target::Local { directory } = server.target else {
            continue;
        };
        let directory = PathBuf::from(directory);
        if overlaps(&directory, &p.target) {
            bail!("A saved server overlaps the new data folder; old files have been retained");
        }
        for name in ["run.sh", "run.bat"] {
            let script = directory.join(name);
            let meta = match fs::symlink_metadata(&script) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            if !meta.is_file() || reparse(&meta) {
                continue;
            }
            let identity =
                hex::encode(sha2::Sha256::digest(directory.to_string_lossy().as_bytes()));
            let backup = p
                .target
                .join("backups/data-location")
                .join(&p.id)
                .join(identity)
                .join(name);
            rewrite_script(&script, &p.source, &p.target, Some(&backup)).with_context(|| {
                format!(
                    "Update managed startup script {}; old runtime files were retained",
                    script.display()
                )
            })?;
        }
    }
    Ok(())
}
fn copy_staged(source: &Path, destination: &Path) -> Result<()> {
    match storage::atomic_copy(source, destination) {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied) =>
        {
            let meta = fs::symlink_metadata(destination)?;
            if !meta.is_file() || reparse(&meta) || !meta.permissions().readonly() {
                return Err(error);
            }
            let mut permissions = meta.permissions();
            permissions.set_readonly(false);
            fs::set_permissions(destination, permissions)?;
            storage::atomic_copy(source, destination)
        }
        Err(error) => Err(error),
    }
}
fn remove_leaf(path: &Path, directory: bool) -> Result<()> {
    #[cfg(windows)]
    let directory = directory
        || fs::symlink_metadata(path).is_ok_and(|meta| {
            use std::os::windows::fs::MetadataExt;
            reparse(&meta) && meta.file_attributes() & 0x10 != 0
        });
    let result = if directory {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        #[cfg(windows)]
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            let meta = fs::symlink_metadata(path)?;
            if reparse(&meta) {
                if meta.is_dir() {
                    fs::remove_dir(path)?;
                } else {
                    fs::remove_file(path)?;
                }
            } else {
                let mut permissions = meta.permissions();
                permissions.set_readonly(false);
                fs::set_permissions(path, permissions)?;
                if directory {
                    fs::remove_dir(path)?;
                } else {
                    fs::remove_file(path)?;
                }
            }
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}
fn symlink(target: &Path, path: &Path, directory: bool) -> Result<()> {
    #[cfg(unix)]
    {
        let _ = directory;
        std::os::unix::fs::symlink(target, path).context("Preserve data symlink")
    }
    #[cfg(windows)]
    {
        if directory {std::os::windows::fs::symlink_dir(target,path)} else {std::os::windows::fs::symlink_file(target,path)}.context("Preserve data link; Windows may require Developer Mode or permission to create symbolic links")
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, path, directory);
        bail!("Data symlinks are unsupported on this platform")
    }
}
fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::File::open(path.parent().context("Directory parent missing")?)?.sync_all()?;
    let _ = path;
    Ok(())
}

#[cfg(test)]
#[path = "data_location_tests.rs"]
mod tests;
