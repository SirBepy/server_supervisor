//! Process lifecycle, logs, window info, and dock routes for the localhost API.

use super::{split_proc_id, unit_result, ApiState, DockRequest};
use crate::supervisor::dock;
use crate::supervisor::window;
use crate::supervisor::Supervisor;
use crate::types::{DockRect, DockState, ProcInfo};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

/// Per-proc window/dock info added to `/procs`. Deliberately not a field on
/// `ProcInfo` itself: that type also flows over the Tauri IPC `list_procs`
/// command, which the dashboard polls every tick for RAM/CPU stats, and
/// `find_window_once` below runs a real `EnumWindows` pass - fine for an
/// on-demand HTTP GET, not for the sampler's hot path.
#[derive(Serialize)]
pub(super) struct WindowInfo {
    found: bool,
    hwnd: Option<i64>,
    title: Option<String>,
}

#[derive(Serialize)]
pub(super) struct ProcWithWindow {
    #[serde(flatten)]
    info: ProcInfo,
    window: Option<WindowInfo>,
}

/// `None` when the proc has no live pid (stopped - nothing to probe).
/// Otherwise `dock_state_for` is the single source of truth for an active
/// dock: an embedded (`WS_CHILD`) window is invisible to `find_window_once`'s
/// `EnumWindows` scan (see `supervisor::dock`'s `note_window_lost` doc), so a
/// live probe would falsely report a docked window as not found. Only when
/// no dock has ever been attempted does this fall back to a live probe, to
/// tell a caller whether there is even a window worth docking.
fn window_info_for(sup: &Supervisor, id: &str, pid: Option<u32>) -> Option<WindowInfo> {
    let pid = pid?;
    if let Some(DockState::Docked { .. }) = sup.dock_state_for(id) {
        return Some(WindowInfo { found: true, hwnd: None, title: None });
    }
    match window::find_window_once(pid, false) {
        Some(w) => Some(WindowInfo { found: true, hwnd: Some(w.hwnd as i64), title: Some(w.title) }),
        None => Some(WindowInfo { found: false, hwnd: None, title: None }),
    }
}

pub(super) async fn list_procs(State(s): State<ApiState>) -> Json<Vec<ProcWithWindow>> {
    let out = s
        .sup
        .list()
        .into_iter()
        .map(|info| {
            let window = window_info_for(&s.sup, &info.id, info.pid);
            ProcWithWindow { info, window }
        })
        .collect();
    Json(out)
}

/// One top-level window of a proc's process tree, for `GET /procs/:id/windows`,
/// the route an agent uses to find a popup or dialog that `window::park`
/// moved off-screen (see todo 0056), since neither `/screenshot` nor
/// `/input`'s default target (the docked/main window) ever reaches one.
#[derive(Serialize)]
pub(super) struct ProcWindow {
    hwnd: i64,
    title: String,
    class: String,
    rect: DockRect,
    /// Whether this is the one window `dock_state_for` tracks as the proc's
    /// main docked/embedded window - the other windows in the list are
    /// popups, dialogs or menus `dock::headless` never reparents.
    docked: bool,
}

