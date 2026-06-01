use crate::ports::{PortEntry, PortRegistry};
use crate::settings::{self, Settings};
use crate::state::AppState;
use crate::supervisor::validate::CommandCheck;
use crate::supervisor::{detect, validate, Supervisor};
use crate::types::{Command, DetectedCommand, LogLine, ProcInfo, Project};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    if let Some(s) = app.try_state::<AppState>() {
        s.should_quit.store(true, Ordering::SeqCst);
    }
    app.exit(0);
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Settings {
    settings::load(&app)
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    settings::save(&app, &settings)
}

#[tauri::command]
pub fn list_procs(sup: State<Arc<Supervisor>>) -> Vec<ProcInfo> {
    sup.list()
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
pub fn add_command(
    sup: State<Arc<Supervisor>>,
    project_id: String,
    name: String,
    cmd: String,
    autostart: bool,
    use_dynamic_port: bool,
    env: String,
) -> Result<Command, String> {
    // Kind is inferred from the command string (None = infer).
    sup.add_command(&project_id, name, cmd, None, autostart, use_dynamic_port, env)
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
    env: String,
) -> Result<Command, String> {
    sup.update_command(&project_id, &command_id, name, cmd, autostart, use_dynamic_port, env)
}

#[tauri::command]
pub fn remove_command(
    sup: State<Arc<Supervisor>>,
    project_id: String,
    command_id: String,
) -> Result<(), String> {
    sup.remove_command(&project_id, &command_id)
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
