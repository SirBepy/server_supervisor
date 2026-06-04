use crate::state::AppState;
use crate::supervisor::Supervisor;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{
    image::Image,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let menu = MenuBuilder::new(app)
        .item(&MenuItemBuilder::with_id("open", "Open").build(app)?)
        .item(&MenuItemBuilder::with_id("close_procs", "Close Processes").build(app)?)
        .separator()
        .item(&MenuItemBuilder::with_id("quit", "Quit").build(app)?)
        .build()?;

    let icon = match app.default_window_icon() {
        Some(i) => i.clone(),
        None => Image::from_bytes(include_bytes!("../icons/32x32.png"))?,
    };

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .tooltip("Server Supervisor")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "close_procs" => crate::supervisor::stop_all(app),
            "quit" => request_quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                // Left-click only ever shows / raises the window - never hides it.
                show_main(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Count running processes via the supervisor list.
fn running_count(app: &AppHandle) -> usize {
    app.try_state::<Arc<Supervisor>>()
        .map(|sup| sup.list().iter().filter(|p| p.pid.is_some()).count())
        .unwrap_or(0)
}

/// Confirm a quit. If processes are running, ask whether to stop them. Two
/// outcomes from the native dialog (it returns a bool, so no separate Cancel;
/// the user re-opens the app if they did not mean to quit - servers are still
/// alive. A true 3-way Cancel would need a custom webview modal; deferred):
///   - first button "Stop all & quit"  -> kill_on_exit = true
///   - second button "Leave running & quit" / dismiss -> kill_on_exit stays false
///
/// `blocking_show()` MUST NOT run on the main thread (the crate docstring and the
/// plugin's own example wrap it in `std::thread::spawn` for exactly this reason).
/// `request_quit` is called from the tray menu event and the `quit_app` IPC, both
/// on the UI thread - so it spawns a worker that shows the dialog, sets the flags,
/// and calls `exit(0)`, and returns immediately. The native MessageDialog is a
/// top-level OS modal: it shows correctly even with the app in the tray and no
/// visible window (the normal quit-from-tray case).
pub fn request_quit(app: &AppHandle) {
    let n = running_count(app);
    if n == 0 {
        if let Some(s) = app.try_state::<AppState>() {
            s.should_quit.store(true, Ordering::SeqCst);
        }
        app.exit(0);
        return;
    }
    let handle = app.clone();
    std::thread::spawn(move || {
        let stop = handle
            .dialog()
            .message(format!(
                "{n} process(es) are running.\n\nStop them before quitting, or leave them running (re-adopted next launch)?"
            ))
            .title("Quit Server Supervisor")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Stop all & quit".into(),
                "Leave running & quit".into(),
            ))
            .blocking_show();
        if let Some(s) = handle.try_state::<AppState>() {
            s.kill_on_exit.store(stop, Ordering::SeqCst);
            s.should_quit.store(true, Ordering::SeqCst);
        }
        handle.exit(0);
    });
}
