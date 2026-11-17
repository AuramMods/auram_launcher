use crate::{desktop::Backend, jobs::Progress, storage};
use std::{sync::Arc, time::Duration};
use tauri::{Emitter, State};
use tauri_plugin_updater::UpdaterExt;

#[tauri::command]
pub async fn update_launcher(
    manual: bool,
    app: tauri::AppHandle,
    backend: State<'_, Backend>,
) -> Result<String, String> {
    if cfg!(debug_assertions) && !cfg!(feature = "qa") {
        return Ok("Development builds do not install production updates.".into());
    }
    let core = backend.core()?;
    if core.game.status().running {
        return Ok("Close Minecraft before updating the launcher.".into());
    }
    let emitter = app.clone();
    let lease = core
        .jobs
        .begin(Arc::new(move |progress: Progress| {
            let _ = emitter.emit("progress", progress);
        }))
        .map_err(|e| e.to_string())?;
    let job = &lease.job;
    job.progress("launcherUpdate", "Checking launcher updates", 0, 0);
    let result=async {
        let builder=app.updater_builder().timeout(Duration::from_secs(20));
        #[cfg(feature="qa")]
        let builder=builder.endpoints(vec![std::env::var("AURAM_QA_UPDATE_URL")?.parse()?])?;
        let updater=builder.build()?;
        let Some(update)=updater.check().await? else {return Ok("Your launcher is up to date.".to_string());};
        let marker=core.layout.state("launcher-update-attempt.json");
        if !manual && storage::read_optional::<String>(&marker)?.is_some_and(|version|version==update.version){return Ok("An update was already attempted. Retry from Settings.".into());}
        let mut received=0;
        let bytes=tokio::select! {_=job.cancel.cancelled()=>anyhow::bail!("Launcher update cancelled"),bytes=update.download(|chunk,total|{received+=chunk as u64;(job.emit)(Progress{operation:"launcherUpdate".into(),message:"Downloading launcher update".into(),bytes:received,total_bytes:total.unwrap_or(0),..Default::default()});},||{})=>bytes?};
        job.check()?;
        storage::write_json(&marker,&update.version)?;
        job.progress("launcherUpdate","Installing verified launcher update",0,0);
        update.install(bytes)?;
        #[cfg(feature="qa")] crate::qa_event(&core.layout.root,"update_installed","pass",&update.version);
        app.restart();
        #[allow(unreachable_code)] Ok::<_,anyhow::Error>(String::new())
    }.await;
    #[cfg(feature = "qa")]
    crate::qa_event(
        &core.layout.root,
        "updater_result",
        if result.is_ok() { "pass" } else { "fail" },
        &result
            .as_ref()
            .map(Clone::clone)
            .unwrap_or_else(|e| e.to_string()),
    );
    result.map_err(|e: anyhow::Error| {
        format!("Launcher update unavailable: {e:#}. The installed version is still usable.")
    })
}
