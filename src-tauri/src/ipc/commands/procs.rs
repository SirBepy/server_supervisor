use crate::supervisor::Supervisor;
use crate::types::{LogLine, ProcInfo};
use std::sync::Arc;
use tauri::State;

// `async` so Tauri runs it on a worker thread, not the main/UI thread. The body
// is sync (a quick lock + clone now that sampling is cached), but keeping it off
// the main thread means even that brief lock never competes with window message
// pumping.
#[tauri::command(async)]
pub fn list_procs(sup: State<Arc<Supervisor>>) -> Vec<ProcInfo> {
    sup.list()
}

/// Open a running command's port in a browser. Flutter-web ports open in the
/// dedicated CORS-disabled dev browser (a Chromium instance pinned to one
/// profile, so repeat clicks become tabs in the same window); everything else
/// opens in the OS default browser.
#[tauri::command(async)]
pub fn open_port_url(
    sup: State<Arc<Supervisor>>,
    url: String,
    flutter: bool,
) -> Result<(), String> {
    if flutter {
        crate::supervisor::dev_browser::open_flutter_web(&url, &sup.dev_browser_profile_dir())
    } else {
        crate::supervisor::dev_browser::open_default(&url)
    }
}

#[tauri::command]
pub fn start_proc(sup: State<Arc<Supervisor>>, id: String) -> Result<(), String> {
    sup.start(&id)
}

#[tauri::command]
pub fn stop_proc(sup: State<Arc<Supervisor>>, id: String) -> Result<(), String> {
    sup.stop(&id)
}

#[tauri::command]
pub fn restart_proc(sup: State<Arc<Supervisor>>, id: String) -> Result<(), String> {
    sup.restart(&id)
}

#[tauri::command]
pub fn reload_proc(sup: State<Arc<Supervisor>>, id: String, full: bool) -> Result<(), String> {
    sup.reload(&id, full)
}

#[tauri::command]
pub fn get_proc_logs(sup: State<Arc<Supervisor>>, id: String) -> Result<Vec<LogLine>, String> {
    sup.logs(&id)
}
