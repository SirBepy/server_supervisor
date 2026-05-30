//! Localhost HTTP control surface for programmatic (AI agent) access.
//!
//! Binds `127.0.0.1` only and requires `Authorization: Bearer <token>` on every
//! route except `/health`. The token is generated on first run and stored in a
//! file under the supervisor data dir. Because this endpoint can spawn arbitrary
//! commands, it must never bind to a non-loopback address. In particular `/run`
//! lets an authorized caller register-and-run an arbitrary command in one call
//! (define-and-run), so loopback-only binding plus the bearer token matter
//! doubly here.

use crate::ports::{PortEntry, PortRegistry};
use crate::supervisor::Supervisor;
use crate::types::{ProcInfo, ProcKind};
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

pub const TOKEN_FILE: &str = "api_token.txt";
const PORT_FILE: &str = "api_port.txt";
/// How many ports above the preferred one we probe before falling back to an
/// OS-assigned ephemeral port.
const PORT_PROBE_TRIES: u16 = 20;

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

#[derive(Deserialize)]
struct RunBody {
    root: String,
    cmd: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<ProcKind>,
    #[serde(default)]
    use_dynamic_port: Option<bool>,
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
        .route("/run", post(run))
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
) {
    let app = router(sup, ports, token);
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

async fn list_procs(State(s): State<ApiState>) -> Json<Vec<ProcInfo>> {
    Json(s.sup.list())
}

async fn list_ports(State(s): State<ApiState>) -> Json<Vec<PortEntry>> {
    Json(s.ports.list())
}

async fn reserve_port(State(s): State<ApiState>, Json(body): Json<ReserveBody>) -> Json<u16> {
    Json(s.ports.reserve_next(&body.owner))
}

async fn run(State(s): State<ApiState>, Json(b): Json<RunBody>) -> Response {
    match s.sup.ensure_and_run(
        &b.root,
        &b.cmd,
        b.name,
        // Omitted kind -> inferred from the command; an explicit kind overrides.
        b.kind,
        b.use_dynamic_port.unwrap_or(true),
    ) {
        Ok(info) => Json(info).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
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
}
