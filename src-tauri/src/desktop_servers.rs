use crate::{
    deploy::{
        self,
        connection::{ConnectionSettings, HostKeyNeeded, Remote, Secret},
    },
    desktop::Backend,
    jobs::{Job, PhaseSpec},
    server::ServerVersion,
    server_instances::{self, Registry, ServerInput, ServerInstance, Target},
    server_process::{self, ConsoleLine},
    storage,
};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc, time::Duration};
use tauri::Emitter;
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
pub async fn choose_server_directory(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose or create a server directory")
        .pick_folder(move |folder| {
            let _ = send.send(folder);
        });
    let folder = receive.await.map_err(|error| error.to_string())?;
    folder
        .map(|folder| {
            folder
                .into_path()
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(|error| error.to_string())
        })
        .transpose()
}
fn save_credentials(
    settings: &ConnectionSettings,
    id: &str,
    secret: Option<Secret>,
    core: &crate::desktop::Core,
) -> Result<()> {
    if let Some(secret) = secret {
        settings.save_instance_secret(&core.layout, id, secret)?;
    }
    Ok(())
}
#[tauri::command]
pub async fn save_server_instance(
    input: ServerInput,
    secret: Option<Secret>,
    backend: tauri::State<'_, Backend>,
) -> Result<ServerInstance, String> {
    let core = backend.core()?;
    let lease = core
        .jobs
        .begin("Saving server", &[], Arc::new(|_| {}))
        .map_err(|e| e.to_string())?;
    let result = (|| {
        let mut registry = Registry::load(&core.layout)?;
        let previous = input.id.as_deref().map(|id| registry.get(id)).transpose()?;
        let instance = registry.prepare(&core.layout, input)?;
        if core.local_servers.running(&instance.id)
            && previous
                .as_ref()
                .is_some_and(|old| old.target != instance.target)
        {
            bail!("Stop the local server before changing its folder");
        }
        if let Target::Sftp { connection } = &instance.target {
            save_credentials(connection, &instance.id, secret, &core)?;
        }
        registry.put(instance.clone());
        registry.write(&core.layout)?;
        if let Some(ServerInstance {
            id,
            target: Target::Sftp { connection: old },
            ..
        }) = previous
        {
            let same = match &instance.target {
                Target::Sftp { connection } => {
                    old.instance_identity(&core.layout, &id)
                        == connection.instance_identity(&core.layout, &id)
                }
                _ => false,
            };
            if !same {
                old.forget_instance_secret(&core.layout, &id)?;
            }
        }
        Ok::<_, anyhow::Error>(instance)
    })();
    lease
        .job
        .finish(result.as_ref().err().map(|e| format!("{e:#}")));
    result.map_err(|e| format!("Save server: {e:#}"))
}
#[tauri::command]
pub async fn select_server_pack_version(
    id: String,
    version: Option<String>,
    backend: tauri::State<'_, Backend>,
) -> Result<(), String> {
    let core = backend.core()?;
    let lease = core
        .jobs
        .begin("Selecting server pack version", &[], Arc::new(|_| {}))
        .map_err(|error| error.to_string())?;
    let result = async {
        let mut registry = Registry::load(&core.layout)?;
        let mut instance = registry.get(&id)?;
        instance.pack_version = match version {
            Some(version) => Some(core.packs.release(Some(&version), &lease.job).await?),
            None => None,
        };
        registry.put(instance);
        registry.write(&core.layout)
    }
    .await;
    lease
        .job
        .finish(result.as_ref().err().map(|error| format!("{error:#}")));
    result.map_err(|error| format!("{error:#}"))
}

