use crate::{
    jobs::Job,
    pack::{Manifest, PackFile, Packs},
    runtime::{self, Runtime, Versions},
    storage,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha1::Digest;
use std::{collections::BTreeMap, fs, io::Read, path::Path};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerVersion {
    #[serde(default)]
    pub schema: u32,
    #[serde(deserialize_with = "version_string")]
    pub version: String,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub minecraft: String,
    #[serde(default)]
    pub forge: String,
}
fn version_string<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    match value {
        serde_json::Value::String(v) => Ok(v),
        serde_json::Value::Number(v) if v.is_u64() => Ok(v.to_string()),
        _ => Err(serde::de::Error::custom("Invalid Auram version")),
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerConfig {
    pub client_mods: Vec<String>,
    pub whitelist: Vec<String>,
    pub inclusions: Vec<Inclusion>,
    pub include_server_properties: bool,
    pub server_properties_path: String,
    pub include_server_icon: bool,
    pub server_icon_path: String,
}
#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Inclusion {
    pub source: String,
    pub destination: String,
    pub inclusion_filter: String,
    pub exclusion_filter: String,
}
impl ServerConfig {
    fn include_mod(&self, name: &str) -> bool {
        self.whitelist.iter().any(|p| name.starts_with(p))
            || !self.client_mods.iter().any(|p| name.starts_with(p))
    }
    fn manifest(&self, full: &Manifest) -> Result<Manifest> {
        let mut desired = Manifest {
            schema: 1,
            policy: 0,
            tag: full.tag.clone(),
            files: BTreeMap::new(),
        };
        let defaults = ["mods", "config", "kubejs", "scripts", "defaultconfigs"];
        let mut inclusions: Vec<Inclusion> = Vec::new();
        if self.inclusions.is_empty() {
            inclusions.extend(defaults.into_iter().map(|source| Inclusion {
                source: source.into(),
                ..Default::default()
            }));
        }
        for inclusion in self.inclusions.iter().chain(inclusions.iter()) {
            let source = relative_resource(&inclusion.source, "minecraft/")?;
            let destination = inclusion.destination.trim_matches('/');
            if !destination.is_empty() {
                storage::safe_join(Path::new(""), destination)?;
            }
            let include = if inclusion.inclusion_filter.is_empty() {
                None
            } else {
                Some(regex::Regex::new(&inclusion.inclusion_filter)?)
            };
            let exclude = if inclusion.exclusion_filter.is_empty() {
                None
            } else {
                Some(regex::Regex::new(&inclusion.exclusion_filter)?)
            };
            for (path, file) in &full.files {
                let Some(relative) = path.strip_prefix(&format!("{source}/")) else {
                    continue;
                };
                if path.starts_with("minecraft/mods/")
                    && !self.include_mod(path.rsplit('/').next().unwrap_or(""))
                {
                    continue;
                }
                if include.as_ref().is_some_and(|r| !r.is_match(relative))
                    || exclude.as_ref().is_some_and(|r| r.is_match(relative))
                {
                    continue;
                }
                let folder = source.strip_prefix("minecraft/").unwrap_or(&source);
                let target = if destination.is_empty() {
                    format!("{folder}/{relative}")
                } else {
                    format!("{destination}/{relative}")
                };
                storage::safe_join(Path::new(""), &target)?;
                let mut file = file.clone();
                file.source = Some(path.clone());
                desired.files.insert(target, file);
            }
        }
        for (enabled, source, fallback, target) in [
            (
                self.include_server_properties,
                self.server_properties_path.as_str(),
                "server.properties",
                "server.properties",
            ),
            (
                self.include_server_icon,
                self.server_icon_path.as_str(),
                "server-icon.png",
                "server-icon.png",
            ),
        ] {
            if !enabled {
                continue;
            }
            let source = relative_resource(
                if source.is_empty() { fallback } else { source },
                "server_files/",
            )?;
            let file = full
                .files
                .get(&source)
                .with_context(|| format!("Pack server resource missing: {source}"))?;
            let mut file = file.clone();
            file.source = Some(source);
            desired.files.insert(target.into(), file);
        }
        Ok(desired)
    }
}
fn relative_resource(source: &str, default_prefix: &str) -> Result<String> {
    let normalized = source.replace('\\', "/");
    if let Some((_, suffix)) = normalized.rsplit_once("/minecraft/") {
        storage::safe_join(Path::new(""), suffix)?;
        return Ok(format!("minecraft/{suffix}"));
    }
    if let Some((_, suffix)) = normalized.rsplit_once("/server_files/") {
        storage::safe_join(Path::new(""), suffix)?;
        return Ok(format!("server_files/{suffix}"));
    }
    let relative = normalized.strip_prefix("./").unwrap_or(&normalized);
    storage::safe_join(Path::new(""), relative)?;
    Ok(
        if relative.starts_with("minecraft/") || relative.starts_with("server_files/") {
            relative.into()
        } else {
            format!("{default_prefix}{relative}")
        },
    )
}
pub async fn build(packs: &Packs, runtime: &Runtime, job: &Job) -> Result<()> {
    job.phase("server-pack", "Server inputs", false);
    let installed = packs
        .installed()?
        .context("Install the client pack before building a server")?;
    let full = packs.manifest(installed.tag.clone(), job).await?;
    let config: ServerConfig = toml::from_str(&fs::read_to_string(
        packs.file(&full, "server_config.conf", job).await?,
    )?)
    .context("Invalid server_config.conf")?;
    let mut desired = config.manifest(&full)?;
    filter_sided_mods(packs, &full, &config, &mut desired, job).await?;
    resolve_dependencies(packs, &full, &mut desired, job).await?;
    let payload = desired.clone();
    let versions = Versions::read(&packs.layout.client())?;
    let java = runtime.java(job).await?;
    job.phase("server-forge", "Forge server", false);
    job.progress("server", "Verifying Forge server runtime", 0, 0);
    let installer = runtime.installer(&versions, job).await?;
    let cache = packs.layout.root.join("cache/server").join(format!(
        "{}-{}-{}",
        versions.minecraft,
        versions.forge,
        runtime.platform.key()
    ));
    let snapshot = cache.join(".verified.json");
    if !snapshot.exists() || !runtime::verify_snapshot(&cache, &snapshot)? {
        let staging = cache.with_extension("staging");
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(&staging)?;
        job.progress("server", "Installing Forge server runtime", 0, 0);
        runtime::run_logged(
            &java,
            &[
                "-jar".into(),
                installer.to_string_lossy().into(),
                "--installServer".into(),
                staging.to_string_lossy().into(),
            ],
            &staging,
            &packs.layout.root.join("logs/forge-server.log"),
            job,
            &[],
        )
        .await?;
        runtime::snapshot_tree(&staging, &staging.join(".verified.json"), &[])?;
        if cache.exists() {
            fs::remove_dir_all(&cache)?;
        }
        fs::create_dir_all(cache.parent().context("Server cache parent missing")?)?;
        fs::rename(staging, &cache)?;
    }
    write_start_scripts(&cache, &java, &versions)?;
    runtime::snapshot_tree(&cache, &snapshot, &[])?;
    for entry in walkdir::WalkDir::new(&cache).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&cache)?
            .to_string_lossy()
            .replace('\\', "/");
        if relative == ".verified.json"
            || relative.ends_with(".log")
            || relative.ends_with("installer.jar")
        {
            continue;
        }
        let file = local_file(entry.path())?;
        storage::atomic_copy(entry.path(), &packs.blob_path(&file))?;
        desired.files.insert(relative, file);
    }
    let version = ServerVersion {
        schema: 1,
        version: installed.tag.name.clone(),
        commit: installed.tag.sha.clone(),
        minecraft: versions.minecraft.clone(),
        forge: versions.forge.clone(),
    };
    let marker = packs.layout.state("built-server-auram.json");
    storage::write_json(&marker, &version)?;
    let file = local_file(&marker)?;
    storage::atomic_copy(&marker, &packs.blob_path(&file))?;
    desired.files.insert("auram.json".into(), file);
    let old: Option<Manifest> =
        storage::read_optional(&packs.layout.server().join(".auram-manifest.json"))?;
    packs
        .reconcile(
            &packs.layout.server(),
            &packs.layout.state("server-transaction"),
            old.as_ref(),
            &desired,
            job,
            true,
        )
        .await?;
    storage::write_json(&packs.layout.state("server-deploy-payload.json"), &payload)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            packs.layout.server().join("run.sh"),
            fs::Permissions::from_mode(0o755),
        )?;
    }
    job.progress(
        "server",
        "Server ready. Review the EULA before the first start.",
        1,
        1,
    );
    Ok(())
}
fn write_start_scripts(root: &Path, java: &Path, versions: &Versions) -> Result<()> {
    let java_quote = format!("'{}'", java.to_string_lossy().replace('\'', "'\\''"));
    #[cfg(windows)]
    let windows_java = crate::java_bridge::helper(java)?;
    #[cfg(not(windows))]
    let windows_java = java.to_path_buf();
    let java_windows = windows_java.to_string_lossy().replace('%', "%%");
    let java_mode = if cfg!(windows) {
        "--auram-java-cli"
    } else {
        ""
    };
    storage::atomic_write(&root.join("run.sh"),format!("#!/bin/sh\nset -eu\ncd -- \"$(dirname -- \"$0\")\"\nJAVA={java_quote}\nif [ ! -x \"$JAVA\" ]; then if [ -n \"${{JAVA_HOME:-}}\" ]; then JAVA=\"$JAVA_HOME/bin/java\"; else JAVA=java; fi; fi\nexec \"$JAVA\" @user_jvm_args.txt @libraries/net/minecraftforge/forge/{}-{}/unix_args.txt nogui \"$@\"\n",versions.minecraft,versions.forge).as_bytes())?;
    storage::atomic_write(&root.join("run.bat"),format!("@echo off\r\nchcp 65001 >nul\r\ncd /d \"%~dp0\"\r\nset \"JAVA={java_windows}\"\r\nset \"JAVAMODE={java_mode}\"\r\nif not exist \"%JAVA%\" set \"JAVAMODE=\"\r\nif not exist \"%JAVA%\" if defined JAVA_HOME set \"JAVA=%JAVA_HOME%\\bin\\java.exe\"\r\nif not exist \"%JAVA%\" set \"JAVA=java\"\r\n\"%JAVA%\" %JAVAMODE% @user_jvm_args.txt @libraries/net/minecraftforge/forge/{}-{}/win_args.txt nogui %*\r\n",versions.minecraft,versions.forge).as_bytes())?;
    storage::atomic_write(
        &root.join("user_jvm_args.txt"),
        format!(
            "-Xms512M\n-Xmx{}M\n",
            storage::Settings::default().memory_mib
        )
        .as_bytes(),
    )?;
    storage::atomic_write(
        &root.join("eula.txt"),
        b"# Read https://aka.ms/MinecraftEULA before accepting in Auram.\neula=false\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[derive(Default, Clone)]
struct ModMetadata {
    ids: Vec<String>,
    required: Vec<String>,
    client_only: bool,
}
fn mod_metadata(path: &Path) -> Result<ModMetadata> {
    let mut archive = zip::ZipArchive::new(fs::File::open(path)?)?;
    let forge = archive
        .file_names()
        .any(|name| name == "META-INF/mods.toml");
    if !forge {
        if let Ok(mut entry) = archive.by_name("fabric.mod.json") {
            if entry.size() > 2 * 1024 * 1024 {
                bail!("Mod metadata is too large");
            }
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            let value: serde_json::Value = serde_json::from_str(&text)?;
            return Ok(ModMetadata {
                ids: value["id"]
                    .as_str()
                    .map(|id| vec![id.into()])
                    .unwrap_or_default(),
                client_only: value["environment"] == "client",
                ..Default::default()
            });
        }
        return Ok(ModMetadata::default());
    }
    let mut entry = archive.by_name("META-INF/mods.toml")?;
    if entry.size() > 2 * 1024 * 1024 {
        bail!("Mod metadata is too large");
    }
    let mut text = String::new();
    entry.read_to_string(&mut text)?;
    parse_mod_metadata(&text)
}
fn parse_mod_metadata(text: &str) -> Result<ModMetadata> {
    let metadata: toml::Value = toml::from_str(text)?;
    let mut result = ModMetadata::default();
    for value in metadata
        .get("mods")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(id) = value.get("modId").and_then(toml::Value::as_str) {
            result.ids.push(id.into());
        }
    }
    for deps in metadata
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|table| table.values())
    {
        for dep in deps.as_array().into_iter().flatten() {
            if dep.get("mandatory").and_then(toml::Value::as_bool) != Some(true) {
                continue;
            }
            if dep.get("side").and_then(toml::Value::as_str) == Some("CLIENT") {
                continue;
            }
            if let Some(id) = dep.get("modId").and_then(toml::Value::as_str) {
                if !["minecraft", "forge", "java"].contains(&id) {
                    result.required.push(id.into());
                }
            }
        }
    }
    Ok(result)
}
async fn filter_sided_mods(
    packs: &Packs,
    full: &Manifest,
    config: &ServerConfig,
    desired: &mut Manifest,
    job: &Job,
) -> Result<()> {
    let mut client_ids = std::collections::BTreeSet::<String>::new();
    for path in [
        "minecraft/config/mods_optimizer/mods-database.json",
        "minecraft/config/mods_optimizer/mods-database-override.json",
    ] {
        if !full.files.contains_key(path) {
            continue;
        }
        let value: serde_json::Value = storage::read_json(&packs.file(full, path, job).await?)?;
        for side in ["both", "server"] {
            for id in value[side]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
            {
                client_ids.remove(id);
            }
        }
        for id in value["client"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
        {
            client_ids.insert(id.into());
        }
    }
    let downloads = desired
        .files
        .iter()
        .filter(|(path, _)| path.starts_with("mods/") && path.ends_with(".jar"))
        .map(|(path, file)| packs.download(desired, path, file))
        .collect::<Result<Vec<_>>>()?;
    packs.net.many(downloads, job).await?;
    let mut exclude = Vec::new();
    for (path, file) in &desired.files {
        if !path.starts_with("mods/") || !path.ends_with(".jar") {
            continue;
        }
        let name = path.rsplit('/').next().unwrap_or("");
        if config
            .whitelist
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            continue;
        }
        let metadata = mod_metadata(&packs.blob_path(file))?;
        // This client transformation service links Forge's display window before sided metadata exists.
        let early_display = name
            .to_ascii_lowercase()
            .starts_with("simplecustomearlyloading-");
        if early_display
            || metadata.client_only
            || (!metadata.ids.is_empty() && metadata.ids.iter().all(|id| client_ids.contains(id)))
        {
            exclude.push(path.clone());
        }
    }
    for path in exclude {
        desired.files.remove(&path);
    }
    Ok(())
}

