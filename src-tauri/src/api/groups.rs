//! Group CRUD handlers for the localhost API.

use super::ApiState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

pub(super) async fn list_groups_api(State(s): State<ApiState>) -> impl IntoResponse {
    Json(crate::groups::load(&s.data_dir))
}

#[derive(Deserialize)]
pub(super) struct GroupNameBody {
    name: String,
}

pub(super) async fn create_group_api(
    State(s): State<ApiState>,
    Json(body): Json<GroupNameBody>,
) -> impl IntoResponse {
    match crate::groups::create(&s.data_dir, body.name) {
        Ok(g) => (StatusCode::CREATED, Json(g)).into_response(),
        Err(e) => (StatusCode::CONFLICT, e).into_response(),
    }
}

pub(super) async fn update_group_api(
    State(s): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<GroupNameBody>,
) -> impl IntoResponse {
    match crate::groups::update(&s.data_dir, &id, body.name) {
        Ok(g) => Json(g).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}

pub(super) async fn delete_group_api(
    State(s): State<ApiState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match crate::groups::delete(&s.data_dir, &id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct SetGroupBody {
    group_id: Option<String>,
}

/// Validates `project_id` first (matching every `:project_id` handler in
/// `presets.rs`) before delegating: an unknown project now 404s the same way
/// an unknown preset project does, instead of `groups::set_project_group`
/// silently no-op-204ing on a project that was never registered.
pub(super) async fn set_project_group_api(
    State(s): State<ApiState>,
    Path(project_id): Path<String>,
    Json(body): Json<SetGroupBody>,
) -> Response {
    if let Err(r) = super::presets::find_project(&s, &project_id) {
        return r;
    }
    match crate::groups::set_project_group(&s.data_dir, &project_id, body.group_id.as_deref()) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}
