pub mod config;
pub mod crud;
pub mod detect;
pub mod mem;
pub mod proc;
pub mod proxy;
pub mod reaper;
pub mod registry;
pub mod spawn_env;
pub mod validate;

pub use registry::Supervisor;

use tauri::{AppHandle, Manager};

/// Kill every child the supervisor started. Called from the `ExitRequested`
/// handler ONLY when an explicit "stop all & quit" was chosen (kill_on_exit);
/// a plain quit/update leaves children running to be re-adopted next launch.
pub fn shutdown_all(app: &AppHandle) {
    if let Some(sup) = app.try_state::<std::sync::Arc<Supervisor>>() {
        sup.shutdown_all();
    }
}

/// Stop all running children but keep the app running (tray "Close Processes").
pub fn stop_all(app: &AppHandle) {
    if let Some(sup) = app.try_state::<std::sync::Arc<Supervisor>>() {
        sup.stop_all();
    }
}
