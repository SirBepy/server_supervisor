//! Routes that let an agent test a supervised GUI app without the dev
//! seeing or hearing it: sound on/off, listen, screenshot, and posted input.

use super::procs::{resolve_window, WindowQuery};
use super::{split_proc_id, unit_result, ApiState};
use crate::supervisor::audio;
use crate::supervisor::window::{capture, input};
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_LISTEN_MS: u64 = 2_000;
const MAX_LISTEN_MS: u64 = 60_000;
/// Gap between posted actions so the app drains one before the next lands;
/// back-to-back posts can coalesce (two clicks read as a double click).
const ACTION_GAP: Duration = Duration::from_millis(30);

fn bad_request(e: String) -> Response {
    (StatusCode::BAD_REQUEST, e).into_response()
}

#[derive(Deserialize)]
pub(super) struct SoundBody {
    on: bool,
}

/// `POST /procs/:id/sound` - lets this command's audio reach the dev's
/// speakers (`on: true`) or mutes it again. Persists on the command and
/// applies live, without a restart.
pub(super) async fn set_sound(
    State(s): State<ApiState>,
    Path(id): Path<String>,
    Json(b): Json<SoundBody>,
) -> Response {
    let Some((project_id, command_id)) = split_proc_id(&id) else {
        return bad_request(format!("malformed process id: {id}"));
    };
    match s.sup.set_command_sound(project_id, command_id, b.on) {
        Ok(cmd) => Json(cmd).into_response(),
        Err(e) => bad_request(e),
    }
}

#[derive(Deserialize)]
pub(super) struct ListenQuery {
    ms: Option<u64>,
}

/// `GET /procs/:id/listen?ms=N` - samples the app's audio levels for N ms
/// (default 2000, max 60000). Works while the app is muted for the dev.
pub(super) async fn listen(
    State(s): State<ApiState>,
    Path(id): Path<String>,
    Query(q): Query<ListenQuery>,
) -> Response {
    let pid = match s.sup.pid_for(&id) {
        Ok(pid) => pid,
        Err(e) => return bad_request(e),
    };
    let ms = q.ms.unwrap_or(DEFAULT_LISTEN_MS).min(MAX_LISTEN_MS);
    match tokio::task::spawn_blocking(move || audio::listen(pid, Duration::from_millis(ms))).await {
        Ok(Ok(heard)) => Json(heard).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("listen task failed: {e}")).into_response(),
    }
}

/// `GET /procs/:id/screenshot?window=N` - PNG of the app's window, docked or
/// not, including when it sits in the invisible headless host. `window` (an
/// hwnd from `GET /procs/:id/windows`) targets a specific popup or dialog
/// instead of the proc's main window; omitted, this is the pre-existing
/// default-window behaviour. A caller-supplied hwnd outside the proc's own
/// pid tree is rejected, never captured.
pub(super) async fn screenshot(
    State(s): State<ApiState>,
    Path(id): Path<String>,
    Query(q): Query<WindowQuery>,
) -> Response {
    let hwnd = match resolve_window(&s.sup, &id, &q) {
        Ok(h) => h,
        Err(e) => return bad_request(e),
    };
    let png = tokio::task::spawn_blocking(move || capture::capture(hwnd).and_then(|c| c.to_png())).await;
    match png {
        Ok(Ok(bytes)) => ([(header::CONTENT_TYPE, "image/png")], bytes).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("capture task failed: {e}")).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum InputBody {
    Many { actions: Vec<input::InputAction> },
    One(input::InputAction),
}

/// `POST /procs/:id/input?window=N` - one action (`{"type":"click","x":10,"y":20}`)
/// or a sequence (`{"actions":[...]}`), in the window's client pixels.
/// `window` targets a specific popup or dialog the same way `/screenshot`'s
/// does; omitted, this is the pre-existing default-window behaviour.
pub(super) async fn send_input(
    State(s): State<ApiState>,
    Path(id): Path<String>,
    Query(q): Query<WindowQuery>,
    Json(b): Json<InputBody>,
) -> Response {
    let hwnd = match resolve_window(&s.sup, &id, &q) {
        Ok(h) => h,
        Err(e) => return bad_request(e),
    };
    let actions = match b {
        InputBody::Many { actions } => actions,
        InputBody::One(a) => vec![a],
    };
    for (i, action) in actions.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(ACTION_GAP).await;
        }
        if let Err(e) = input::send(hwnd, action) {
            return bad_request(format!("action {i}: {e}"));
        }
    }
    unit_result(Ok(()))
}
