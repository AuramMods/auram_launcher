use crate::{
    auth::Credentials,
    download::{Checksum, Download, Downloader},
    jobs::Job,
    storage::{self, Layout, Settings},
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::io::{AsyncBufReadExt, BufReader};

mod compat;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacArm,
    WindowsX64,
}
impl Platform {
    pub fn current() -> Result<Self> {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Ok(Self::MacArm)
        } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Ok(Self::WindowsX64)
        } else {
            bail!("Auram supports Apple Silicon macOS and Windows x64")
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::MacArm => "macos-arm64",
            Self::WindowsX64 => "windows-x64",
        }
    }
    pub fn mojang(self) -> &'static str {
        match self {
            Self::MacArm => "osx",
            Self::WindowsX64 => "windows",
        }
    }
    fn arch(self) -> &'static str {
        match self {
            Self::MacArm => "aarch64",
            Self::WindowsX64 => "x86_64",
        }
    }
    pub fn separator(self) -> &'static str {
        match self {
            Self::MacArm => ":",
            Self::WindowsX64 => ";",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Versions {
    pub minecraft: String,
    pub forge: String,
}
impl Versions {
    pub fn read(client: &Path) -> Result<Self> {
        let value: Value = storage::read_json(&client.join("mmc-pack.json"))?;
        let entries = value["components"]
            .as_array()
            .context("Pack components missing")?;
        let find = |id: &str| -> Result<String> {
            Ok(entries
                .iter()
                .find(|v| v["uid"] == id)
                .and_then(|v| v["version"].as_str())
                .with_context(|| format!("Pack component {id} missing"))?
                .into())
        };
        let result = Self {
            minecraft: find("net.minecraft")?,
            forge: find("net.minecraftforge")?,
        };
        if result.minecraft != "1.20.1" {
            bail!(
                "This launcher supports Minecraft 1.20.1; pack requests {}",
                result.minecraft
            );
        }
        if !result.forge.chars().all(|c| c.is_ascii_digit() || c == '.') {
            bail!("Invalid Forge version");
        }
        Ok(result)
    }
    pub fn forge_id(&self) -> String {
        format!("{}-forge-{}", self.minecraft, self.forge)
    }
}
#[derive(Deserialize)]
struct JavaPackage {
    url: String,
    sha256: String,
    size: u64,
}
#[derive(Clone)]
pub struct Runtime {
    pub layout: Layout,
    pub net: Downloader,
    pub platform: Platform,
}
#[derive(Clone)]
pub struct Ready {
    pub java: PathBuf,
    pub vanilla: Value,
    pub forge: Value,
    pub versions: Versions,
}
impl Runtime {
    pub async fn java(&self, job: &Job) -> Result<PathBuf> {
        job.phase("java", "Java 17", false);
        job.progress("java", "Verifying the Java runtime", 0, 0);
        let all: HashMap<String, JavaPackage> =
            serde_json::from_str(include_str!("../java-runtimes.json"))?;
        let package = &all[self.platform.key()];
        let java_root = self.layout.root.join("cache/java").join(&package.sha256);
        let snapshot = java_root.join(".verified.json");
        if snapshot.exists() && verify_snapshot(&java_root, &snapshot)? {
            job.complete_phase();
            return find_java(&java_root);
        }
        job.progress("java", "Preparing Java 17", 0, 0);
        let archive = self
            .layout
            .root
            .join("cache")
            .join(format!("java-{}.zip", package.sha256));
        self.net
            .get(
                &Download {
                    label: Some("Java 17 runtime archive".into()),
                    url: package.url.clone(),
                    target: archive.clone(),
                    size: Some(package.size),
                    checksum: Checksum::Sha256(package.sha256.clone()),
                },
                job,
            )
            .await?;
        let staging = self
            .layout
            .root
            .join("cache/java")
            .join(format!("{}.staging", package.sha256));
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(&staging)?;
        job.progress("java", "Extracting and verifying Java 17", 0, 0);
        extract_zip(&archive, &staging).context("Extract managed Java archive")?;
        let java = find_java(&staging)?;
        run_logged(
            &java,
            &["-version".into()],
            &staging,
            &self.layout.root.join("logs/java.log"),
            job,
            &[],
        )
        .await?;
        snapshot_tree(&staging, &staging.join(".verified.json"), &[])?;
        if java_root.exists() {
            fs::remove_dir_all(&java_root)?;
        }
        fs::rename(&staging, &java_root)?;
        job.complete_phase();
        find_java(&java_root)
    }
    pub async fn installer(&self, versions: &Versions, job: &Job) -> Result<PathBuf> {
        let coordinate = format!("{}-{}", versions.minecraft, versions.forge);
        let url=format!("https://maven.minecraftforge.net/net/minecraftforge/forge/{coordinate}/forge-{coordinate}-installer.jar");
        let checksum_file = self
            .layout
            .state(&format!("forge-{coordinate}-checksum.json"));
        let checksum = if let Some(value) = storage::read_optional::<String>(&checksum_file)? {
            value
        } else {
            let value = self
                .net
                .client
                .get(format!("{url}.sha1"))
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;
            let value = value.split_whitespace().next().unwrap_or("").to_string();
            if value.len() != 40 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
                bail!("Invalid Forge installer checksum");
            }
            storage::write_json(&checksum_file, &value)?;
            value
        };
        let target = self
            .layout
            .root
            .join("cache/forge")
            .join(format!("{coordinate}-installer.jar"));
        self.net
            .get(
                &Download {
                    label: Some("Forge installer".into()),
                    url,
                    target: target.clone(),
                    size: None,
                    checksum: Checksum::Sha1(checksum),
                },
                job,
            )
            .await?;
        Ok(target)
    }
    pub async fn ensure(&self, versions: Versions, job: &Job) -> Result<Ready> {
        let java = self.java(job).await?;
        job.phase("minecraft", "Minecraft", false);
        job.progress("runtime", "Preparing Minecraft and Forge", 0, 0);
        let runtime = self.layout.runtime();
        fs::create_dir_all(&runtime)?;
        let meta_file = self.layout.state("minecraft-1.20.1.json");
        let vanilla: Value = if let Some(value) = storage::read_optional(&meta_file)? {
            value
        } else {
            let manifest: Value = self
                .net
                .json("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")
                .await?;
            let entry = manifest["versions"]
                .as_array()
                .context("Version manifest missing")?
                .iter()
                .find(|v| v["id"] == versions.minecraft)
                .context("Minecraft 1.20.1 not found")?;
            let target = self
                .layout
                .root
                .join("cache/metadata/minecraft-1.20.1.json");
            self.net.get(&artifact(entry, &target)?, job).await?;
            let value: Value = storage::read_json(&target)?;
            storage::write_json(&meta_file, &value)?;
            value
        };
        let base_dir = runtime.join("versions").join(&versions.minecraft);
        fs::create_dir_all(&base_dir)?;
        storage::write_json(
            &base_dir.join(format!("{}.json", versions.minecraft)),
            &vanilla,
        )?;
        let vanilla = compat::client_metadata(vanilla, self.platform)?;
        self.net
            .get(
                &artifact(
                    &vanilla["downloads"]["client"],
                    &base_dir.join(format!("{}.jar", versions.minecraft)),
                )?,
                job,
            )
            .await?;
        let libraries = library_downloads(&vanilla, &runtime.join("libraries"), self.platform)?;
        self.net
            .many_named(libraries, job, "Minecraft libraries")
            .await?;
        job.phase("forge", "Forge", false);
        job.progress("forge", "Verifying Forge runtime", 0, 0);
        let forge_dir = runtime.join("versions").join(versions.forge_id());
        let forge_json = forge_dir.join(format!("{}.json", versions.forge_id()));
        let forge_snapshot = self.layout.state(&format!(
            "forge-runtime-{}-{}.json",
            versions.forge,
            self.platform.key()
        ));
        let forge_valid = forge_json.exists()
            && forge_snapshot.exists()
            && verify_snapshot(&runtime, &forge_snapshot)?;
        if !forge_valid {
            if forge_snapshot.exists() {
                remove_invalid_snapshot_files(&runtime, &forge_snapshot)?;
            }
            let installer = self.installer(&versions, job).await?;
            storage::write_json(
                &runtime.join("launcher_profiles.json"),
                &json!({"profiles":{},"version":3}),
            )?;
            job.progress("forge", "Installing Forge client runtime", 0, 0);
            run_logged(
                &java,
                &[
                    "-jar".into(),
                    installer.to_string_lossy().into(),
                    "--installClient".into(),
                    runtime.to_string_lossy().into(),
                ],
                &runtime,
                &self.layout.root.join("logs/forge-client.log"),
                job,
                &[],
            )
            .await?;
            if !forge_json.is_file() {
                bail!("Forge installer did not generate version metadata; see forge-client.log");
            }
            snapshot_tree(&runtime, &forge_snapshot, &["libraries/", "versions/"])?;
        }
        let forge = compat::client_metadata(storage::read_json(&forge_json)?, self.platform)?;
        self.net
            .many_named(
                library_downloads(&forge, &runtime.join("libraries"), self.platform)?,
                job,
                "Forge libraries",
            )
            .await?;
        job.phase("assets", "Assets", true);
        job.progress("assets", "Checking Minecraft assets", 0, 0);
        let index = &vanilla["assetIndex"];
        let index_id = index["id"].as_str().context("Asset index ID missing")?;
        let index_file =
            storage::safe_join(&runtime.join("assets/indexes"), &format!("{index_id}.json"))?;
        self.net.get(&artifact(index, &index_file)?, job).await?;
        let index: Value = storage::read_json(&index_file)?;
        let mut assets = Vec::new();
        for (name, object) in index["objects"]
            .as_object()
            .context("Asset objects missing")?
            .iter()
        {
            let hash = object["hash"].as_str().context("Asset hash missing")?;
            if hash.len() != 40 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
                bail!("Invalid asset hash");
            }
            assets.push(Download {
                label: Some(name.clone()),
                url: format!(
                    "https://resources.download.minecraft.net/{}/{hash}",
                    &hash[..2]
                ),
                target: runtime.join("assets/objects").join(&hash[..2]).join(hash),
                size: object["size"].as_u64(),
                checksum: Checksum::Sha1(hash.into()),
            });
        }
        self.net.many_named(assets, job, "Mojang assets").await?;
        job.complete_phase();
        Ok(Ready {
            java,
            vanilla,
            forge,
            versions,
        })
    }
    pub fn arguments(
        &self,
        ready: &Ready,
        session: &Credentials,
        settings: &Settings,
    ) -> Result<Vec<String>> {
        let profile = session
            .profile
            .as_ref()
            .context("Minecraft account missing")?;
        let runtime = self.layout.runtime();
        let libraries = runtime.join("libraries");
        let natives = runtime.join("natives").join(self.platform.key());
        fs::create_dir_all(&natives)?;
        let mut paths = BTreeMap::new();
        for meta in [&ready.vanilla, &ready.forge] {
            for library in meta["libraries"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|lib| allowed(lib, self.platform) && native_compatible(lib, self.platform))
            {
                let coordinate = library["name"]
                    .as_str()
                    .context("Library coordinate missing")?;
                let path = library["downloads"]["artifact"]["path"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or(maven_path(coordinate)?);
                let jar = storage::safe_join(&libraries, &path)?;
                let identity = coordinate.split(':').take(2).collect::<Vec<_>>().join(":")
                    + if coordinate.split(':').count() > 3 {
                        coordinate.rsplit(':').next().unwrap_or("")
                    } else {
                        ""
                    };
                paths.insert(identity, jar);
                if let Some(native) = native_artifact(library, self.platform) {
                    if let Some(path) = native["path"].as_str() {
                        extract_zip(&storage::safe_join(&libraries, path)?, &natives)?;
                    }
                }
            }
        }
        paths.insert(
            "minecraft".into(),
            runtime.join("versions/1.20.1/1.20.1.jar"),
        );
        let classpath = paths
            .values()
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join(self.platform.separator());
        let mut values = HashMap::<String, String>::new();
        for (key, value) in [
            ("auth_player_name", profile.name.clone()),
            ("auth_uuid", profile.uuid.clone()),
            ("auth_access_token", session.minecraft_token.clone()),
            (
                "auth_session",
                format!("token:{}:{}", session.minecraft_token, profile.uuid),
            ),
            ("clientid", settings.microsoft_client_id.clone()),
            ("auth_xuid", profile.xuid.clone()),
            ("xuid", profile.xuid.clone()),
            ("user_type", "msa".into()),
            ("user_properties", "{}".into()),
            ("version_name", ready.versions.forge_id()),
            ("version_type", "release".into()),
            (
                "game_directory",
                self.layout.game().to_string_lossy().into(),
            ),
            (
                "assets_root",
                runtime.join("assets").to_string_lossy().into(),
            ),
            (
                "game_assets",
                runtime.join("assets").to_string_lossy().into(),
            ),
            (
                "assets_index_name",
                ready.vanilla["assetIndex"]["id"]
                    .as_str()
                    .unwrap_or("5")
                    .into(),
            ),
            ("natives_directory", natives.to_string_lossy().into()),
            ("launcher_name", "Auram".into()),
            ("launcher_version", env!("AURAM_BUILD_NUMBER").into()),
            ("classpath", classpath),
            ("classpath_separator", self.platform.separator().into()),
            ("library_directory", libraries.to_string_lossy().into()),
        ] {
            values.insert(key.into(), value);
        }
        let replace = |arg: String| -> Result<String> {
            let mut arg = arg;
            for (key, value) in &values {
                arg = arg.replace(&format!("${{{key}}}"), value);
            }
            if arg.contains("${") {
                bail!("Unresolved launch argument: {arg}");
            }
            Ok(arg)
        };
        let mut jvm = Vec::new();
        let mut game = Vec::new();
        for meta in [&ready.vanilla, &ready.forge] {
            jvm.extend(arguments(meta, "jvm", self.platform)?);
            game.extend(arguments(meta, "game", self.platform)?);
        }
        let mut output = jvm.into_iter().map(replace).collect::<Result<Vec<_>>>()?;
        let vanilla_jar = format!("{}.jar", ready.versions.minecraft);
        for argument in &mut output {
            if let Some(ignore) = argument.strip_prefix("-DignoreList=") {
                if !ignore.split(',').any(|entry| entry == vanilla_jar) {
                    *argument = format!("{argument},{vanilla_jar}");
                }
            }
        }
        if !output
            .iter()
            .any(|a| a == "-cp" || a == "-classpath" || a == "--class-path")
        {
            output.extend(["-cp".into(), values["classpath"].clone()]);
        }
        if self.platform == Platform::MacArm && !output.iter().any(|a| a == "-XstartOnFirstThread")
        {
            output.push("-XstartOnFirstThread".into());
        }
        output.extend(crate::launch_options::jvm_arguments(settings.memory_mib)?);
        output.push(
            ready.forge["mainClass"]
                .as_str()
                .or_else(|| ready.vanilla["mainClass"].as_str())
                .context("Launch main class missing")?
                .into(),
        );
        output.extend(game.into_iter().map(replace).collect::<Result<Vec<_>>>()?);
        Ok(output)
    }
}
fn artifact(value: &Value, target: &Path) -> Result<Download> {
    Ok(Download {
        label: None,
        url: value["url"]
            .as_str()
            .context("Artifact URL missing")?
            .into(),
        target: target.into(),
        size: value["size"].as_u64(),
        checksum: Checksum::Sha1(
            value["sha1"]
                .as_str()
                .context("Artifact SHA-1 missing")?
                .into(),
        ),
    })
}
pub fn maven_path(name: &str) -> Result<String> {
    let (coordinate, extension) = name.split_once('@').unwrap_or((name, "jar"));
    let parts: Vec<_> = coordinate.split(':').collect();
    if parts.len() < 3 || parts.len() > 4 {
        bail!("Invalid Maven coordinate {name}");
    }
    Ok(format!(
        "{}/{}/{}/{}-{}{}.{}",
        parts[0].replace('.', "/"),
        parts[1],
        parts[2],
        parts[1],
        parts[2],
        parts.get(3).map(|v| format!("-{v}")).unwrap_or_default(),
        extension
    ))
}
pub fn allowed(value: &Value, platform: Platform) -> bool {
    let Some(rules) = value["rules"].as_array() else {
        return true;
    };
    let mut allowed = false;
    for rule in rules {
        if let Some(name) = rule["os"]["name"].as_str() {
            if name != platform.mojang() {
                continue;
            }
        }
        if let Some(arch) = rule["os"]["arch"].as_str() {
            if !regex::Regex::new(&format!("^(?:{arch})$"))
                .is_ok_and(|r| r.is_match(platform.arch()))
            {
                continue;
            }
        }
        if let Some(version) = rule["os"]["version"].as_str() {
            if !regex::Regex::new(version)
                .is_ok_and(|r| r.is_match(&sysinfo::System::os_version().unwrap_or_default()))
            {
                continue;
            }
        }
        if rule["features"]
            .as_object()
            .is_some_and(|features| features.values().any(|v| v.as_bool().unwrap_or(false)))
        {
            continue;
        }
        allowed = rule["action"] == "allow";
    }
    allowed
}
fn native_compatible(library: &Value, platform: Platform) -> bool {
    let name = library["name"].as_str().unwrap_or("");
    let classifier = name.split(':').nth(3).unwrap_or("");
    if classifier.starts_with("natives-macos") {
        return platform == Platform::MacArm && classifier == "natives-macos-arm64";
    }
    if classifier.starts_with("natives-windows") {
        return platform == Platform::WindowsX64 && classifier == "natives-windows";
    }
    true
}
fn native_artifact(library: &Value, platform: Platform) -> Option<Value> {
    let classifier = library["natives"][platform.mojang()].as_str()?.replace(
        "${arch}",
        if platform == Platform::MacArm {
            "arm64"
        } else {
            "64"
        },
    );
    library["downloads"]["classifiers"].get(classifier).cloned()
}
fn library_downloads(meta: &Value, root: &Path, platform: Platform) -> Result<Vec<Download>> {
    let mut result = Vec::new();
    for library in meta["libraries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| allowed(v, platform) && native_compatible(v, platform))
    {
        if let Some(path) = library["downloads"]["artifact"]["path"].as_str() {
            let artifact_value = &library["downloads"]["artifact"];
            if artifact_value["url"]
                .as_str()
                .is_some_and(|url| !url.is_empty())
            {
                result.push(artifact(artifact_value, &storage::safe_join(root, path)?)?);
            }
        }
        if let Some(native) = native_artifact(library, platform) {
            if let Some(path) = native["path"].as_str() {
                result.push(artifact(&native, &storage::safe_join(root, path)?)?);
            }
        }
    }
    Ok(result)
}
fn arguments(meta: &Value, side: &str, platform: Platform) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for value in meta["arguments"][side].as_array().into_iter().flatten() {
        if let Some(value) = value.as_str() {
            result.push(value.into());
        } else if allowed(value, platform) {
            let value = &value["value"];
            if let Some(value) = value.as_str() {
                result.push(value.into());
            } else if let Some(values) = value.as_array() {
                for value in values {
                    result.push(value.as_str().context("Invalid launch argument")?.into());
                }
            }
        }
    }
    Ok(result)
}
pub fn extract_zip(archive: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    let mut archive = zip::ZipArchive::new(fs::File::open(archive)?)?;
    let mut links = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name().trim_end_matches('/').to_owned();
        if name.is_empty() {
            continue;
        }
        let target = storage::safe_join(destination, &name)?;
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            let mut link = String::new();
            entry.read_to_string(&mut link)?;
            let source = archive_link(&name, &link)?;
            links.push((target, source));
            continue;
        }
        fs::create_dir_all(target.parent().context("Archive path has no parent")?)?;
        let mut output = fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut output)?;
        output.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &target,
                fs::Permissions::from_mode(entry.unix_mode().unwrap_or(0o644) & 0o777),
            )?;
        }
    }
    // Materialize internal archive links as independent files, never filesystem symlinks.
    while !links.is_empty() {
        let before = links.len();
        let mut pending = Vec::new();
        for (target, relative) in links {
            let source = storage::safe_join(destination, &relative)?;
            if source.is_file() {
                storage::atomic_copy(&source, &target)?;
            } else {
                pending.push((target, relative));
            }
        }
        if pending.len() == before {
            bail!("Archive contains unresolved links");
        }
        links = pending;
    }

    Ok(())
}
fn archive_link(name: &str, link: &str) -> Result<String> {
    if link.starts_with('/') || link.contains('\\') || link.contains(':') {
        bail!("Unsafe archive link");
    }
    let mut parts = name.split('/').map(str::to_owned).collect::<Vec<_>>();
    parts.pop();
    for part in link.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                if parts.pop().is_none() {
                    bail!("Archive link escapes destination");
                }
            }
            part => parts.push(part.into()),
        }
    }
    Ok(parts.join("/"))
}

