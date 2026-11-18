use crate::{
    auth::{Auth, Profile},
    download::Downloader,
    game::{Game, GameStatus},
    jobs::{Jobs, PhaseSpec, Progress},
    launch_options::MemoryPreferences,
    pack::{PackTag, Packs},
    runtime::{Platform, Runtime, Versions},
    server,
    server_process::{ServerProcesses, ServerStatus},
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
    pub pack_startup: crate::startup::PackStartup,
    pub local_servers: ServerProcesses,
    pub server_inspections: crate::inspection::Inspections,
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
            pack_startup: crate::startup::PackStartup::default(),
            local_servers: ServerProcesses::default(),
            server_inspections: crate::inspection::Inspections::default(),
        })
    }
    fn settings(&self) -> Settings {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn update_settings(&self, apply: impl FnOnce(&mut Settings)) -> Result<(), String> {
        if self.jobs.busy() {
            return Err("Wait for the current operation to finish".into());
        }
        let mut current = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = current.clone();
        apply(&mut next);
        storage::write_json(&self.layout.state("settings.json"), &next)
            .map_err(|error| error.to_string())?;
        *current = next;
        Ok(())
    }
}
pub struct Backend(Mutex<Result<Arc<Core>, String>>, tokio::sync::Mutex<()>);
impl Backend {
    fn new() -> Self {
        Self(
            Mutex::new(Core::new().map(Arc::new).map_err(|e| format!("{e:#}"))),
            tokio::sync::Mutex::new(()),
        )
    }
    pub(crate) fn core(&self) -> Result<Arc<Core>, String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub(crate) fn location_gate(&self) -> &tokio::sync::Mutex<()> {
        &self.1
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
    memory: crate::launch_options::AutomaticMemory,
    pack_update: crate::startup::PackPreparation,
    busy: bool,
    progress: Option<Progress>,
    game: GameStatus,
    legacy_available: bool,
    servers: Vec<crate::server_instances::ServerInstance>,
    local_servers: Vec<ServerStatus>,
    data_path: String,
    client_path: String,
}
fn status(core: &Core) -> Result<Status> {
    let account = core.auth.profile();
    let account_error = account
        .as_ref()
        .err()
        .map(|e| format!("Credential storage: {e}"));
    Ok(Status {
        build: env!("AURAM_BUILD_NUMBER").into(),
        pack: core.packs.installed()?.map(|m| m.tag),
        account: account.unwrap_or_default(),
        account_error,
        settings: core.settings(),
        memory: crate::launch_options::automatic_memory().clone(),
        pack_update: core.pack_startup.status(),
        busy: core.jobs.busy(),
        progress: core.jobs.progress(),
        game: core.game.status(),
        legacy_available: Layout::legacy_candidates()
            .iter()
            .any(|p| p.join("minecraft/minecraft").is_dir())
            && !core.layout.state("migration.json").exists(),
        servers: crate::server_instances::Registry::load(&core.layout)?.servers,
        local_servers: core.local_servers.status(),
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
            Self::Launch => {
                let mut phases = runtime();
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
async fn prepare_pack(
    core: &Core,
    app: &tauri::AppHandle,
    retry: bool,
) -> crate::startup::PackPreparation {
    core.pack_startup
        .prepare(retry, || async {
            if core.game.status().running {
                bail!("Close Minecraft before updating its installation");
            }
            let app = app.clone();
            let lease = core.jobs.begin(
                "Updating Auram",
                &[
                    PhaseSpec("pack-plan", "Check pack", false),
                    PhaseSpec("pack-files", "Pack files", true),
                    PhaseSpec("pack-apply", "Apply files", false),
                ],
                Arc::new(move |progress| {
                    let _ = app.emit("progress", progress);
                }),
            )?;
            let result = core.packs.prepare_installed(&lease.job).await;
            lease
                .job
                .finish(result.as_ref().err().map(|error| format!("{error:#}")));
            result
        })
        .await
}
#[tauri::command]
async fn prepare_pack_update(
    retry: bool,
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
) -> Result<crate::startup::PackPreparation, String> {
    let core = backend.core()?;
    Ok(prepare_pack(&core, &app, retry).await)
}
#[tauri::command]
async fn run_operation(
    operation: Operation,
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
) -> Result<(), String> {
    let core = backend.core()?;
    if matches!(operation, Operation::Launch) {
        let prepared = prepare_pack(&core, &app, false).await;
        if !prepared.allows_launch() {
            return Err(prepared
                .message
                .unwrap_or_else(|| "Install Auram before launching.".into()));
        }
    }
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
            Operation::Launch => {
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
    if matches!(operation, Operation::Install | Operation::ForceReinstall) {
        if let Ok(Some(installed)) = core.packs.installed() {
            let prepared = match &result {
                Ok(()) => crate::startup::PackPreparation::ready(&installed.tag.name),
                Err(error) if crate::download::unavailable(error) => {
                    crate::startup::PackPreparation::offline(&installed.tag.name, error)
                }
                Err(error) => crate::startup::PackPreparation::failed(format!(
                    "Pack preparation failed: {error:#}"
                )),
            };
            core.pack_startup.record(prepared);
        }
    }
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
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthSettings {
    microsoft_client_id: String,
    browser_login: bool,
}
#[tauri::command]
fn save_settings(settings: AuthSettings, backend: tauri::State<Backend>) -> Result<(), String> {
    if settings.microsoft_client_id.trim().is_empty() {
        return Err("Microsoft application ID is required".into());
    }
    backend.core()?.update_settings(|current| {
        current.microsoft_client_id = settings.microsoft_client_id;
        current.browser_login = settings.browser_login;
    })
}
#[tauri::command]
fn save_memory_preferences(
    preferences: MemoryPreferences,
    backend: tauri::State<Backend>,
) -> Result<(), String> {
    preferences.validate().map_err(|error| error.to_string())?;
    backend
        .core()?
        .update_settings(|settings| settings.memory = preferences)
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
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
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
                    if core.game.status().running || core.local_servers.active() {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            crate::updater::update_launcher,
            get_status,
            prepare_pack_update,
            retry_initialize,
            run_operation,
            cancel_operation,
            copy_code_and_open,
            save_settings,
            save_memory_preferences,
            crate::desktop_location::get_data_location,
            crate::desktop_location::choose_data_directory,
            crate::desktop_location::change_data_location,
            open_folder,
            crate::desktop_servers::choose_server_directory,
            crate::desktop_servers::save_server_instance,
            crate::desktop_servers::remove_server_instance,
            crate::desktop_servers::inspect_server_instance,
            crate::desktop_servers::cancel_server_inspection,
            crate::desktop_servers::deploy_server_instance,
            crate::desktop_servers::open_server_instance,
            crate::desktop_servers::manage_local_server,
            crate::desktop_servers::local_server_console,
            crate::desktop_servers::send_server_command
        ])
        .build(tauri::generate_context!())
        .expect("Unable to create the native launcher window");
    app.run(|handle, event| match event {
        tauri::RunEvent::ExitRequested { api, .. } => {
            if let Ok(core) = handle.state::<Backend>().core() {
                if core.game.status().running || core.local_servers.active() {
                    api.prevent_exit();
                }
            }
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } => {
            if let Some(window) = handle.get_webview_window("main") {
                let _ = window.show();
            }
        }
        _ => {}
    });
}
