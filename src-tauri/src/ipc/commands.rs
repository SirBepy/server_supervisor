use crate::settings::{self, Settings};
use crate::state::AppState;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Manager};

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