async fn resolve_dependencies(
    packs: &Packs,
    full: &Manifest,
    desired: &mut Manifest,
    job: &Job,
) -> Result<()> {
    let mut metadata = BTreeMap::<String, ModMetadata>::new();
    let mut attempted = std::collections::BTreeSet::new();
    loop {
        let downloads = desired
            .files
            .iter()
            .filter(|(path, _)| path.starts_with("mods/") && path.ends_with(".jar"))
            .map(|(path, file)| packs.download(desired, path, file))
            .collect::<Result<Vec<_>>>()?;
        packs.net.many(downloads, job).await?;
        let mut included = std::collections::BTreeSet::new();
        let mut required = std::collections::BTreeSet::new();
        for (path, file) in &desired.files {
            if !path.starts_with("mods/") || !path.ends_with(".jar") {
                continue;
            }
            let value = metadata
                .entry(file.sha.clone())
                .or_insert(mod_metadata(&packs.blob_path(file))?);
            included.extend(value.ids.iter().cloned());
            required.extend(value.required.iter().cloned());
        }
        let needed = required
            .difference(&included)
            .filter(|id| !attempted.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        if needed.is_empty() {
            break;
        }
        let mut changed = false;
        for id in needed {
            attempted.insert(id.clone());
            let mut candidates = full
                .files
                .iter()
                .filter(|(path, _)| {
                    path.starts_with("minecraft/mods/")
                        && path.ends_with(".jar")
                        && !desired
                            .files
                            .contains_key(path.strip_prefix("minecraft/").unwrap_or(path))
                })
                .collect::<Vec<_>>();
            candidates.sort_by_key(|(path, _)| {
                !path
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .replace('-', "_")
                    .starts_with(&id.replace('-', "_"))
            });
            for (source, file) in candidates {
                job.check()?;
                if !metadata.contains_key(&file.sha) {
                    let path = packs.file(full, source, job).await?;
                    metadata.insert(file.sha.clone(), mod_metadata(&path)?);
                }
                if metadata[&file.sha].ids.contains(&id) {
                    let mut value = file.clone();
                    value.source = Some(source.clone());
                    desired.files.insert(
                        source.strip_prefix("minecraft/").unwrap_or(source).into(),
                        value,
                    );
                    job.progress(
                        "server",
                        format!("Including {id}: required by a server mod"),
                        0,
                        0,
                    );
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            break;
        }
    }
    Ok(())
}

fn local_file(path: &Path) -> Result<PackFile> {
    let mut input = fs::File::open(path)?;
    let size = input.metadata()?.len();
    let mut hash = sha1::Sha1::new();
    hash.update(format!("blob {size}\0"));
    let mut buffer = [0u8; 65536];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        if input.metadata()?.permissions().mode() & 0o111 != 0 {
            "100755"
        } else {
            "100644"
        }
    };
    #[cfg(not(unix))]
    let mode = "100644";
    Ok(PackFile {
        sha: hex::encode(hash.finalize()),
        size,
        mode: mode.into(),
        source: None,
    })
}
pub fn accept_eula(server: &Path, accepted: bool) -> Result<()> {
    if !server.join(".auram-manifest.json").exists() {
        bail!("Build a server first");
    }
    storage::atomic_write(
        &server.join("eula.txt"),
        format!("# Minecraft EULA: https://aka.ms/MinecraftEULA\neula={accepted}\n").as_bytes(),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn excludes_client_mods_and_keeps_whitelist_exceptions() {
        let config = ServerConfig {
            client_mods: vec!["client-".into(), "appleskin-".into()],
            whitelist: vec!["appleskin-".into()],
            ..Default::default()
        };
        assert!(!config.include_mod("client-1.jar"));
        assert!(config.include_mod("appleskin-1.jar"));
        assert!(config.include_mod("server-1.jar"));
    }
    #[test]
    fn remaps_author_paths_and_rejects_escape() {
        assert_eq!(
            relative_resource(
                "/Users/author/Prism/Auram/server_files/server.properties",
                "server_files/"
            )
            .unwrap(),
            "server_files/server.properties"
        );
        assert!(relative_resource("../../outside", "minecraft/").is_err());
    }
    #[test]
    fn mandatory_server_dependencies_override_overbroad_filename_exclusions() {
        let metadata = parse_mod_metadata(
            r#"[[mods]]
modId="ae2"
[[dependencies.ae2]]
modId="guideme"
mandatory=true
side="BOTH"
[[dependencies.ae2]]
modId="client_helper"
mandatory=true
side="CLIENT"
[[dependencies.ae2]]
modId="optional"
mandatory=false
side="BOTH"
"#,
        )
        .unwrap();
        assert_eq!(metadata.ids, vec!["ae2"]);
        assert_eq!(metadata.required, vec!["guideme"]);
    }
}
