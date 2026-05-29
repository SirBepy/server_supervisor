pub mod config;
pub mod detect;
pub mod proc;
pub mod reaper;
pub mod registry;

pub use registry::Supervisor;

use tauri::{AppHandle, Manager};

/// Single cleanup entry point, called from the `ExitRequested` handler. Kills
/// every child the supervisor started so a real quit never leaves orphans.
pub fn shutdown_all(app: &AppHandle) {
    if let Some(sup) = app.try_state::<std::sync::Arc<Supervisor>>() {
        sup.shutdown_all();
    }
}
