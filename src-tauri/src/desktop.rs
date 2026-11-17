use crate::{
    auth::{Auth, Profile},
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
    eula_accepted: bool,
    data_path: String,
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
        eula_accepted: eula.lines().any(|line| line.trim() == "eula=true"),
        data_path: core.layout.root.display().to_string(),
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
            Self::Install | Self::Repair => {
                let mut phases = pack();
                phases.extend(runtime());
                (
                    if matches!(self, Self::Repair) {
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
                    | Operation::Repair
                    | Operation::Launch
                    | Operation::LaunchInstalled
                    | Operation::BuildServer
                    | Operation::ImportLegacy
            )
        {
            bail!("Close Minecraft before changing its installation");
        }
        if matches!(
            operation,
            Operation::Install
                | Operation::Repair
                | Operation::Launch
                | Operation::LaunchInstalled
                | Operation::BuildServer
                | Operation::ImportLegacy
        ) {
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
            open_eula
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
