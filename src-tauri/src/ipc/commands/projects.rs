use crate::supervisor::Supervisor;
use crate::types::Project;
use std::sync::Arc;
use tauri::State;

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
