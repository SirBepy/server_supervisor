pub mod api;
pub mod ipc;
pub mod ports;
pub mod settings;
pub mod state;
pub mod supervisor;
pub mod tray;
pub mod types;

use state::AppState;
use std::sync::atomic::Ordering;
use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_kit_settings::with_logging())
        .plugin(tauri_kit_settings::with_kit_commands())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            ipc::commands::quit_app,
            ipc::commands::get_settings,
            ipc::commands::save_settings,
            ipc::commands::list_procs,
            ipc::commands::start_proc,
            ipc::commands::stop_proc,
            ipc::commands::restart_proc,
            ipc::commands::reload_proc,
            ipc::commands::get_proc_logs,
            ipc::commands::list_projects,
            ipc::commands::add_project,
            ipc::commands::remove_project,
            ipc::commands::add_command,
            ipc::commands::remove_command,
            ipc::commands::detect_commands,
            ipc::commands::list_ports,
            ipc::commands::reserve_port,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            log::info!(
                "server_supervisor starting; version={}",
                env!("CARGO_PKG_VERSION")
            );

            // Single owner of all dev-server processes. Reconcile any orphans left
            // by a prior crash, then auto-start the processes flagged autostart.
            let data_dir = handle
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join("supervisor");
            let _ = std::fs::create_dir_all(&data_dir);
            let ports = std::sync::Arc::new(ports::PortRegistry::new(data_dir.clone()));
            let supervisor =
                std::sync::Arc::new(supervisor::Supervisor::new(data_dir.clone(), ports.clone()));
            supervisor.reconcile_orphans();
            supervisor.start_autostart();

            // Localhost API for programmatic (AI agent) control.
            let port = settings::load(&handle).api_port;
            let token = api::ensure_token(&data_dir);
            let api_sup = supervisor.clone();
            let api_ports = ports.clone();
            let api_data_dir = data_dir.clone();
            tauri::async_runtime::spawn(async move {
                api::serve(api_sup, api_ports, port, token, api_data_dir).await;
            });

            handle.manage(supervisor);
            handle.manage(ports);

            tray::setup(&handle)?;

            // Close-to-tray: hide instead of quitting, unless an explicit Quit set should_quit.
            if let Some(window) = handle.get_webview_window("main") {
                let w = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        let quitting = w
                            .app_handle()
                            .try_state::<AppState>()
                            .map(|s| s.should_quit.load(Ordering::SeqCst))
                            .unwrap_or(false);
                        if quitting {
                            return;
                        }
                        api.prevent_close();
                        let _ = w.hide();
                    }
                });
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Single-owner guarantee: on real exit, kill every child we started.
            if let tauri::RunEvent::ExitRequested { .. } = event {
                supervisor::shutdown_all(app);
            }
        });
}
