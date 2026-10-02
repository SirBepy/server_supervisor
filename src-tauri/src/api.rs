//! Localhost HTTP control surface for programmatic (AI agent) access.
//!
//! Binds `127.0.0.1` only and requires `Authorization: Bearer <token>` on every
//! route except `/health`. The token is generated on first run and stored in a
//! file under the supervisor data dir. Because this endpoint can spawn arbitrary
//! commands, it must never bind to a non-loopback address. In particular `/run`
//! lets an authorized caller register-and-run an arbitrary command in one call
//! (define-and-run), so loopback-only binding plus the bearer token matter
//! doubly here.

mod commands;
mod groups;
mod media;
mod ports;
mod presets;
mod procs;

use crate::ports::PortRegistry;
use crate::supervisor::Supervisor;
use crate::types::{DockOutcome, DockRect};
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
    Router,
};
use commands::{add_command, remove_command, run, update_command};
use groups::{create_group_api, delete_group_api, list_groups_api, set_project_group_api, update_group_api};
use ports::{list_ports, reserve_port};
use presets::{activate_preset_api, add_preset_api, hub_port_api, list_presets_api, proxy_log_api, remove_preset_api};
use procs::{
    delete_proc, dock_proc, get_logs, list_procs, list_windows, reload_proc, restart_proc,
    start_proc, stop_proc,
};
use std::path::Path as FsPath;
use std::sync::Arc;
use tokio::net::TcpListener;

pub const TOKEN_FILE: &str = "api_token.txt";
const PORT_FILE: &str = "api_port.txt";
/// How many ports above the preferred one we probe before falling back to an
/// OS-assigned ephemeral port.
const PORT_PROBE_TRIES: u16 = 20;

/// Reads current AI permission flags: (ai_can_add_projects, ai_can_add_commands).
/// Stored as a trait object so `api.rs` stays free of Tauri types in its structs,
/// keeping integration tests runnable without Tauri DLL dependencies.
type PermissionFlags = Arc<dyn Fn() -> (bool, bool) + Send + Sync>;

/// Docks (`Some(rect)`) or undocks (`None`) `id`'s window, returning the dock
/// outcome on a dock or `None` on an undock. A trait object for the exact
/// reason `PermissionFlags` above is one: mentioning `tauri::AppHandle`
/// anywhere in this module's types, even just as a field, pulls Tauri's
/// windowing runtime (tao/wry/webview2-com) into anything that links this
/// module - including `tests/api_test.rs`, a plain console binary with no
/// WebView2 runtime alongside it. Proven, not guessed: with that field in
/// place the test binary failed to even start
/// (STATUS_ENTRYPOINT_NOT_FOUND) from a from-scratch build in an unshared
/// target dir, which rules out a stale-artifact explanation. Closing over
/// the real `AppHandle` inside `serve` and handing back only this closure
/// keeps the concrete Tauri type out of `api.rs` entirely.
type DockFn = Arc<dyn Fn(&Supervisor, &str, DockRequest) -> Result<Option<DockOutcome>, String> + Send + Sync>;

pub enum DockRequest {
    Pane(DockRect),
    Headless,
    Undock,
}

#[derive(Clone)]
struct ApiState {
    sup: Arc<Supervisor>,
    ports: Arc<PortRegistry>,
    token: String,
    /// None in tests (all AI operations allowed); Some in production (reads live settings).
    ai_flags: Option<PermissionFlags>,
    data_dir: std::path::PathBuf,
    /// None in the integration tests in `tests/api_test.rs`, which build the
    /// router directly with no running Tauri app (see `router`'s own doc) -
    /// Some in production, wired from `serve`. Docking needs this to marshal
    /// `SetParent`/`SetWindowPos` onto the main thread; every other route
    /// works without it.
    dock_fn: Option<DockFn>,
}

