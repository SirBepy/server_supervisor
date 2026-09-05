use crate::groups::Group;
use crate::ports::{PortEntry, PortRegistry};
use crate::settings::{self, Settings};
use crate::supervisor::proxy_hub::RequestLogEntry;
use crate::supervisor::validate::CommandCheck;
use crate::supervisor::{detect, validate, Supervisor};
use crate::types::{Command, DetectedCommand, LogLine, ProcInfo, Project, Role, UpstreamPreset};
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use ts_rs::TS;

fn data_dir(app: &AppHandle) -> std::path::PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("supervisor")
}

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

#[tauri::command]
pub fn list_projects(sup: State<Arc<Supervisor>>) -> Vec<Project> {
    sup.list_projects()
}

#[tauri::command]
pub fn add_project(
    sup: State<Arc<Supervisor>>,
    name: String,
    root: String,
) -> Result<Project, String> {
    sup.add_project(name, root)
}

#[tauri::command]
pub fn remove_project(sup: State<Arc<Supervisor>>, project_id: String) -> Result<(), String> {
    sup.remove_project(&project_id)
}

#[tauri::command]
pub fn rename_project(
    sup: State<Arc<Supervisor>>,
    project_id: String,
    name: String,
) -> Result<Project, String> {
    sup.rename_project(&project_id, name)
}

#[tauri::command]
pub fn add_command(
    sup: State<Arc<Supervisor>>,
    project_id: String,
    name: String,
    cmd: String,
    autostart: bool,
    use_dynamic_port: bool,
    fixed_port: Option<u16>,
    env: String,
    role: Option<Role>,
) -> Result<Command, String> {
    // Kind is inferred from the command string (None = infer).
    sup.add_command(
        &project_id, name, cmd, None, autostart, use_dynamic_port, fixed_port, env, role,
    )
}

#[tauri::command]
pub fn update_command(
    sup: State<Arc<Supervisor>>,
    project_id: String,
    command_id: String,
    name: String,
    cmd: String,
    autostart: bool,
    use_dynamic_port: bool,
    fixed_port: Option<u16>,
    env: String,
    role: Option<Role>,
) -> Result<Command, String> {
    sup.update_command(
        &project_id, &command_id, name, cmd, autostart, use_dynamic_port, fixed_port, env, role,
    )
}

#[tauri::command]
pub fn remove_command(
    sup: State<Arc<Supervisor>>,
    project_id: String,
    command_id: String,
) -> Result<(), String> {
    sup.remove_command(&project_id, &command_id)
}

/// The fixed loopback port a project's reverse-proxy hub listens on (or would
/// listen on once it has a preset) - the stable address a dev app bakes in.
#[tauri::command]
pub fn get_hub_port(sup: State<Arc<Supervisor>>, project_id: String) -> Result<u16, String> {
    sup.hub_port(&project_id)
}

#[tauri::command]
pub fn add_preset(
    sup: State<Arc<Supervisor>>,
    project_id: String,
    name: String,
    base_url: String,
    danger: bool,
) -> Result<UpstreamPreset, String> {
    sup.add_preset(&project_id, name, base_url, danger)
}

#[tauri::command]
pub fn remove_preset(
    sup: State<Arc<Supervisor>>,
    project_id: String,
    preset_id: String,
) -> Result<(), String> {
    sup.remove_preset(&project_id, &preset_id)
}

#[tauri::command]
pub fn set_active_preset(
    sup: State<Arc<Supervisor>>,
    project_id: String,
    preset_id: String,
) -> Result<(), String> {
    sup.set_active_preset(&project_id, &preset_id)
}

#[tauri::command]
pub fn get_hub_log(sup: State<Arc<Supervisor>>, project_id: String) -> Vec<RequestLogEntry> {
    sup.hub_log(&project_id)
}

#[tauri::command]
pub fn detect_commands(path: String) -> Vec<DetectedCommand> {
    detect::detect(std::path::Path::new(&path))
}

#[tauri::command]
pub fn validate_command(root: String, cmd: String) -> CommandCheck {
    validate::validate_command(&root, &cmd)
}

#[tauri::command]
pub fn list_ports(reg: State<Arc<PortRegistry>>) -> Vec<PortEntry> {
    reg.list()
}

#[tauri::command]
pub fn reserve_port(reg: State<Arc<PortRegistry>>, owner: String) -> u16 {
    reg.reserve_next(&owner)
}

/// Returns the raw HTTP API bearer token. Callers receive the full secret —
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

#[tauri::command]
pub fn list_groups(app: AppHandle) -> Vec<Group> {
    crate::groups::load(&data_dir(&app))
}

#[tauri::command]
pub fn create_group(app: AppHandle, name: String) -> Result<Group, String> {
    crate::groups::create(&data_dir(&app), name)
}

#[tauri::command]
pub fn update_group(app: AppHandle, id: String, name: String) -> Result<Group, String> {
    crate::groups::update(&data_dir(&app), &id, name)
}

#[tauri::command]
pub fn delete_group(app: AppHandle, id: String) -> Result<(), String> {
    crate::groups::delete(&data_dir(&app), &id)
}

#[tauri::command]
pub fn set_project_group(
    app: AppHandle,
    project_id: String,
    group_id: Option<String>,
) -> Result<(), String> {
    crate::groups::set_project_group(&data_dir(&app), &project_id, group_id.as_deref())
}
