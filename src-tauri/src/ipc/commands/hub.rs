use crate::supervisor::proxy_hub::RequestLogEntry;
use crate::supervisor::Supervisor;
use crate::types::UpstreamPreset;
use std::sync::Arc;
use tauri::State;

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