/// Read the bearer token from `<data_dir>/api_token.txt`, generating a fresh
/// random token on first run.
pub fn ensure_token(data_dir: &FsPath) -> String {
    let path = data_dir.join(TOKEN_FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let t = existing.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    let token = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::write(&path, &token);
    token
}

/// Returns a 403 response when an AI permission flag is disabled, None when allowed.
fn ai_forbidden(allowed: bool, action: &str) -> Option<Response> {
    if !allowed {
        Some((StatusCode::FORBIDDEN, format!("{action} is disabled in Settings")).into_response())
    } else {
        None
    }
}

/// Build the router. Exposed for tests so the API can be exercised without Tauri.
/// Pass `None` for `ai_flags` in tests; permission checks are skipped when `None`.
/// Pass `None` for `dock_fn` in tests too; every route works without one
/// except `/procs/:id/dock`, which needs it to marshal onto the main thread
/// (see `ApiState::dock_fn`).
pub fn router(
    sup: Arc<Supervisor>,
    ports: Arc<PortRegistry>,
    token: String,
    ai_flags: Option<PermissionFlags>,
    data_dir: std::path::PathBuf,
    dock_fn: Option<DockFn>,
) -> Router {
    let state = ApiState { sup, ports, token, ai_flags, data_dir, dock_fn };
    Router::new()
        .route("/procs", get(list_procs))
        .route("/procs/:id/start", post(start_proc))
        .route("/procs/:id/stop", post(stop_proc))
        .route("/procs/:id/restart", post(restart_proc))
        .route("/procs/:id/reload", post(reload_proc))
        .route("/procs/:id/logs", get(get_logs))
        .route("/procs/:id/dock", post(dock_proc))
        .route("/procs/:id/windows", get(list_windows))
        .route("/procs/:id/sound", post(media::set_sound))
        .route("/procs/:id/listen", get(media::listen))
        .route("/procs/:id/screenshot", get(media::screenshot))
        .route("/procs/:id/input", post(media::send_input))
        .route("/procs/:id", delete(delete_proc))
        .route("/ports", get(list_ports))
        .route("/ports/reserve", post(reserve_port))
        .route("/run", post(run))
        .route("/projects/:project_id/commands", post(add_command))
        .route(
            "/projects/:project_id/commands/:command_id",
            patch(update_command).delete(remove_command),
        )
        .route("/groups", get(list_groups_api).post(create_group_api))
        .route("/groups/:id", put(update_group_api).delete(delete_group_api))
        .route("/projects/:project_id/group", patch(set_project_group_api))
        .route(
            "/projects/:project_id/presets",
            get(list_presets_api).post(add_preset_api),
        )
        .route("/projects/:project_id/presets/:preset_id", delete(remove_preset_api))
        .route(
            "/projects/:project_id/presets/:preset_id/activate",
            post(activate_preset_api),
        )
        .route("/projects/:project_id/proxy-log", get(proxy_log_api))
        .route("/projects/:project_id/hub-port", get(hub_port_api))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth))
        // /health is added after the auth layer, so it stays unauthenticated.
        .route("/health", get(health))
        .with_state(state)
}

/// Bind `127.0.0.1` starting at `preferred`, probing upward through
/// `PORT_PROBE_TRIES` consecutive ports on collision, then falling back to an
/// OS-assigned ephemeral port (port 0) so we never give up. Returns the bound
/// listener.
async fn bind_probe(preferred: u16) -> std::io::Result<TcpListener> {
    for offset in 0..PORT_PROBE_TRIES {
        let candidate = preferred.saturating_add(offset);
        match TcpListener::bind(("127.0.0.1", candidate)).await {
            Ok(listener) => return Ok(listener),
            Err(e) => log::warn!("API port 127.0.0.1:{candidate} unavailable: {e}; probing next"),
        }
    }
    log::warn!("no port free in {preferred}..{}; binding OS-assigned ephemeral port", preferred.saturating_add(PORT_PROBE_TRIES));
    TcpListener::bind(("127.0.0.1", 0)).await
}

