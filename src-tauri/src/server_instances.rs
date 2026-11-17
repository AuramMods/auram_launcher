//! Saved deployment destinations. Runtime staging is shared; each destination
//! has its own version cache, credentials and recoverable installation state.
use crate::{
    deploy,
    jobs::Job,
    pack::Packs,
    runtime::Runtime,
    server,
    storage::{self, Layout},
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Target {
    Local {
        directory: String,
    },
    Sftp {
        connection: deploy::connection::ConnectionSettings,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInstance {
    pub id: String,
    pub name: String,
    pub target: Target,
    pub known_version: Option<server::ServerVersion>,
    pub checked_at: Option<String>,
    pub last_error: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInput {
    pub id: Option<String>,
    pub name: String,
    pub target: Target,
}
#[derive(Serialize, Deserialize)]
pub struct Registry {
    schema: u32,
    pub servers: Vec<ServerInstance>,
}
impl Registry {
    pub fn load(layout: &Layout) -> Result<Self> {
        let value: Self = match storage::read_optional(&layout.state("servers.json"))? {
            Some(value) => value,
            None => {
                let legacy = deploy::connection::ConnectionSettings::load(layout)?;
                Self {
                    schema: 1,
                    servers: if legacy.host.is_empty() {
                        vec![]
                    } else {
                        vec![ServerInstance {
                            id: "legacy-sftp".into(),
                            name: legacy.host.clone(),
                            target: Target::Sftp { connection: legacy },
                            known_version: None,
                            checked_at: None,
                            last_error: None,
                        }]
                    },
                }
            }
        };
        if value.schema != 1 {
            bail!("Unsupported server list schema");
        }
        Ok(value)
    }
    pub fn write(&self, layout: &Layout) -> Result<()> {
        storage::write_json(&layout.state("servers.json"), self)
    }
    pub fn get(&self, id: &str) -> Result<ServerInstance> {
        self.servers
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .context("Server instance no longer exists")
    }
    pub fn prepare(&self, layout: &Layout, input: ServerInput) -> Result<ServerInstance> {
        let old = input.id.as_deref().map(|id| self.get(id)).transpose()?;
        let name = input.name.trim().to_string();
        if name.is_empty() || name.len() > 120 {
            bail!("Choose a server name of 1–120 characters");
        }
        let mut target = input.target;
        match &mut target {
            Target::Local { directory } => {
                *directory = local_directory(layout, directory)?
                    .to_string_lossy()
                    .into_owned();
            }
            Target::Sftp { connection } => {
                connection.host = connection.host.trim().into();
                connection.username = connection.username.trim().into();
                if connection.host.is_empty()
                    || connection.username.is_empty()
                    || connection.port == 0
                {
                    bail!("Enter the SFTP host, port and username");
                }
                if !["password", "key"].contains(&connection.authentication.as_str()) {
                    bail!("Choose password or private-key authentication");
                }
                // Pins can only be accepted after a real handshake, never by a form.
                connection.fingerprint = match old.as_ref().map(|s| &s.target) {
                    Some(Target::Sftp {
                        connection: previous,
                    }) if previous.host == connection.host && previous.port == connection.port => {
                        previous.fingerprint.clone()
                    }
                    _ => String::new(),
                };
            }
        }
        let unchanged = old.as_ref().is_some_and(|s| s.target == target);
        Ok(ServerInstance {
            id: old
                .as_ref()
                .map(|s| s.id.clone())
                .unwrap_or_else(|| format!("{:032x}", rand::random::<u128>())),
            name,
            target,
            known_version: old
                .as_ref()
                .filter(|_| unchanged)
                .and_then(|s| s.known_version.clone()),
            checked_at: old
                .as_ref()
                .filter(|_| unchanged)
                .and_then(|s| s.checked_at.clone()),
            last_error: None,
        })
    }
    pub fn put(&mut self, instance: ServerInstance) {
        if let Some(old) = self.servers.iter_mut().find(|s| s.id == instance.id) {
            *old = instance;
        } else {
            self.servers.push(instance);
        }
    }
    pub fn observation(&mut self, id: &str, version: Option<server::ServerVersion>) -> Result<()> {
        let mut instance = self.get(id)?;
        instance.known_version = version;
        instance.checked_at = Some(chrono::Utc::now().to_rfc3339());
        instance.last_error = None;
        self.put(instance);
        Ok(())
    }
    pub fn failure(&mut self, id: &str, error: &str) -> Result<()> {
        let mut instance = self.get(id)?;
        instance.last_error = Some(error.into());
        self.put(instance);
        Ok(())
    }
}
pub fn local_directory(layout: &Layout, directory: &str) -> Result<PathBuf> {
    let path = Path::new(directory);
    if !path.is_absolute() {
        bail!("Choose an absolute local server directory");
    }
    // Canonicalization actually opens the selected directory, including aliases.
    let path = fs::canonicalize(path).context("Open the selected server directory")?;
    if !path.is_dir() {
        bail!("The selected server path is not a directory");
    }
    let data = fs::canonicalize(&layout.root)?;
    if path.parent().is_none() || path.starts_with(&data) || data.starts_with(&path) {
        bail!("Choose a server directory outside the launcher's application data");
    }
    Ok(path)
}
pub fn local_journal(layout: &Layout, directory: &Path) -> PathBuf {
    layout
        .state("server-instances")
        .join(hex::encode(sha2::Sha256::digest(
            directory.to_string_lossy().as_bytes(),
        )))
}
/// Prepare the current client baseline automatically; users never manage this
/// internal staging server as an extra step before deploying a saved instance.
pub async fn prepare_payload(packs: &Packs, runtime: &Runtime, job: &Job) -> Result<()> {
    // A warm build verifies and reuses the shared runtime and cached mod
    // metadata. A marker alone must not conceal missing or damaged cache files.
    server::build(packs, runtime, job).await
}
pub async fn deploy_local(
    packs: &Packs,
    directory: &str,
    job: &Job,
) -> Result<server::ServerVersion> {
    use deploy::connection::Remote;
    let destination = local_directory(&packs.layout, directory)?;
    let journal = local_journal(&packs.layout, &destination);
    storage::recover(&destination, &journal)?;
    let remote = deploy::local::LocalRemote::new(destination.clone())?;
    if remote.stat(".auram-deploy/lock").await?.is_some() {
        bail!("Recover the interrupted server deployment before deploying again");
    }
    // Local deployments also provision Forge, launch scripts and defaults.
    // The four payload directories use the same changelist transaction as SFTP.
    // auram.json is committed by that transaction only after both stages succeed.
    let mut runtime: crate::pack::Manifest =
        storage::read_json(&packs.layout.server().join(".auram-manifest.json"))?;
    runtime
        .files
        .retain(|path, _| !deploy::payload_path(path) && path != "auram.json");
    let mut old: Option<crate::pack::Manifest> =
        storage::read_optional(&destination.join(".auram-manifest.json"))?;
    if let Some(old) = &mut old {
        old.files
            .retain(|path, _| !deploy::payload_path(path) && path != "auram.json");
    }
    packs
        .reconcile_server_runtime(&destination, &journal, old.as_ref(), &runtime, job)
        .await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            destination.join("run.sh"),
            fs::Permissions::from_mode(0o755),
        )?;
    }
    let plan = deploy::inspect(packs, &remote, job).await?;
    let version = plan.target_version.clone();
    deploy::apply(packs, &remote, plan, job).await?;
    Ok(version)
}

#[cfg(test)]
mod tests;
