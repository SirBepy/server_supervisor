//! Localhost HTTP control surface for programmatic (AI agent) access.
//!
//! Binds `127.0.0.1` only and requires `Authorization: Bearer <token>` on every
//! route except `/health`. The token is generated on first run and stored in a
//! file under the supervisor data dir. Because this endpoint can spawn arbitrary
//! commands, it must never bind to a non-loopback address.

use crate::ports::{PortEntry, PortRegistry};
use crate::supervisor::Supervisor;
use crate::types::ProcInfo;
use axum::{
    extract::{Path, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::path::Path as FsPath;
use std::sync::Arc;
use tokio::net::TcpListener;

const TOKEN_FILE: &str = "api_token.txt";

#[derive(Clone)]
struct ApiState {
    sup: Arc<Supervisor>,
    ports: Arc<PortRegistry>,
    token: String,
}

#[derive(Deserialize)]
struct ReserveBody {
    owner: String,
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

/// Build the router. Exposed for tests so the API can be exercised without Tauri.
pub fn router(sup: Arc<Supervisor>, ports: Arc<PortRegistry>, token: String) -> Router {
    let state = ApiState { sup, ports, token };
    Router::new()
        .route("/procs", get(list_procs))
        .route("/procs/:id/start", post(start_proc))
        .route("/procs/:id/stop", post(stop_proc))
        .route("/procs/:id/restart", post(restart_proc))
        .route("/procs/:id/reload", post(reload_proc))
        .route("/procs/:id/logs", get(get_logs))
        .route("/ports", get(list_ports))
        .route("/ports/reserve", post(reserve_port))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth))
        // /health is added after the auth layer, so it stays unauthenticated.
        .route("/health", get(health))
        .with_state(state)
}

pub async fn serve(sup: Arc<Supervisor>, ports: Arc<PortRegistry>, port: u16, token: String) {
    let app = router(sup, ports, token);
    match TcpListener::bind(("127.0.0.1", port)).await {
        Ok(listener) => {
            log::info!("supervisor API listening on http://127.0.0.1:{port}");
            if let Err(e) = axum::serve(listener, app).await {
                log::error!("supervisor API server error: {e}");
            }
        }
        Err(e) => log::error!("supervisor API failed to bind 127.0.0.1:{port}: {e}"),
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

async fn list_procs(State(s): State<ApiState>) -> Json<Vec<ProcInfo>> {
    Json(s.sup.list())
}

async fn list_ports(State(s): State<ApiState>) -> Json<Vec<PortEntry>> {
    Json(s.ports.list())
}

async fn reserve_port(State(s): State<ApiState>, Json(body): Json<ReserveBody>) -> Json<u16> {
    Json(s.ports.reserve_next(&body.owner))
}

fn unit_result(r: Result<(), String>) -> Response {
    match r {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn start_proc(State(s): State<ApiState>, Path(id): Path<String>) -> Response {
    unit_result(s.sup.start(&id))
}

async fn stop_proc(State(s): State<ApiState>, Path(id): Path<String>) -> Response {
    unit_result(s.sup.stop(&id))
}

async fn restart_proc(State(s): State<ApiState>, Path(id): Path<String>) -> Response {
    unit_result(s.sup.restart(&id))
}

async fn reload_proc(State(s): State<ApiState>, Path(id): Path<String>) -> Response {
    // Web hot reload is upstream-broken, so we always fullRestart.
    unit_result(s.sup.reload(&id, true))
}

async fn get_logs(State(s): State<ApiState>, Path(id): Path<String>) -> Response {
    match s.sup.logs(&id) {
        Ok(lines) => Json(lines).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}
