use crate::groups::Group;
use tauri::{AppHandle, Manager};

fn data_dir(app: &AppHandle) -> std::path::PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("supervisor")
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
