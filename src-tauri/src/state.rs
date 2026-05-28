use std::sync::atomic::AtomicBool;

/// Process-global app state. `should_quit` distinguishes a real Quit (allow the
/// window to close and the app to exit) from a close-to-tray (hide only).
#[derive(Default)]
pub struct AppState {
    pub should_quit: AtomicBool,
}
