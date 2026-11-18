//! Directory selection and data moves stay behind scoped Rust commands.
use crate::{
    data_location::Locations,
    desktop::Backend,
    jobs::{Jobs, PhaseSpec, Progress},
    local_work,
};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};
use tauri::Emitter;
use tauri_plugin_dialog::DialogExt;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataLocation {
    #[serde(flatten)]
    location: crate::data_location::LocationStatus,
    application_path: String,
    platform: &'static str,
}
#[tauri::command]
pub fn get_data_location() -> Result<DataLocation, String> {
    Ok(DataLocation {
        location: Locations::discover()
            .and_then(|store| store.status())
            .map_err(|e| format!("{e:#}"))?,
        platform: if cfg!(windows) { "windows" } else { "macos" },
        application_path: std::env::current_exe()
            .map(|path| {
                let application = if cfg!(target_os = "macos") {
                    path.ancestors()
                        .find(|parent| parent.extension().is_some_and(|ext| ext == "app"))
                        .or_else(|| path.parent())
                        .unwrap_or(&path)
                } else {
                    path.parent().unwrap_or(&path)
                };
                application.to_string_lossy().into_owned()
            })
            .unwrap_or_default(),
    })
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    parent_directory: String,
    data_directory: String,
}
#[tauri::command]
pub async fn choose_data_directory(app: tauri::AppHandle) -> Result<Option<Choice>, String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose a parent folder for Auram's data")
        .pick_folder(move |folder| {
            let _ = send.send(folder);
        });
    let folder = receive.await.map_err(|e| e.to_string())?;
    folder
        .map(|folder| {
            let parent = folder.into_path().map_err(|e| e.to_string())?;
            let target = Locations::discover()
                .and_then(|store| store.destination(Some(&parent)))
                .map_err(|e| format!("{e:#}"))?;
            Ok(Choice {
                parent_directory: parent.to_string_lossy().into_owned(),
                data_directory: target.to_string_lossy().into_owned(),
            })
        })
        .transpose()
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Change {
    Move {
        #[serde(rename = "parentDirectory")]
        parent_directory: String,
    },
    Reset,
    Retry,
    Cancel,
    ResetUnavailable,
}
#[tauri::command]
pub async fn change_data_location(
    change: Change,
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
) -> Result<Option<String>, String> {
    let _guard = backend
        .location_gate()
        .try_lock()
        .map_err(|_| "A data-location change is already running")?;
    let core = backend.core();
    let fallback = Jobs::default();
    let jobs = core.as_ref().map_or(&fallback, |core| &core.jobs);
    let emitter = app.clone();
    let lease = jobs
        .begin(
            "Moving launcher data",
            &[
                PhaseSpec("data-plan", "Inspect files", false),
                PhaseSpec("data-copy", "Copy files", true),
                PhaseSpec("data-verify", "Verify copy", true),
                PhaseSpec("data-activate", "Activate location", false),
                PhaseSpec("data-cleanup", "Finish move", true),
            ],
            Arc::new(move |progress: Progress| {
                let _ = emitter.emit("progress", progress);
            }),
        )
        .map_err(|e| e.to_string())?;
    if core.as_ref().is_ok_and(|core| core.game.status().running) {
        return Err("Close Minecraft before changing the data location".into());
    }
    if core.as_ref().is_ok_and(|core| core.local_servers.active()) {
        return Err("Stop local servers before changing the data location".into());
    }
    let store = Locations::discover().map_err(|e| format!("{e:#}"))?;
    let job = lease.job.clone();
    let result=match change {
        Change::ResetUnavailable=>local_work::blocking(move ||{store.reset_unavailable()?;Ok(crate::data_location::MoveOutcome{restart:true,warning:None})}).await,
        Change::Cancel=>local_work::blocking(move ||{store.cancel()?;Ok(crate::data_location::MoveOutcome{restart:false,warning:Some("Unfinished move cancelled. Prepared copies were retained beside their destination.".into())})}).await,
        change=>{
            let core=core.clone()?;
            let source=core.layout.clone();
            local_work::blocking(move || {
                let destination=match change {
                    Change::Move{parent_directory}=>store.destination(Some(&PathBuf::from(parent_directory)))?,
                    Change::Reset=>store.destination(None)?,Change::Retry=>store.retry_destination()?,
                    _=>unreachable!(),
                };
                store.relocate(&source,destination,&job)
            }).await
        }
    };
    lease
        .job
        .finish(result.as_ref().err().map(|e| format!("{e:#}")));
    let outcome = result.map_err(|e| format!("{e:#}"))?;
    if outcome.restart {
        if cfg!(debug_assertions) {
            if let Some(marker) = std::env::var_os("AURAM_DEV_RESTART_FILE") {
                if crate::storage::write_json(
                    std::path::Path::new(&marker),
                    &serde_json::json!({"schema":1,"restart":true}),
                )
                .is_ok()
                {
                    app.exit(0);
                    return Ok(Some(
                        "Restarting the development launcher at its selected data folder.".into(),
                    ));
                }
            }
        }
        #[cfg(feature = "qa")]
        if let Ok(store) = Locations::discover() {
            crate::qa_event(
                &store.default_root,
                "data_location_committed",
                "pass",
                "Verified data folder selected; restarting",
            );
        }
        app.restart();
    }
    Ok(outcome.warning)
}