#[tauri::command]
pub async fn remove_server_instance(
    id: String,
    backend: tauri::State<'_, Backend>,
) -> Result<(), String> {
    let core = backend.core()?;
    let lease = core
        .jobs
        .begin("Removing saved server", &[], Arc::new(|_| {}))
        .map_err(|e| e.to_string())?;
    let result = (|| {
        let mut registry = Registry::load(&core.layout)?;
        let instance = registry.get(&id)?;
        if core.local_servers.running(&id) {
            bail!("Stop the local server before removing it");
        }
        if let Target::Sftp { connection } = instance.target {
            connection.forget_instance_secret(&core.layout, &id)?;
        }
        registry.servers.retain(|s| s.id != id);
        registry.write(&core.layout)?;
        core.local_servers.forget_stopped(&id)
    })();
    lease
        .job
        .finish(result.as_ref().err().map(|e| format!("{e:#}")));
    result.map_err(|e| format!("Remove saved server: {e:#}"))
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    version: Option<ServerVersion>,
    host_key: Option<HostKeyNeeded>,
    recovery_required: bool,
}
async fn read_inspection(remote: &impl Remote, job: &Job) -> Result<Inspection> {
    deploy::connection::wait(
        job,
        "reading the server version and deployment journal",
        Duration::from_secs(30),
        async {
            Ok(Inspection {
                version: deploy::read_version(remote).await?,
                host_key: None,
                recovery_required: remote.stat(".auram-deploy/lock").await?.is_some(),
            })
        },
    )
    .await
}
#[tauri::command]
pub async fn inspect_server_instance(
    id: String,
    request_id: String,
    trust_fingerprint: Option<String>,
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
) -> Result<Inspection, String> {
    let core = backend.core()?;
    let emitter = app.clone();
    let lease = core
        .jobs
        .begin(
            "Checking server version",
            &[
                PhaseSpec("server-connect", "Connect", false),
                PhaseSpec("server-version", "Read version", false),
            ],
            Arc::new(move |p| {
                let _ = emitter.emit("progress", p);
            }),
        )
        .map_err(|e| e.to_string())?;
    let job = &lease.job;
    let _registration = core
        .server_inspections
        .register(&request_id, job.cancel.clone())
        .map_err(|e| e.to_string())?;
    let result = async {
        job.check()?;
        let mut registry = Registry::load(&core.layout)?;
        let mut instance = registry.get(&id)?;
        let inspection = match &mut instance.target {
            Target::Local { directory } => {
                let root = server_instances::local_directory(&core.layout, directory)?;
                if !core.local_servers.uses(&root) {
                    storage::recover(&root, &server_instances::local_journal(&core.layout, &root))?;
                }
                job.phase("server-version", "Read version", false);
                read_inspection(&deploy::local::LocalRemote::new(root)?, job).await?
            }
            Target::Sftp { connection } => {
                if let Some(fingerprint) = trust_fingerprint {
                    connection.fingerprint = fingerprint;
                }
                job.progress(
                    "server-connect",
                    "Connecting and verifying the SSH host key",
                    0,
                    0,
                );
                let remote = match deploy::connection::connect(
                    connection,
                    &connection.instance_secret(&core.layout, &id)?,
                    job,
                )
                .await
                {
                    Ok(remote) => remote,
                    Err(error) => {
                        if let Some(key) = error.downcast_ref::<HostKeyNeeded>() {
                            return Ok(Inspection {
                                version: None,
                                host_key: Some(key.clone()),
                                recovery_required: false,
                            });
                        }
                        return Err(error);
                    }
                };
                job.phase("server-version", "Read version", false);
                let result = read_inspection(&remote, job).await;
                remote.close().await;
                result?
            }
        };
        registry.put(instance);
        registry.observation(&id, inspection.version.clone())?;
        registry.write(&core.layout)?;
        Ok::<_, anyhow::Error>(inspection)
    }
    .await;
    if let Err(error) = &result {
        if !job.cancel.is_cancelled() {
            record_failure(&core, &id, &format!("{error:#}"));
        }
    }
    job.finish(result.as_ref().err().map(|e| format!("{e:#}")));
    result.map_err(|e| format!("Check server version: {e:#}"))
}
#[tauri::command]
pub fn cancel_server_inspection(
    request_id: String,
    backend: tauri::State<Backend>,
) -> Result<(), String> {
    backend
        .core()?
        .server_inspections
        .cancel(&request_id)
        .map_err(|e| e.to_string())
}
fn record_failure(core: &crate::desktop::Core, id: &str, error: &str) {
    if let Ok(mut registry) = Registry::load(&core.layout) {
        if registry.failure(id, error).is_ok() {
            let _ = registry.write(&core.layout);
        }
    }
}
#[tauri::command]
pub async fn deploy_server_instance(
    id: String,
    recover_only: bool,
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
) -> Result<String, String> {
    let core = backend.core()?;
    let emitter = app.clone();
    let lease = core
        .jobs
        .begin(
            if recover_only {
                "Recovering server deployment"
            } else {
                "Deploying server"
            },
            &[
                PhaseSpec("server-pack", "Server inputs", false),
                PhaseSpec("java", "Java 17", false),
                PhaseSpec("server-forge", "Forge server", false),
                PhaseSpec("server-files", "Server files", true),
                PhaseSpec("server-apply", "Prepare files", false),
                PhaseSpec("destination-files", "Local runtime", true),
                PhaseSpec("destination-apply", "Install runtime", false),
                PhaseSpec("sftp-connect", "Destination", false),
                PhaseSpec("sftp-recover", "Recover", true),
                PhaseSpec("sftp-plan", "Plan update", false),
                PhaseSpec("sftp-destination", "Check destination", true),
                PhaseSpec("sftp-stage", "Prepare folders", true),
                PhaseSpec("sftp-upload", "Transfer files", true),
                PhaseSpec("sftp-validate", "Verify destination", true),
                PhaseSpec("sftp-apply", "Activate update", true),
            ],
            Arc::new(move |p| {
                let _ = emitter.emit("progress", p);
            }),
        )
        .map_err(|e| e.to_string())?;
    let job = &lease.job;
    let result = async {
        if core.game.status().running {
            bail!("Close Minecraft before preparing a server deployment");
        }
        let instance = Registry::load(&core.layout)?.get(&id)?;
        let explicit = instance.pack_version.is_some();
        if let Target::Local { directory } = &instance.target {
            let root = server_instances::local_directory(&core.layout, directory)?;
            if core.local_servers.uses(&root) {
                bail!("Stop the local server before deploying to this folder");
            }
        }
        if !recover_only {
            if let Some(tag) = instance.pack_version.clone() {
                crate::server::build_release(&core.packs, &core.runtime, tag, job).await?;
            } else {
                server_instances::prepare_payload(&core.packs, &core.runtime, job).await?;
            }
        }
        let version = match &instance.target {
            Target::Local { directory } => {
                let destination = server_instances::local_directory(&core.layout, directory)?;
                let remote = deploy::local::LocalRemote::new(destination)?;
                if recover_only {
                    deploy::recover(&remote, job).await?;
                    deploy::read_version(&remote).await?
                } else {
                    Some(
                        server_instances::deploy_local_selected(
                            &core.packs,
                            directory,
                            explicit,
                            job,
                        )
                        .await?,
                    )
                }
            }
            Target::Sftp { connection } => {
                job.phase("sftp-connect", "Destination", false);
                let remote = deploy::connection::connect_pool(
                    connection,
                    &connection.instance_secret(&core.layout, &id)?,
                    job,
                )
                .await?;
                let result = async {
                    if recover_only {
                        deploy::recover(&remote, job).await?;
                        return deploy::read_version(&remote).await;
                    }
                    let plan =
                        deploy::inspect_selected(&core.packs, &remote, explicit, job).await?;
                    if let Some(note) = plan
                        .notes
                        .iter()
                        .find(|note| note.starts_with("The target requires Forge"))
                    {
                        job.detail("sftp-plan", note);
                    }
                    let version = plan.target_version.clone();
                    deploy::apply(&core.packs, &remote, plan, job).await?;
                    Ok::<_, anyhow::Error>(Some(version))
                }
                .await;
                remote.close().await;
                result?
            }
        };
        let mut registry = Registry::load(&core.layout)?;
        registry.observation(&id, version.clone())?;
        registry.write(&core.layout)?;
        if recover_only {
            Ok("Previous server deployment recovered. Reopen the server to deploy again.".into())
        } else {
            Ok(format!(
                "Auram {} deployed to {}.",
                version.map(|v| v.version).unwrap_or_default(),
                instance.name
            ))
        }
    }
    .await;
    if let Err(error) = &result {
        record_failure(&core, &id, &format!("{error:#}"));
    }
    job.finish(result.as_ref().err().map(|e| format!("{e:#}")));
    result.map_err(|e| format!("Server deployment failed: {e:#}"))
}
#[tauri::command]
pub fn open_server_instance(
    id: String,
    app: tauri::AppHandle,
    backend: tauri::State<Backend>,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let core = backend.core()?;
    (|| {
        let instance = Registry::load(&core.layout)?.get(&id)?;
        let Target::Local { directory } = instance.target else {
            bail!("This server is hosted over SFTP");
        };
        let directory = server_instances::local_directory(&core.layout, &directory)?;
        app.opener()
            .open_path(directory.to_string_lossy(), None::<&str>)?;
        Ok::<_, anyhow::Error>(())
    })()
    .map_err(|e| format!("Open server directory: {e:#}"))
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LocalServerAction {
    Start,
    Stop,
    Restart,
    Close,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum LocalServerOutcome {
    Done,
    EulaRequired { directory: String },
}

#[tauri::command]
pub fn open_minecraft_eula() -> Result<(), String> {
    tauri_plugin_opener::open_url(crate::server::EULA_URL, None::<&str>)
        .map_err(|error| format!("Open the Minecraft EULA: {error}"))
}

#[tauri::command]
pub async fn manage_local_server(
    id: String,
    action: LocalServerAction,
    accept_eula_for: Option<String>,
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
) -> Result<LocalServerOutcome, String> {
    let core = backend.core()?;
    if matches!(action, LocalServerAction::Stop | LocalServerAction::Close) {
        return async {
            core.local_servers.stop(&id).await?;
            if action == LocalServerAction::Close {
                core.local_servers.forget_stopped(&id)?;
            }
            Ok::<_, anyhow::Error>(LocalServerOutcome::Done)
        }
        .await
        .map_err(|error| format!("{error:#}"));
    }
    let restarting = action == LocalServerAction::Restart;
    let mut phases = Vec::new();
    if restarting {
        phases.push(PhaseSpec("server-stop", "Stop server", false));
    }
    phases.extend([
        PhaseSpec("java", "Java 17", false),
        PhaseSpec("server-start", "Start server", false),
    ]);
    let lease = core
        .jobs
        .begin(
            if restarting {
                "Restarting server"
            } else {
                "Starting server"
            },
            &phases,
            Arc::new(move |progress| {
                let _ = app.emit("progress", progress);
            }),
        )
        .map_err(|error| error.to_string())?;
    let job = &lease.job;
    let result = async {
        let instance = Registry::load(&core.layout)?.get(&id)?;
        let Target::Local { directory } = instance.target else {
            bail!("Only local servers can be started here");
        };
        let directory = server_instances::local_directory(&core.layout, &directory)?;
        if accept_eula_for
            .as_deref()
            .is_some_and(|expected| Path::new(expected) != directory)
        {
            bail!("The server folder has changed. Start it again to review the EULA.");
        }
        if core.local_servers.uses(&directory) {
            if !restarting || !core.local_servers.running(&id) {
                bail!("A local server is already running in this folder");
            }
        } else {
            storage::recover(
                &directory,
                &server_instances::local_journal(&core.layout, &directory),
            )?;
        }
        let deployment = server_process::deployed_arguments(
            &directory,
            core.runtime.platform,
            instance.memory_gib,
        )
        .await?;
        job.check()?;
        if !server_process::ensure_eula(&directory, accept_eula_for.is_some())? {
            return Ok(LocalServerOutcome::EulaRequired {
                directory: directory.to_string_lossy().into_owned(),
            });
        }
        if restarting {
            job.progress(
                "server-stop",
                "Waiting for the server to save and stop",
                0,
                0,
            );
            tokio::select! {
                result = core.local_servers.stop(&id) => result?,
                _ = job.cancel.cancelled() => bail!("Server restart cancelled"),
            }
            job.complete_phase();
        }
        if core.local_servers.uses(&directory) {
            bail!("A local server is already running in this folder");
        }
        job.phase("java", "Java 17", false);
        let arguments =
            server_process::launch_arguments(&directory, &core.runtime, deployment, job).await?;
        let java = core.runtime.java(job).await?;
        job.check()?;
        job.phase("server-start", "Start server", false);
        core.local_servers
            .start(&id, directory, &java, &arguments)?;
        Ok(LocalServerOutcome::Done)
    }
    .await;
    job.finish(result.as_ref().err().map(|error| format!("{error:#}")));
    result.map_err(|error| format!("{error:#}"))
}

#[tauri::command]
pub fn local_server_console(
    id: String,
    after: u64,
    backend: tauri::State<'_, Backend>,
) -> Result<Vec<ConsoleLine>, String> {
    Ok(backend.core()?.local_servers.console(&id, after))
}

#[tauri::command]
pub async fn send_server_command(
    id: String,
    command: String,
    backend: tauri::State<'_, Backend>,
) -> Result<(), String> {
    backend
        .core()?
        .local_servers
        .send(&id, &command)
        .await
        .map_err(|error| format!("{error:#}"))
}
