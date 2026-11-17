use crate::{
    jobs::Job,
    pack::{Manifest, PackFile, Packs},
    runtime::{self, Runtime, Versions},
    storage,
};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha1::Digest;
use std::{collections::BTreeMap, fs, io::Read, path::Path};

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
    let installed = packs
        .installed()?
        .context("Install the client pack before building a server")?;
    let full = packs.manifest(installed.tag.clone(), job).await?;
    let config: ServerConfig = toml::from_str(&fs::read_to_string(
        packs.file(&full, "server_config.conf", job).await?,
    )?)
    .context("Invalid server_config.conf")?;
    let mut desired = config.manifest(&full)?;
    let versions = Versions::read(&packs.layout.client())?;
    let java = runtime.java(job).await?;
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
        let java_quote = format!("'{}'", java.to_string_lossy().replace('\'', "'\\''"));
        let java_windows = java.to_string_lossy().replace('%', "%%");
        storage::atomic_write(&staging.join("run.sh"),format!("#!/bin/sh\nset -eu\ncd -- \"$(dirname -- \"$0\")\"\nif [ -n \"${{JAVA_HOME:-}}\" ]; then JAVA=\"$JAVA_HOME/bin/java\"; elif command -v java >/dev/null 2>&1; then JAVA=java; else JAVA={java_quote}; fi\nexec \"$JAVA\" @user_jvm_args.txt @libraries/net/minecraftforge/forge/{}-{}/unix_args.txt nogui \"$@\"\n",versions.minecraft,versions.forge).as_bytes())?;
        storage::atomic_write(&staging.join("run.bat"),format!("@echo off\r\ncd /d \"%~dp0\"\r\nset \"JAVA={java_windows}\"\r\nif defined JAVA_HOME set \"JAVA=%JAVA_HOME%\\bin\\java.exe\"\r\nif not exist \"%JAVA%\" set \"JAVA=java\"\r\n\"%JAVA%\" @user_jvm_args.txt @libraries/net/minecraftforge/forge/{}-{}/win_args.txt nogui %*\r\n",versions.minecraft,versions.forge).as_bytes())?;
        storage::atomic_write(&staging.join("user_jvm_args.txt"), b"-Xms512M\n-Xmx4G\n")?;
        storage::atomic_write(
            &staging.join("eula.txt"),
            b"# Read https://aka.ms/MinecraftEULA before accepting in Auram.\neula=false\n",
        )?;
        runtime::snapshot_tree(&staging, &staging.join(".verified.json"), &[])?;
        if cache.exists() {
            fs::remove_dir_all(&cache)?;
        }
        fs::create_dir_all(cache.parent().context("Server cache parent missing")?)?;
        fs::rename(staging, &cache)?;
    }
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
    Ok(PackFile {
        sha: hex::encode(hash.finalize()),
        size,
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
}