pub async fn serve(
    sup: Arc<Supervisor>,
    ports: Arc<PortRegistry>,
    port: u16,
    token: String,
    data_dir: std::path::PathBuf,
    app_handle: tauri::AppHandle,
) {
    let flags: PermissionFlags = {
        let app_handle = app_handle.clone();
        Arc::new(move || {
            let cfg = crate::settings::load(&app_handle);
            (cfg.ai_can_add_projects, cfg.ai_can_add_commands)
        })
    };
    // Closes over the real `AppHandle` here, in the one function that only
    // ever runs inside the live Tauri app - never called from
    // `tests/api_test.rs` - so the concrete type stays out of `api.rs`'s own
    // signatures (see `DockFn`'s doc for why that split is load-bearing).
    // Headless is persisted on the command, not just applied: the headless
    // tick reconciles every proc with that flag, so a one-off dock would be
    // undone within a second, and a pane dock or undock left with the flag on
    // redone.
    let dock_fn: DockFn = Arc::new(move |sup: &Supervisor, id: &str, req: DockRequest| match req {
        DockRequest::Pane(r) => {
            if let Some((project_id, command_id)) = split_proc_id(id) {
                let _ = sup.set_command_headless(project_id, command_id, false);
            }
            sup.dock_window(&app_handle, id, r).map(Some)
        }
        DockRequest::Headless => {
            let (project_id, command_id) =
                split_proc_id(id).ok_or_else(|| format!("malformed process id: {id}"))?;
            sup.set_command_headless(project_id, command_id, true)?;
            sup.dock_headless(&app_handle, id).map(Some)
        }
        DockRequest::Undock => {
            if let Some((project_id, command_id)) = split_proc_id(id) {
                let _ = sup.set_command_headless(project_id, command_id, false);
            }
            sup.undock_window(&app_handle, id).map(|()| None)
        }
    });
    let app = router(sup, ports, token, Some(flags), data_dir.clone(), Some(dock_fn));
    let listener = match bind_probe(port).await {
        Ok(l) => l,
        Err(e) => {
            log::error!("supervisor API failed to bind any 127.0.0.1 port: {e}");
            return;
        }
    };
    // Read the ACTUAL bound port (may differ from `port` after probing) and
    // publish it to a discovery file, mirroring how the bearer token is written.
    let bound = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(e) => {
            log::error!("supervisor API could not read local_addr: {e}");
            return;
        }
    };
    let _ = std::fs::write(data_dir.join(PORT_FILE), bound.to_string());
    log::info!("supervisor API listening on http://127.0.0.1:{bound}");
    if let Err(e) = axum::serve(listener, app).await {
        log::error!("supervisor API server error: {e}");
    }
}

async fn auth(
    State(state): State<ApiState>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let ok = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t == state.token)
        .unwrap_or(false);
    if !ok {
        return (StatusCode::UNAUTHORIZED, "invalid or missing bearer token").into_response();
    }
    next.run(req).await
}

async fn health() -> &'static str {
    "ok"
}

/// Split a composite proc id (`project:command`) into its parts. `slug` never
/// emits `:`, so the first `:` is the project/command boundary. None if absent.
fn split_proc_id(id: &str) -> Option<(&str, &str)> {
    id.split_once(':')
}

fn unit_result(r: Result<(), String>) -> Response {
    match r {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_probe_skips_occupied_port() {
        // Occupy a real port by letting the OS pick a free one for us.
        let occupied = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let occupied_port = occupied.local_addr().unwrap().port();

        // Probing with the occupied port as preferred must land elsewhere.
        let probed = bind_probe(occupied_port).await.unwrap();
        let probed_port = probed.local_addr().unwrap().port();

        assert_ne!(
            probed_port, occupied_port,
            "bind_probe must probe past an occupied preferred port"
        );
    }

    #[test]
    fn split_proc_id_splits_on_first_colon() {
        assert_eq!(super::split_proc_id("proj:cmd"), Some(("proj", "cmd")));
        // Command slugs never contain ':', but be explicit: split on the FIRST.
        assert_eq!(
            super::split_proc_id("proj:cmd:weird"),
            Some(("proj", "cmd:weird"))
        );
        assert_eq!(super::split_proc_id("nocolon"), None);
    }
}
