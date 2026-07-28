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

/// Built/installed info for the About page (kit's `getVersionInfo` option).
#[derive(Serialize, TS)]
pub struct VersionInfo {
    pub version: String,
    pub build_date: String,
    pub installed_at: Option<String>,
}

#[tauri::command]
pub async fn get_version_info(app: AppHandle) -> VersionInfo {
    let version = env!("CARGO_PKG_VERSION").to_string();
    let base_date = option_env!("BUILD_DATE").unwrap_or("unknown").to_string();
    let build_date = if base_date == "unknown" {
        base_date
    } else {
        match app.path().app_data_dir().ok() {
            Some(dir) => fetch_build_datetime(&version, &base_date, &dir).await,
            None => base_date,
        }
    };
    let installed_at = load_or_record_install_date(&app, &version);
    VersionInfo {
        version,
        build_date,
        installed_at,
    }
}

/// Returns `"YYYY-MM-DD HH:MM"` for the given version by fetching the GitHub
/// release `published_at` field. Caches the result so only the first call per
/// version hits the network. Falls back to `base_date` (`"YYYY-MM-DD"`) on any
/// error so the UI always shows at least a date.
async fn fetch_build_datetime(version: &str, base_date: &str, data_dir: &std::path::Path) -> String {
    // Local / non-release builds: nothing to fetch.
    if version == "local-build" || version == "unknown" {
        return base_date.to_string();
    }

    #[derive(serde::Deserialize, serde::Serialize)]
    struct BuildTimeCache {
        version: String,
        datetime: String,
    }

    let cache_path = data_dir.join("build-time-cache.json");

    // Cache hit?
    if let Ok(raw) = std::fs::read_to_string(&cache_path) {
        if let Ok(c) = serde_json::from_str::<BuildTimeCache>(&raw) {
            if c.version == version {
                return c.datetime;
            }
        }
    }

    // Fetch from GitHub releases API.
    let url =
        format!("https://api.github.com/repos/SirBepy/server_supervisor/releases/tags/v{version}");
    let result: Option<String> = async {
        #[derive(serde::Deserialize)]
        struct GhRelease {
            published_at: Option<String>,
        }

        let client = reqwest::Client::builder()
            .user_agent("server-supervisor-app")
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .ok()?;
        let resp = client.get(&url).send().await.ok()?;
        let release: GhRelease = resp.json().await.ok()?;
        let iso = release.published_at?;
        // ISO 8601: "2026-06-28T13:35:00Z" -> "2026-06-28 13:35"
        let date_part = iso.get(..10)?;
        let time_part = iso.get(11..16)?;
        Some(format!("{date_part} {time_part}"))
    }
    .await;

    match result {
        Some(datetime) => {
            let cache = BuildTimeCache {
                version: version.to_string(),
                datetime: datetime.clone(),
            };
            if let Ok(json) = serde_json::to_string(&cache) {
                let _ = std::fs::write(&cache_path, json);
            }
            datetime
        }
        None => base_date.to_string(),
    }
}

fn load_or_record_install_date(app: &AppHandle, current_version: &str) -> Option<String> {
    #[derive(serde::Deserialize, serde::Serialize)]
    struct InstallInfo {
        version: String,
        installed_at: String,
    }

    let dir = app.path().app_data_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("install-info.json");

    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(info) = serde_json::from_str::<InstallInfo>(&content) {
            if info.version == current_version {
                return Some(info.installed_at);
            }
        }
    }

    let today = chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string();
    let info = InstallInfo {
        version: current_version.to_string(),
        installed_at: today.clone(),
    };
    if let Ok(json) = serde_json::to_string(&info) {
        let _ = std::fs::write(&path, json);
    }
    Some(today)
}
