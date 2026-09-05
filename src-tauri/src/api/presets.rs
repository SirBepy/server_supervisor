//! Preset and reverse-proxy hub handlers for the localhost API.

use super::{unit_result, ApiState};
use crate::supervisor::proxy_hub::RequestLogEntry;
use crate::types::Project;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

/// Look up a project by id, or the shared 404 response every `:project_id`
/// handler below needs when it is absent. `pub(super)` (not private): reused
/// by `groups::set_project_group_api` so the group and preset APIs agree on
/// what an unknown `project_id` does.
pub(super) fn find_project(state: &ApiState, project_id: &str) -> Result<Project, Response> {
    state
        .sup
        .list_projects()
        .into_iter()
        .find(|p| p.id == project_id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("unknown project: {project_id}")).into_response())
}

pub(super) async fn list_presets_api(State(s): State<ApiState>, Path(project_id): Path<String>) -> Response {
    match find_project(&s, &project_id) {
        Ok(p) => Json(p.presets).into_response(),
        Err(r) => r,
    }
}

/// Body for `POST /projects/:project_id/presets`.
#[derive(Deserialize)]
pub(super) struct AddPresetBody {
    name: String,
    base_url: String,
    #[serde(default)]
    danger: bool,
}

pub(super) async fn add_preset_api(
    State(s): State<ApiState>,
    Path(project_id): Path<String>,
    Json(b): Json<AddPresetBody>,
) -> Response {
    match s.sup.add_preset(&project_id, b.name, b.base_url, b.danger) {
        Ok(preset) => Json(preset).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

pub(super) async fn remove_preset_api(
    State(s): State<ApiState>,
    Path((project_id, preset_id)): Path<(String, String)>,
) -> Response {
    unit_result(s.sup.remove_preset(&project_id, &preset_id))
}

pub(super) async fn activate_preset_api(
    State(s): State<ApiState>,
    Path((project_id, preset_id)): Path<(String, String)>,
) -> Response {
    unit_result(s.sup.set_active_preset(&project_id, &preset_id))
}

pub(super) async fn proxy_log_api(
    State(s): State<ApiState>,
    Path(project_id): Path<String>,
) -> Json<Vec<RequestLogEntry>> {
    Json(s.sup.hub_log(&project_id))
}

pub(super) async fn hub_port_api(State(s): State<ApiState>, Path(project_id): Path<String>) -> Response {
    let p = match find_project(&s, &project_id) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if p.presets.is_empty() {
        return (StatusCode::NOT_FOUND, format!("no hub configured for project: {project_id}")).into_response();
    }
    match s.sup.hub_port(&project_id) {
        Ok(port) => Json(port).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}
