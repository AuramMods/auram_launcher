pub mod download;
pub mod jobs;
pub mod storage;
#[cfg(feature = "desktop")]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_single_instance::init(|_, _, _| {}))
        .invoke_handler(tauri::generate_handler![ping])
        .run(tauri::generate_context!())
        .expect("Unable to run Auram Launcher");
}

#[cfg(feature = "desktop")]
#[tauri::command]
fn ping() -> String {
    "Auram native backend is ready".into()
}
