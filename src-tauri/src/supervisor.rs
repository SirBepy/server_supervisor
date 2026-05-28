use tauri::AppHandle;

/// Single cleanup entry point: kill every child process the supervisor started.
///
/// Phase 2 wires the real process registry plus Windows job-object reaping so
/// the "no orphans, ever" guarantee holds even on crash. For now this is the
/// one place app-exit cleanup lives, called from the `ExitRequested` handler.
pub fn shutdown_all(_app: &AppHandle) {
    log::info!("supervisor: shutdown_all (no managed processes yet)");
}
