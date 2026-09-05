use crate::supervisor::validate::CommandCheck;
use crate::supervisor::{detect, validate, Supervisor};
use crate::types::{Command, DetectedCommand, Role};
use std::sync::Arc;
use tauri::State;

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

#[tauri::command]
pub fn detect_commands(path: String) -> Vec<DetectedCommand> {
    detect::detect(std::path::Path::new(&path))
}

#[tauri::command]
pub fn validate_command(root: String, cmd: String) -> CommandCheck {
    validate::validate_command(&root, &cmd)
}
