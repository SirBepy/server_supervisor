//! Port registry routes for the localhost API.

use super::ApiState;
use crate::ports::PortEntry;
use axum::{extract::State, Json};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct ReserveBody {
    owner: String,
}

pub(super) async fn list_ports(State(s): State<ApiState>) -> Json<Vec<PortEntry>> {
    Json(s.ports.list())
}

pub(super) async fn reserve_port(State(s): State<ApiState>, Json(body): Json<ReserveBody>) -> Json<u16> {
    Json(s.ports.reserve_next(&body.owner))
}
