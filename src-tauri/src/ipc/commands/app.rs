use crate::settings::{self, Settings};
use crate::supervisor::Supervisor;
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use ts_rs::TS;

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    crate::tray::request_quit(&app);
}

#[tauri::command]
pub fn stop_all_procs(sup: State<Arc<Supervisor>>) {
    sup.stop_all();
}

/// Open a project's root folder in the OS file manager (Windows Explorer).
#[tauri::command]
pub fn open_in_explorer(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // `explorer.exe <path>` is unreliable when Explorer is already running
        // (it ignores the path and opens the default folder). `cmd /c start`
        // always opens the correct folder via the shell's folder handler.
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &path])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("open_in_explorer is only implemented on Windows".to_string())
    }
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Settings {
    settings::load(&app)
}

/// System-wide RAM + CPU snapshot for the dashboard's stats tile.
#[derive(Serialize, TS)]
pub struct SystemStats {
    pub total_mem_bytes: u64,
    pub used_mem_bytes: u64,
    pub cpu_pct: f32,
}

// `async` so Tauri runs it on a worker thread, not the main/UI thread: sysinfo
// needs two CPU reads a short interval apart for a meaningful delta, so this
// briefly blocks (see `supervisor::sysstats`).
#[tauri::command(async)]
pub fn get_system_stats() -> SystemStats {
    let s = crate::supervisor::sysstats::sample();
    SystemStats {
        total_mem_bytes: s.total_mem_bytes,
        used_mem_bytes: s.used_mem_bytes,
        cpu_pct: s.cpu_pct,
    }
}

/// One project's sampled disk usage, in bytes. See `supervisor::disk`.
#[derive(Serialize, TS)]
pub struct DiskUsage {
    pub project_id: String,
    pub bytes: u64,
}

/// Cached per-project disk usage, sampled on its own ~60s cadence off the
/// main RAM/CPU poll (see `supervisor::disk`). Omits any project not yet
/// sampled - the frontend treats an absent id as "no figure yet", not zero.
#[tauri::command(async)]
pub fn get_disk_usage(sup: State<Arc<Supervisor>>) -> Vec<DiskUsage> {
    let roots: Vec<(String, std::path::PathBuf)> = sup
        .list_projects()
        .into_iter()
        .map(|p| (p.id, std::path::PathBuf::from(p.root)))
        .collect();
    crate::supervisor::disk::sample_and_snapshot(&roots)
        .into_iter()
        .map(|(project_id, bytes)| DiskUsage { project_id, bytes })
        .collect()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    crate::settings::sync_autostart(&app, settings.autostart);
    settings::save(&app, &settings)
}

/// Returns the raw HTTP API bearer token. Callers receive the full secret -
/// never forward it to untrusted renderers or external services.
#[tauri::command]
pub fn get_api_token(app: AppHandle) -> Result<String, String> {
    let path = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("supervisor")
        .join(crate::api::TOKEN_FILE);
    std::fs::read_to_string(&path)
        .map(|s| s.trim().to_string())
        .map_err(|e| e.to_string())
}
