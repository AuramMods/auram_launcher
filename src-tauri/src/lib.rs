pub mod auth;
pub mod data_location;
pub mod deploy;
#[cfg(feature = "desktop")]
mod desktop;
#[cfg(feature = "desktop")]
mod desktop_location;
#[cfg(feature = "desktop")]
mod desktop_servers;
pub mod download;
pub mod game;
mod game_branding;
pub mod inspection;
pub mod jobs;
pub mod launch_options;
pub mod local_work;
#[cfg(target_os = "macos")]
mod mac_game_helper;
pub mod pack;
pub mod runtime;
pub mod server;
pub mod server_instances;
pub mod server_process;
pub mod startup;
pub mod storage;
#[cfg(feature = "desktop")]
pub use desktop::run;

#[cfg(feature = "desktop")]
mod updater;

#[cfg(feature = "qa")]
pub fn qa_event(root: &std::path::Path, event: &str, status: &str, details: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(root.join("qa.log"))
    {
        let _ = writeln!(
            file,
            "QA_EVT {}",
            serde_json::json!({"event":event,"status":status,"details":details,"context":{"build":env!("AURAM_BUILD_NUMBER"),"commit":env!("AURAM_COMMIT")}})
        );
    }
}
pub mod java_bridge;
