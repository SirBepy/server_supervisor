//! Group CRUD handlers for the localhost API.

use super::ApiState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
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

pub(super) async fn set_project_group_api(
    State(s): State<ApiState>,
    Path(project_id): Path<String>,
    Json(body): Json<SetGroupBody>,
) -> impl IntoResponse {
    match crate::groups::set_project_group(&s.data_dir, &project_id, body.group_id.as_deref()) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}