fn find_java(root: &Path) -> Result<PathBuf> {
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file()
            && (entry.file_name() == "java" || entry.file_name() == "java.exe")
            && entry.path().parent().is_some_and(|p| p.ends_with("bin"))
        {
            return Ok(entry.into_path());
        }
    }
    bail!("Java executable not found in {}", root.display())
}
pub fn snapshot_tree(root: &Path, snapshot: &Path, prefixes: &[&str]) -> Result<()> {
    let mut hashes = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() || entry.path() == snapshot {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        if !prefixes.is_empty() && !prefixes.iter().any(|prefix| relative.starts_with(prefix)) {
            continue;
        }
        hashes.insert(relative, file_sha256(entry.path())?);
    }
    storage::write_json(snapshot, &hashes)
}
fn file_sha256(path: &Path) -> Result<String> {
    let mut hash = sha2::Sha256::new();
    let mut input = fs::File::open(path)?;
    let mut buffer = [0u8; 65536];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}
pub fn verify_snapshot(root: &Path, snapshot: &Path) -> Result<bool> {
    let hashes: BTreeMap<String, String> = storage::read_json(snapshot)?;
    if hashes.is_empty() {
        return Ok(false);
    }
    for (relative, hash) in hashes {
        let path = storage::safe_join(root, &relative)?;
        if !path.is_file() || !Checksum::Sha256(hash).verify(&path)? {
            return Ok(false);
        }
    }
    Ok(true)
}
fn remove_invalid_snapshot_files(root: &Path, snapshot: &Path) -> Result<()> {
    let hashes: BTreeMap<String, String> = storage::read_json(snapshot)?;
    for (relative, hash) in hashes {
        let path = storage::safe_join(root, &relative)?;
        if path.is_file() && !Checksum::Sha256(hash).verify(&path)? {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}
pub fn argfile(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|arg| {
            format!(
                "\"{}\"",
                arg.replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}
pub async fn run_logged(
    java: &Path,
    args: &[String],
    cwd: &Path,
    log: &Path,
    job: &Job,
    secrets: &[String],
) -> Result<()> {
    let label = match log.file_stem().and_then(|s| s.to_str()) {
        Some("forge-client") => "Forge client installer",
        Some("forge-server") => "Forge server installer",
        _ => "Java runtime check",
    };
    let mut task = job.task(&format!("process:{}", log.display()), label, None);
    task.state(crate::jobs::TaskState::Running, 0);
    let result = async {
        fs::create_dir_all(log.parent().context("Log has no parent")?)?;
        let file = std::sync::Arc::new(std::sync::Mutex::new(fs::File::create(log)?));
        let (mut command, input) = crate::java_bridge::command(java, args)?;
        command
            .current_dir(cwd)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command.spawn().context("Unable to start Java")?;
        crate::java_bridge::feed(&mut child, input).await?;
        let stdout = child.stdout.take().context("Java output missing")?;
        let stderr = child.stderr.take().context("Java error output missing")?;
        let output = async {
            let (one, two) = tokio::join!(
                copy_log(stdout, file.clone(), secrets),
                copy_log(stderr, file.clone(), secrets)
            );
            one?;
            two?;
            Ok::<_, anyhow::Error>(())
        };
        let status = async {
            let (status, output) = tokio::join!(child.wait(), output);
            output?;
            Ok::<_, anyhow::Error>(status?)
        };
        let status = tokio::select! {_=job.cancel.cancelled()=>bail!("Operation cancelled"),status=status=>status?};
        if !status.success() {
            bail!("Java exited with {status}. See {}", log.display());
        }
        Ok(())
    }
    .await;
    match &result {
        Ok(()) => task.complete(false),
        Err(error) => task.fail(&format!("{error:#}")),
    }
    result
}
pub(crate) async fn copy_log(
    reader: impl tokio::io::AsyncRead + Unpin,
    file: std::sync::Arc<std::sync::Mutex<fs::File>>,
    secrets: &[String],
) -> Result<()> {
    let mut lines = BufReader::new(reader).lines();
    while let Some(mut line) = lines.next_line().await? {
        for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
            line = line.replace(secret, "[REDACTED]");
        }
        writeln!(file.lock().unwrap_or_else(|e| e.into_inner()), "{line}")?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_coordinates_match_java_architecture() {
        let lib = json!({"name":"org.lwjgl:lwjgl:3.3.1:natives-macos"});
        assert!(!native_compatible(&lib, Platform::MacArm));
        let lib = json!({"name":"org.lwjgl:lwjgl:3.3.1:natives-macos-arm64"});
        assert!(native_compatible(&lib, Platform::MacArm));
        assert!(!native_compatible(&lib, Platform::WindowsX64));
        assert_eq!(
            maven_path("group:artifact:1:natives@zip").unwrap(),
            "group/artifact/1/artifact-1-natives.zip"
        );
    }
    #[test]
    fn argfile_preserves_spaces_unicode_and_backslashes() {
        assert_eq!(
            argfile(&["C:\\Users\\Name S\\世界".into(), "a\"b".into()]),
            "\"C:\\\\Users\\\\Name S\\\\世界\"\n\"a\\\"b\"\n"
        );
    }
    #[test]
    fn archive_links_cannot_escape_the_extraction_root() {
        assert!(archive_link("folder/link", "../../outside").is_err());
        assert!(archive_link("folder/link", "/outside").is_err());
        assert_eq!(
            archive_link("jdk/legal/module/LICENSE", "../../license/LICENSE").unwrap(),
            "jdk/license/LICENSE"
        );
    }
    #[test]
    fn forge_arguments_preserve_vanilla_exclusion_and_exact_architecture_rules() {
        assert!(!allowed(
            &json!({"rules":[{"action":"allow","os":{"arch":"x86"}}]}),
            Platform::WindowsX64
        ));
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout::new(temp.path().join("World with spaces 世界")).unwrap();
        let runtime = Runtime {
            net: Downloader::new(&layout).unwrap(),
            layout,
            platform: Platform::MacArm,
        };
        let ready = Ready {
            java: PathBuf::new(),
            versions: Versions {
                minecraft: "1.20.1".into(),
                forge: "47.4.10".into(),
            },
            vanilla: json!({"libraries":[],"assetIndex":{"id":"5"},"arguments":{"jvm":["-cp","${classpath}"],"game":["--username","${auth_player_name}","--xuid","${auth_xuid}"]}}),
            forge: json!({"mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher","arguments":{"jvm":["-DignoreList=forge-,${version_name}.jar"]}}),
        };
        let session = Credentials {
            minecraft_token: "fixture-only".into(),
            profile: Some(crate::auth::Profile {
                name: "Player".into(),
                uuid: "uuid".into(),
                xuid: "xbox-user-id".into(),
            }),
            ..Default::default()
        };
        let args = runtime
            .arguments(&ready, &session, &Settings::default())
            .unwrap();
        assert!(args.contains(&"-DignoreList=forge-,1.20.1-forge-47.4.10.jar,1.20.1.jar".into()));
        assert_eq!(
            args[args.iter().position(|arg| arg == "--xuid").unwrap() + 1],
            "xbox-user-id"
        );
        assert!(args
            .iter()
            .any(|arg| arg.contains("World with spaces 世界")));
    }
}
