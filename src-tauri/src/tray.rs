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
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogResult};

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

/// Confirm a quit via a real 3-way dialog. `rfd` reports Cancel, the window's
/// own X, and Escape all as `MessageDialogResult::Cancel` (distinct from
/// either `Custom` label) - the one branch below that must abort the quit.
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
        const STOP_AND_QUIT: &str = "Stop all & quit";
        const LEAVE_AND_QUIT: &str = "Leave running & quit";
        let result = handle
            .dialog()
            .message(format!(
                "{n} process(es) are running.\n\nStop them before quitting, or leave them running (re-adopted next launch)?"
            ))
            .title("Quit Server Supervisor")
            .buttons(MessageDialogButtons::YesNoCancelCustom(
                STOP_AND_QUIT.into(),
                LEAVE_AND_QUIT.into(),
                "Cancel".into(),
            ))
            .blocking_show_with_result();
        let kill_on_exit = match result {
            MessageDialogResult::Custom(s) if s == STOP_AND_QUIT => true,
            MessageDialogResult::Custom(s) if s == LEAVE_AND_QUIT => false,
            _ => return,
        };
        if let Some(s) = handle.try_state::<AppState>() {
            s.kill_on_exit.store(kill_on_exit, Ordering::SeqCst);
            s.should_quit.store(true, Ordering::SeqCst);
        }
        handle.exit(0);
    });
}
