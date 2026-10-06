use crate::supervisor::validate::CommandCheck;
use crate::supervisor::{detect, validate, Supervisor};
use crate::types::{Command, CommandParam, DetectedCommand, Role};
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
    // Optional so a caller that predates this field (an old frontend build)
    // can omit it entirely rather than erroring; absent = docking off.
    dock_window: Option<bool>,
    // Optional for the same reason; absent = no params (every command
    // predating this field has none).
    params: Option<Vec<CommandParam>>,
) -> Result<Command, String> {
    // Kind is inferred from the command string (None = infer).
    sup.add_command(
        &project_id,
        name,
        cmd,
        None,
        autostart,
        use_dynamic_port,
        fixed_port,
        env,
        role,
        dock_window.unwrap_or(false),
        params.unwrap_or_default(),
    )
    .map(|outcome| outcome.command)
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
    // Optional for the same reason as `add_command`'s.
    dock_window: Option<bool>,
    // `None` keeps the existing params (an old frontend build omitting this
    // key must never wipe them); `Some(vec![])` clears. See
    // `Supervisor::update_command`.
    params: Option<Vec<CommandParam>>,
) -> Result<Command, String> {
    sup.update_command(
        &project_id,
        &command_id,
        name,
        cmd,
        autostart,
        use_dynamic_port,
        fixed_port,
        env,
        role,
        dock_window.unwrap_or(false),
        params,
    )
}

#[tauri::command]
pub fn set_command_sound(
    sup: State<Arc<Supervisor>>,
    project_id: String,
    command_id: String,
    on: bool,
) -> Result<Command, String> {
    sup.set_command_sound(&project_id, &command_id, on)
}

#[tauri::command]
pub fn set_command_param(
    sup: State<Arc<Supervisor>>,
    project_id: String,
    command_id: String,
    name: String,
    value_id: String,
) -> Result<Command, String> {
    sup.set_command_param(&project_id, &command_id, &name, &value_id)
}

#[tauri::command]
pub fn set_command_headless(
    sup: State<Arc<Supervisor>>,
    project_id: String,
    command_id: String,
    on: bool,
) -> Result<Command, String> {
    sup.set_command_headless(&project_id, &command_id, on)
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
pub fn combine_commands(
    sup: State<Arc<Supervisor>>,
    project_id: String,
    source_ids: Vec<String>,
    name: String,
    template: String,
    param: CommandParam,
) -> Result<Command, String> {
    sup.combine_commands(&project_id, source_ids, name, template, param)
}

#[tauri::command]
pub fn detect_commands(path: String) -> Vec<DetectedCommand> {
    detect::detect(std::path::Path::new(&path))
}

#[tauri::command]
pub fn validate_command(root: String, cmd: String) -> CommandCheck {
    validate::validate_command(&root, &cmd)
}
