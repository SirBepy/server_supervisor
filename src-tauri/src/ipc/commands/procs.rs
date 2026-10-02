use crate::supervisor::Supervisor;
use crate::types::{DockOutcome, DockRect, DockState, LogLine, ProcInfo};
use std::sync::Arc;
use tauri::{AppHandle, State};

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

/// Docks `id`'s window into `rect` (dashboard pane bounds in screen
/// coordinates). Marshals the actual Win32 work onto the main thread inside
/// `Supervisor::dock_window` - see `supervisor::dock` module docs for why a
/// command handler can never do that itself.
#[tauri::command]
pub fn dock_proc_window(
    app: AppHandle,
    sup: State<Arc<Supervisor>>,
    id: String,
    rect: DockRect,
) -> Result<DockOutcome, String> {
    sup.dock_window(&app, &id, rect)
}

#[tauri::command]
pub fn undock_proc_window(app: AppHandle, sup: State<Arc<Supervisor>>, id: String) -> Result<(), String> {
    sup.undock_window(&app, &id)
}

/// Re-places an already-docked proc's window as its pane's layout bounds
/// change. A no-op when `id` isn't actively docked.
#[tauri::command]
pub fn set_proc_dock_bounds(
    app: AppHandle,
    sup: State<Arc<Supervisor>>,
    id: String,
    rect: DockRect,
) -> Result<(), String> {
    sup.reassert_dock(&app, &id, rect)
}

/// The proc's window as a `data:image/png;base64,...` URL, for the dock
/// pane's headless preview. Async so the capture never runs on the main
/// thread that also pumps the headless host's messages.
#[tauri::command(async)]
pub fn capture_proc_window(sup: State<Arc<Supervisor>>, id: String) -> Result<String, String> {
    use base64::Engine;
    let hwnd = sup.window_for(&id)?;
    let png = crate::supervisor::window::capture::capture(hwnd)?.to_png()?;
    Ok(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png)))
}

#[tauri::command]
pub fn get_dock_state(sup: State<Arc<Supervisor>>, id: String) -> Result<DockState, String> {
    Ok(sup.dock_state_for(&id).unwrap_or(DockState::NotDocked))
}
