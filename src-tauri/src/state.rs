use std::sync::atomic::AtomicBool;

/// Process-global app state.
/// - `should_quit` distinguishes a real Quit (let the window close, app exits)
///   from a close-to-tray (hide only).
/// - `kill_on_exit` decides what a real exit does to owned processes: `false`
///   (default) leaves them running to be re-adopted next launch; `true` kills
///   them first. Set true only by an explicit "stop all & quit" choice.
#[derive(Default)]
pub struct AppState {
    pub should_quit: AtomicBool,
    pub kill_on_exit: AtomicBool,
}
