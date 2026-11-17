use crate::{
    auth::{Auth, Profile},
    deploy::{
        self,
        connection::{ConnectionSettings, HostKeyNeeded, Secret},
    },
    download::Downloader,
    game::{Game, GameStatus},
    jobs::{Jobs, PhaseSpec, Progress},
    pack::{PackTag, Packs},
    runtime::{Platform, Runtime, Versions},
    server,
    storage::{self, Layout, Settings},
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_opener::OpenerExt;

pub struct Core {
    pub layout: Layout,
    pub packs: Packs,
    pub runtime: Runtime,
    pub auth: Auth,
    pub jobs: Jobs,
    pub game: Game,
    pub settings: Mutex<Settings>,
}
impl Core {
    fn new() -> Result<Self> {
        let layout = Layout::discover()?;
        #[cfg(feature = "qa")]
        crate::qa_event(
            &layout.root,
            "native_boot",
            "pass",
            "Native application started with isolated QA data",
        );
        layout.resume_client_reset()?;
        storage::recover(&layout.client(), &layout.state("client-transaction"))?;
        storage::recover(&layout.server(), &layout.state("server-transaction"))?;
        let settings = storage::read_optional(&layout.state("settings.json"))?.unwrap_or_default();
        let net = Downloader::new(&layout)?;
        let platform = Platform::current()?;
        Ok(Self {
            packs: Packs::new(layout.clone(), net.clone()),
            runtime: Runtime {
                layout: layout.clone(),
                net: net.clone(),
                platform,
            },
            auth: Auth::new(net.client),
            layout,
            jobs: Jobs::default(),
            game: Game::default(),
            settings: Mutex::new(settings),
        })
    }
    fn settings(&self) -> Settings {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}
pub struct Backend(Mutex<Result<Arc<Core>, String>>);
impl Backend {
    fn new() -> Self {
        Self(Mutex::new(
            Core::new().map(Arc::new).map_err(|e| format!("{e:#}")),
        ))
    }
    pub(crate) fn core(&self) -> Result<Arc<Core>, String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    build: String,
    pack: Option<PackTag>,
    account: Option<Profile>,
    account_error: Option<String>,
    settings: Settings,
    busy: bool,
    progress: Option<Progress>,
    game: GameStatus,
    legacy_available: bool,
    server_ready: bool,
    server_version: Option<server::ServerVersion>,
    sftp: ConnectionSettings,
    eula_accepted: bool,
    data_path: String,
    client_path: String,
}
fn status(core: &Core) -> Result<Status> {
    let account = core.auth.profile();
    let account_error = account
        .as_ref()
        .err()
        .map(|e| format!("Credential storage: {e}"));
    let eula = std::fs::read_to_string(core.layout.server().join("eula.txt")).unwrap_or_default();
    Ok(Status {
        build: env!("AURAM_BUILD_NUMBER").into(),
        pack: core.packs.installed()?.map(|m| m.tag),
        account: account.unwrap_or_default(),
        account_error,
        settings: core.settings(),
        busy: core.jobs.busy(),
        progress: core.jobs.progress(),
        game: core.game.status(),
        legacy_available: Layout::legacy_candidates()
            .iter()
            .any(|p| p.join("minecraft/minecraft").is_dir())
            && !core.layout.state("migration.json").exists(),
        server_ready: core.layout.server().join(".auram-manifest.json").exists(),
        server_version: storage::read_optional(&core.layout.server().join("auram.json"))?,
        sftp: ConnectionSettings::load(&core.layout)?,
        eula_accepted: eula.lines().any(|line| line.trim() == "eula=true"),
        data_path: core.layout.root.display().to_string(),
        client_path: core.layout.client().display().to_string(),
    })
}
#[tauri::command]
fn get_status(backend: tauri::State<Backend>) -> Result<Status, String> {
    status(backend.core()?.as_ref()).map_err(|e| format!("{e:#}"))
}
#[tauri::command]
fn retry_initialize(backend: tauri::State<Backend>) -> Result<(), String> {
    let mut value = backend.0.lock().unwrap_or_else(|e| e.into_inner());
    if value.is_err() {
        *value = Core::new().map(Arc::new).map_err(|e| format!("{e:#}"));
    }
    value.as_ref().map(|_| ()).map_err(Clone::clone)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum Operation {
    Install,
    ForceReinstall,
    Repair,
    Launch,
    LaunchInstalled,
    SignIn,
    SignOut,
    BuildServer,
    ImportLegacy,
}
impl Operation {
    fn progress_plan(&self) -> (&'static str, Vec<PhaseSpec>) {
        let pack = || {
            vec![
                PhaseSpec("pack-plan", "Read pack", false),
                PhaseSpec("pack-files", "Pack files", true),
                PhaseSpec("pack-apply", "Apply files", false),
            ]
        };
        let runtime = || {
            vec![
                PhaseSpec("java", "Java 17", false),
                PhaseSpec("minecraft", "Minecraft", false),
                PhaseSpec("forge", "Forge", false),
                PhaseSpec("assets", "Assets", true),
            ]
        };
        match self {
            Self::Install | Self::Repair | Self::ForceReinstall => {
                let mut phases = pack();
                if matches!(self, Self::ForceReinstall) {
                    phases.insert(0, PhaseSpec("client-reset", "Delete client pack", false));
                }
                phases.extend(runtime());
                (
                    if matches!(self, Self::ForceReinstall) {
                        "Reinstalling Auram"
                    } else if matches!(self, Self::Repair) {
                        "Repairing Auram"
                    } else {
                        "Preparing Auram"
                    },
                    phases,
                )
            }
            Self::Launch | Self::LaunchInstalled => {
                let mut phases = if matches!(self, Self::Launch) {
                    pack()
                } else {
                    vec![]
                };
                phases.extend(runtime());
                phases.extend([
                    PhaseSpec("account", "Account", false),
                    PhaseSpec("launch", "Launch", false),
                ]);
                ("Launching Auram", phases)
            }
            Self::BuildServer => (
                "Building server",
                vec![
                    PhaseSpec("server-pack", "Server inputs", false),
                    PhaseSpec("java", "Java 17", false),
                    PhaseSpec("server-forge", "Forge server", false),
                    PhaseSpec("server-files", "Server files", true),
                    PhaseSpec("server-apply", "Apply files", false),
                ],
            ),
            Self::ImportLegacy => (
                "Importing installation",
                vec![PhaseSpec("migration", "Import files", false)],
            ),
            Self::SignIn | Self::SignOut => (
                "Microsoft account",
                vec![PhaseSpec("account", "Account", false)],
            ),
        }
    }
}
#[tauri::command]
async fn run_operation(
    operation: Operation,
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
) -> Result<(), String> {
    let core = backend.core()?;
    let challenge_app = app.clone();
    let emit = Arc::new(move |progress: Progress| {
        let _ = app.emit("progress", progress);
    });
    let (title, phases) = operation.progress_plan();
    let lease = core
        .jobs
        .begin(title, &phases, emit)
        .map_err(|e| e.to_string())?;
    let job = &lease.job;
    let result: Result<()> = async {
        if core.game.status().running
            && matches!(
                operation,
                Operation::Install
                    | Operation::ForceReinstall
                    | Operation::Repair
                    | Operation::Launch
                    | Operation::LaunchInstalled
                    | Operation::BuildServer
                    | Operation::ImportLegacy
            )
        {
            bail!("Close Minecraft before changing its installation");
        }
        // Force reinstall discards the previous client journal during its reset.
        if matches!(
            operation,
            Operation::Install
                | Operation::Repair
                | Operation::Launch
                | Operation::LaunchInstalled
                | Operation::BuildServer
                | Operation::ImportLegacy
        ) {
            let layout = core.layout.clone();
            tokio::task::spawn_blocking(move || layout.resume_client_reset()).await??;
            // A prior activation can fail to roll back while Windows holds a file lock.
            // Recover before reading version metadata, including the offline launch path.
            storage::recover(
                &core.layout.client(),
                &core.layout.state("client-transaction"),
            )?;
        }
        if matches!(operation, Operation::BuildServer | Operation::ImportLegacy) {
            storage::recover(
                &core.layout.server(),
                &core.layout.state("server-transaction"),
            )?;
        }
        match operation {
            Operation::Install => {
                core.packs.update(false, job).await?;
                let versions = Versions::read(&core.layout.client())?;
                core.runtime.ensure(versions, job).await?;
            }
            Operation::ForceReinstall => {
                core.packs.force_reinstall(job).await?;
                core.runtime
                    .ensure(Versions::read(&core.layout.client())?, job)
                    .await?;
            }
            Operation::Repair => {
                core.packs.update(true, job).await?;
                core.runtime
                    .ensure(Versions::read(&core.layout.client())?, job)
                    .await?;
            }
            Operation::Launch | Operation::LaunchInstalled => {
                if matches!(operation, Operation::Launch) {
                    core.packs.update(false, job).await?;
                }
                let ready = core
                    .runtime
                    .ensure(Versions::read(&core.layout.client())?, job)
                    .await?;
                let settings = core.settings();
                job.phase("account", "Account", false);
                job.progress("account", "Checking your Minecraft session", 0, 0);
                let session = core
                    .auth
                    .session(&settings.microsoft_client_id, job)
                    .await?;
                core.game
                    .launch(&core.runtime, &ready, &session, &settings, job)
                    .await?;
            }
            Operation::SignIn => {
                let settings = core.settings();
                if settings.browser_login {
                    core.auth
                        .sign_in(&settings.microsoft_client_id, job, |url| {
                            tauri_plugin_opener::open_url(url, None::<&str>).map_err(Into::into)
                        })
                        .await?;
                } else {
                    core.auth
                        .sign_in_device(&settings.microsoft_client_id, job, move |challenge| {
                            let _ = challenge_app.emit("device-challenge", challenge);
                        })
                        .await?;
                }
            }
            Operation::SignOut => core.auth.sign_out().await?,
            Operation::BuildServer => server::build(&core.packs, &core.runtime, job).await?,
            Operation::ImportLegacy => {
                let source = Layout::legacy_candidates()
                    .into_iter()
                    .find(|p| p.join("minecraft/minecraft").is_dir())
                    .context("No legacy installation was found")?;
                job.progress(
                    "migration",
                    "Importing existing files; the original installation is retained",
                    0,
                    0,
                );
                core.layout.migrate(&source)?;
            }
        }
        Ok(())
    }
    .await;
    job.finish(result.as_ref().err().map(|e| format!("{e:#}")));
    result.map_err(|e| format!("{e:#}"))
}
#[tauri::command]
fn copy_code_and_open(app: tauri::AppHandle, backend: tauri::State<Backend>) -> Result<(), String> {
    let challenge = backend
        .core()?
        .auth
        .device_challenge()
        .map_err(|e| e.to_string())?;
    app.clipboard()
        .write_text(challenge.code)
        .map_err(|e| e.to_string())?;
    app.opener()
        .open_url(challenge.verification_uri, None::<&str>)
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn cancel_operation(backend: tauri::State<Backend>) -> Result<(), String> {
    backend.core()?.jobs.cancel();
    Ok(())
}
#[tauri::command]
fn save_settings(settings: Settings, backend: tauri::State<Backend>) -> Result<(), String> {
    let core = backend.core()?;
    if core.jobs.busy() {
        return Err("Wait for the current operation to finish".into());
    }
    if !(1024..=32768).contains(&settings.memory_mib) {
        return Err("Choose between 1 and 32 GiB of memory".into());
    }
    if settings.microsoft_client_id.trim().is_empty() {
        return Err("Microsoft application ID is required".into());
    }
    storage::write_json(&core.layout.state("settings.json"), &settings)
        .map_err(|e| e.to_string())?;
    *core.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings;
    Ok(())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum Folder {
    Data,
    Client,
    Server,
    Logs,
}
#[tauri::command]
fn open_folder(
    folder: Folder,
    app: tauri::AppHandle,
    backend: tauri::State<Backend>,
) -> Result<(), String> {
    let core = backend.core()?;
    let path = match folder {
        Folder::Data => core.layout.root.clone(),
        Folder::Client => core.layout.game(),
        Folder::Server => core.layout.server(),
        Folder::Logs => core.layout.root.join("logs"),
    };
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(path.display().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn set_server_eula(accepted: bool, backend: tauri::State<Backend>) -> Result<(), String> {
    let core = backend.core()?;
    if core.jobs.busy() {
        return Err("Wait for the current operation to finish".into());
    }
    server::accept_eula(&core.layout.server(), accepted).map_err(|e| e.to_string())
}
#[tauri::command]
fn open_eula(app: tauri::AppHandle) -> Result<(), String> {
    app.opener()
        .open_url("https://aka.ms/MinecraftEULA", None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn save_sftp(
    settings: ConnectionSettings,
    secret: Option<Secret>,
    backend: tauri::State<Backend>,
) -> Result<(), String> {
    let core = backend.core()?;
    if core.jobs.busy() {
        return Err("Wait for the current operation to finish".into());
    }
    // A pin belongs to the host/port that was actually verified.
    let previous = ConnectionSettings::load(&core.layout).map_err(|e| e.to_string())?;
    let mut settings = settings;
    settings.fingerprint = if settings.host == previous.host && settings.port == previous.port {
        previous.fingerprint
    } else {
        String::new()
    };
    settings
        .save(&core.layout, secret)
        .map_err(|e| format!("Save SFTP connection: {e:#}"))
}
#[tauri::command]
fn forget_sftp(backend: tauri::State<Backend>) -> Result<(), String> {
    let core = backend.core()?;
    if core.jobs.busy() {
        return Err("Wait for the current operation to finish".into());
    }
    ConnectionSettings::load(&core.layout)
        .and_then(|settings| settings.forget(&core.layout))
        .map_err(|e| format!("Forget SFTP connection: {e:#}"))
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SftpCheck {
    host_key: Option<HostKeyNeeded>,
    plan: Option<deploy::DeploymentPlan>,
    remote_version: Option<server::ServerVersion>,
    build_required: bool,
}
#[tauri::command]
async fn check_sftp(
    trust_fingerprint: Option<String>,
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
) -> Result<SftpCheck, String> {
    let core = backend.core()?;
    let emit = app.clone();
    let lease = core
        .jobs
        .begin(
            "Inspecting SFTP server",
            &[
                PhaseSpec("sftp-connect", "Connect", false),
                PhaseSpec("sftp-plan", "Plan server update", false),
            ],
            Arc::new(move |p| {
                let _ = emit.emit("progress", p);
            }),
        )
        .map_err(|e| e.to_string())?;
    let job = &lease.job;
    let result = async {
        let mut settings = ConnectionSettings::load(&core.layout)?;
        if let Some(fingerprint) = trust_fingerprint {
            settings.fingerprint = fingerprint;
        }
        let secret = settings.secret(&core.layout)?;
        job.progress(
            "sftp-connect",
            "Connecting and verifying the SSH host key",
            0,
            0,
        );
        let remote = match deploy::connection::connect(&settings, &secret, job).await {
            Ok(remote) => remote,
            Err(error) => {
                if let Some(key) = error.downcast_ref::<HostKeyNeeded>() {
                    return Ok(SftpCheck {
                        host_key: Some(key.clone()),
                        plan: None,
                        remote_version: None,
                        build_required: false,
                    });
                }
                return Err(error);
            }
        };
        settings.save(&core.layout, None)?;
        let result = async {
            let remote_version = deploy::connection::wait(
                job,
                "reading auram.json",
                std::time::Duration::from_secs(30),
                deploy::read_version(&remote),
            )
            .await?;
            let payload: Option<crate::pack::Manifest> =
                storage::read_optional(&core.layout.state("server-deploy-payload.json"))?;
            let plan = if payload.is_some() {
                Some(deploy::inspect(&core.packs, &remote, job).await?)
            } else {
                None
            };
            Ok::<_, anyhow::Error>(SftpCheck {
                host_key: None,
                plan,
                remote_version,
                build_required: payload.is_none(),
            })
        }
        .await;
        remote.close().await;
        result
    }
    .await;
    job.finish(
        result
            .as_ref()
            .err()
            .map(|e: &anyhow::Error| format!("{e:#}")),
    );
    result.map_err(|e| format!("SFTP inspection failed: {e:#}"))
}
#[tauri::command]
async fn deploy_sftp(
    recover_only: bool,
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
) -> Result<String, String> {
    let core = backend.core()?;
    let lease = core
        .jobs
        .begin(
            if recover_only {
                "Recovering SFTP deployment"
            } else {
                "Deploying server over SFTP"
            },
            &[
                PhaseSpec("sftp-connect", "Connect", false),
                PhaseSpec("sftp-recover", "Recover", false),
                PhaseSpec("sftp-plan", "Plan server update", false),
                PhaseSpec("sftp-upload", "Upload files", true),
                PhaseSpec("sftp-apply", "Activate update", false),
            ],
            Arc::new(move |p| {
                let _ = app.emit("progress", p);
            }),
        )
        .map_err(|e| e.to_string())?;
    let job = &lease.job;
    let result=async {
        let settings=ConnectionSettings::load(&core.layout)?;
        let remote=deploy::connection::connect_pool(&settings,&settings.secret(&core.layout)?,job).await?;
        let result=async {
            if recover_only {deploy::recover(&remote,job).await?;return Ok("Previous remote installation recovered. Inspect the server to prepare another deployment.".into());}
            let plan=deploy::inspect(&core.packs,&remote,job).await?;
            let version=plan.target_version.version.clone();
            deploy::apply(&core.packs,&remote,plan,job).await?;
            Ok::<_,anyhow::Error>(format!("Auram {version} deployed. Start the server when ready."))
        }.await;
        remote.close().await;
        result
    }.await;
    job.finish(
        result
            .as_ref()
            .err()
            .map(|e: &anyhow::Error| format!("{e:#}")),
    );
    result.map_err(|e| format!("SFTP deployment failed: {e:#}"))
}
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
            }
        }))
        .manage(Backend::new())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if let Ok(core) = window.state::<Backend>().core() {
                    if core.game.status().running {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            crate::updater::update_launcher,
            get_status,
            retry_initialize,
            run_operation,
            cancel_operation,
            copy_code_and_open,
            save_settings,
            open_folder,
            set_server_eula,
            open_eula,
            save_sftp,
            forget_sftp,
            check_sftp,
            deploy_sftp
        ])
        .build(tauri::generate_context!())
        .expect("Unable to create the native launcher window");
    app.run(|handle, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if let Ok(core) = handle.state::<Backend>().core() {
                if core.game.status().running {
                    api.prevent_exit();
                }
            }
        }
    });
}
