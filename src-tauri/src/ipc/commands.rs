use crate::settings::{self, Settings};
use crate::state::AppState;
use crate::supervisor::Supervisor;
use crate::types::{LogLine, ProcInfo};
use std::sync::atomic::Ordering;
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
pub fn list_procs(sup: State<Supervisor>) -> Vec<ProcInfo> {
    sup.list()
}

#[tauri::command]
pub fn start_proc(sup: State<Supervisor>, id: String) -> Result<(), String> {
    sup.start(&id)
}

#[tauri::command]
pub fn stop_proc(sup: State<Supervisor>, id: String) -> Result<(), String> {
    sup.stop(&id)
}

#[tauri::command]
pub fn restart_proc(sup: State<Supervisor>, id: String) -> Result<(), String> {
    sup.restart(&id)
}

#[tauri::command]
pub fn get_proc_logs(sup: State<Supervisor>, id: String) -> Result<Vec<LogLine>, String> {
    sup.logs(&id)
}