fn to_dock_rect(r: window::Rect) -> DockRect {
    DockRect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

/// `GET /procs/:id/windows` - every visible top-level window owned by the
/// proc's own pid tree (main window, docked or not, plus any popup/dialog),
/// so an agent can target one directly via `/screenshot?window=`/`/input?window=`.
pub(super) async fn list_windows(State(s): State<ApiState>, Path(id): Path<String>) -> Response {
    let pid = match s.sup.pid_for(&id) {
        Ok(pid) => pid,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let tree = window::descendant_pids(pid);
    let docked_hwnd = dock::active_dock_hwnd(&id);
    let windows: Vec<ProcWindow> = window::list_windows_of(&tree)
        .into_iter()
        .map(|w| ProcWindow {
            hwnd: w.hwnd as i64,
            title: w.title,
            class: w.class,
            rect: to_dock_rect(w.rect),
            docked: docked_hwnd == Some(w.hwnd),
        })
        .collect();
    Json(windows).into_response()
}

#[derive(Deserialize)]
pub(super) struct WindowQuery {
    window: Option<i64>,
}

/// Resolves the window `/screenshot` or `/input` should act on: `query`'s
/// `window` hwnd when given, else the proc's current docked/main window (the
/// pre-existing default behaviour). A caller-supplied hwnd is checked against
/// the proc's own pid tree first - this is the only thing stopping the API
/// from being pointed at an arbitrary window of an unrelated process.
pub(super) fn resolve_window(sup: &Supervisor, id: &str, query: &WindowQuery) -> Result<isize, String> {
    let Some(hwnd) = query.window else {
        return sup.window_for(id);
    };
    let pid = sup.pid_for(id)?;
    let tree = window::descendant_pids(pid);
    match window::hwnd_pid(hwnd as isize) {
        Some(owner) if tree.contains(&owner) => Ok(hwnd as isize),
        _ => Err(format!("window {hwnd} does not belong to process {id}")),
    }
}

/// Body for `POST /procs/:id/dock`. `headless: true` moves the window into
/// an invisible host of its own (and keeps it there across restarts);
/// otherwise `rect` present docks/reasserts into that screen-coordinate
/// rectangle (mirrors the Tauri IPC `dock_proc_window` body), and neither
/// undocks.
#[derive(Deserialize)]
pub(super) struct DockBody {
    #[serde(default)]
    rect: Option<DockRect>,
    #[serde(default)]
    headless: bool,
}

pub(super) async fn dock_proc(
    State(s): State<ApiState>,
    Path(id): Path<String>,
    Json(b): Json<DockBody>,
) -> Response {
    let Some(dock) = s.dock_fn.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "dock control is unavailable: no running Tauri app",
        )
            .into_response();
    };
    // `dock_fn` closes over the real `AppHandle` and calls straight into
    // `Supervisor::dock_window`/`undock_window`, which already marshal their
    // `SetParent`/`SetWindowPos` calls onto the main thread internally (see
    // `supervisor::dock`'s `on_main` helper) - this handler runs on an axum
    // worker thread, never the main thread, so no extra marshalling belongs
    // here. Calling either directly off-main would be exactly the deadlock
    // `supervisor::window`'s module docs warn about; `on_main` is what avoids
    // it. Both are idempotent: re-docking reasserts into the new rect, and
    // undocking an already-undocked proc is a no-op success.
    let req = match (b.headless, b.rect) {
        (true, _) => DockRequest::Headless,
        (false, Some(r)) => DockRequest::Pane(r),
        (false, None) => DockRequest::Undock,
    };
    match dock(&s.sup, &id, req) {
        Ok(Some(outcome)) => Json(outcome).into_response(),
        Ok(None) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

pub(super) async fn delete_proc(State(s): State<ApiState>, Path(id): Path<String>) -> Response {
    match split_proc_id(&id) {
        Some((project_id, command_id)) => {
            unit_result(s.sup.remove_command(project_id, command_id))
        }
        None => (StatusCode::BAD_REQUEST, format!("malformed process id: {id}")).into_response(),
    }
}

pub(super) async fn start_proc(State(s): State<ApiState>, Path(id): Path<String>) -> Response {
    unit_result(s.sup.start(&id))
}

pub(super) async fn stop_proc(State(s): State<ApiState>, Path(id): Path<String>) -> Response {
    unit_result(s.sup.stop(&id))
}

pub(super) async fn restart_proc(State(s): State<ApiState>, Path(id): Path<String>) -> Response {
    unit_result(s.sup.restart(&id))
}

pub(super) async fn reload_proc(State(s): State<ApiState>, Path(id): Path<String>) -> Response {
    // Try the flutter daemon hot restart; registry falls back to a full restart if the daemon is not ready.
    unit_result(s.sup.reload(&id, true))
}

pub(super) async fn get_logs(State(s): State<ApiState>, Path(id): Path<String>) -> Response {
    match s.sup.logs(&id) {
        Ok(lines) => Json(lines).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}
