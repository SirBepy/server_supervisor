//! In-supervisor multi-project reverse-proxy hub: one loopback listener per
//! project on a FIXED port (the project's port-block top slot, `base+9` - see
//! `ports::PortRegistry::project_hub_port`), so a dev app can bake that
//! address in at compile time and never need a rebuild to point at a
//! different backend. Swapping which upstream a project's hub forwards to is
//! a live update to shared state the running listener reads per request -
//! the listener itself is NEVER torn down and rebound, which is the whole
//! point: the app's connection target must never change.
//!
//! This is a SEPARATE module from `proxy.rs`, which is a per-process,
//! Flutter-web-only reverse proxy with live-reload injection fronting one dev
//! server. This hub fronts however many named upstream presets a project
//! declares (e.g. local / develop / prod), swappable without moving the
//! app's own port. The two do not share runtime state; `proxy.rs`'s
//! `HOP_BY_HOP`/`is_hop_by_hop` helpers and `ensure_crypto_provider` are
//! reused here rather than duplicated.
//!
//! ## CORS
//! The dashboard/app runs on one localhost port and calls the hub on
//! another, so every request is cross-origin. The hub reflects permissive
//! CORS headers (allow-origin echoed, credentials allowed) for any `Origin`
//! that resolves to a loopback host. This is acceptable ONLY because the
//! listener itself is bound to `127.0.0.1` - nothing off-box can ever reach
//! it to exploit the permissive policy.
//!
//! ## Scope: unauthenticated, read-only traffic only
//! `Set-Cookie` responses have their `Domain` attribute stripped so a cookie
//! set by the upstream is not silently dropped by the browser for not
//! matching the hub's own `127.0.0.1` origin. That is the FULL extent of the
//! auth story here - this does NOT emulate a shared auth realm across
//! environments. Swapping the active preset mid-session on an app that is
//! logged in is expected to require a re-login against the newly active
//! upstream.
//!
//! ## Websockets
//! Every request is fully buffered (method, headers, body) and re-issued via
//! `reqwest`, the same approach `proxy.rs` takes - there is no raw
//! byte-level passthrough. A request carrying a `Connection: Upgrade`
//! (WebSocket, HTTP/2 h2c, ...) cannot go through that path, so it is
//! rejected with a logged `501` rather than silently mangled into a broken
//! plain HTTP response.

use super::config;
use super::crud;
use super::proc::now_ms;
use super::proxy::{
    buffer_body, copy_response_headers, ensure_crypto_provider, path_and_query,
    spawn_loopback_server, to_reqwest_method, to_upstream_headers,
};
use super::registry::Supervisor;
use crate::types::{Project, UpstreamPreset};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tokio::sync::oneshot;
use ts_rs::TS;

/// Cap on the in-memory per-project request-log ring buffer.
const REQUEST_LOG_CAP: usize = 500;

/// One captured request/response pair, sans bodies (never logged - see the
/// module doc comment). Exposed to the dashboard/AI-agent API for live
/// traffic visibility - this is the part of the hub with no cheaper
/// substitute, so it is a real feature, not a debug side effect.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct RequestLogEntry {
    pub ts: u64,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub duration_ms: u64,
    /// Name of the preset that served this request (the active one at the
    /// time), so a log spanning a preset swap stays legible.
    pub preset: String,
}

/// The upstream a hub's listener currently forwards to. Cheap to clone;
/// swapped wholesale under a `RwLock` on every preset change - no rebind.
#[derive(Clone)]
struct ActiveUpstream {
    preset_name: String,
    /// Trimmed of any trailing `/`.
    base_url: String,
}

impl ActiveUpstream {
    fn from_preset(p: &UpstreamPreset) -> Self {
        Self {
            preset_name: p.name.clone(),
            base_url: p.base_url.trim_end_matches('/').to_string(),
        }
    }
}

struct HubShared {
    active: RwLock<ActiveUpstream>,
    log: Mutex<VecDeque<RequestLogEntry>>,
    client: reqwest::Client,
}

impl HubShared {
    fn push_log(&self, entry: RequestLogEntry) {
        let mut log = self.log.lock().unwrap();
        log.push_back(entry);
        while log.len() > REQUEST_LOG_CAP {
            log.pop_front();
        }
    }
}

#[derive(Clone)]
struct HubState {
    inner: Arc<HubShared>,
}

/// A running project proxy-hub listener. Dropping it (or calling `stop`)
/// signals graceful shutdown and joins the listener thread - mirrors
/// `proxy::ProxyTask`.
pub struct ProxyHub {
    shutdown: Option<oneshot::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
    shared: Arc<HubShared>,
}

impl ProxyHub {
    /// Swap the active upstream. Takes effect on the very next request the
    /// already-running listener handles - no rebind.
    fn set_active(&self, preset: &UpstreamPreset) {
        *self.shared.active.write().unwrap() = ActiveUpstream::from_preset(preset);
    }

    /// Snapshot of the ring buffer, oldest first.
    fn log_snapshot(&self) -> Vec<RequestLogEntry> {
        self.shared.log.lock().unwrap().iter().cloned().collect()
    }

    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for ProxyHub {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The ONLY place a hub's bind address is ever constructed: always the IPv4
/// loopback. `spawn` takes just a `port`, never a caller-suppliable address,
/// so there is no code path that can make a hub listener bind externally -
/// per the project's hard rule that nothing the supervisor exposes may.
fn loopback_addr(port: u16) -> std::net::SocketAddr {
    std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port))
}

/// Spawn a project's hub listener on `127.0.0.1:port` ONLY - hardcoded, never
/// caller-controllable, per the project's hard rule that nothing the
/// supervisor exposes may bind externally. Returns once bound so a bind error
/// surfaces synchronously.
pub fn spawn(port: u16, initial: &UpstreamPreset) -> std::io::Result<ProxyHub> {
    ensure_crypto_provider();
    let shared = Arc::new(HubShared {
        active: RwLock::new(ActiveUpstream::from_preset(initial)),
        log: Mutex::new(VecDeque::with_capacity(REQUEST_LOG_CAP)),
        client: reqwest::Client::new(),
    });
    let state = HubState { inner: shared.clone() };
    let app = Router::new().fallback(hub_handler).with_state(state);

    let (shutdown_tx, handle) = spawn_loopback_server(loopback_addr(port), app, "proxy_hub")?;
    Ok(ProxyHub {
        shutdown: Some(shutdown_tx),
        handle: Some(handle),
        shared,
    })
}

/// True if `origin` (a full `scheme://host[:port]` value from the `Origin`
/// header) resolves to a loopback host. Gates the permissive CORS
/// reflection - see the module doc comment for why that's safe here.
fn is_loopback_origin(origin: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    matches!(url.host_str(), Some("localhost") | Some("127.0.0.1") | Some("::1"))
}

/// CORS response headers for `req_headers`, if its `Origin` is a loopback
/// origin. Empty otherwise (browsers then enforce same-origin as normal).
fn cors_headers(req_headers: &HeaderMap) -> Vec<(HeaderName, HeaderValue)> {
    let mut out = Vec::new();
    let Some(origin) = req_headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return out;
    };
    if !is_loopback_origin(origin) {
        return out;
    }
    if let Ok(v) = HeaderValue::from_str(origin) {
        out.push((header::ACCESS_CONTROL_ALLOW_ORIGIN, v));
    }
    out.push((
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    ));
    out.push((
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, OPTIONS"),
    ));
    let allow_headers = req_headers
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("*"));
    out.push((header::ACCESS_CONTROL_ALLOW_HEADERS, allow_headers));
    out
}

fn apply_cors(
    mut builder: axum::http::response::Builder,
    req_headers: &HeaderMap,
) -> axum::http::response::Builder {
    for (name, value) in cors_headers(req_headers) {
        builder = builder.header(name, value);
    }
    builder
}

fn error_response(status: StatusCode, msg: &'static str, req_headers: &HeaderMap) -> Response {
    let mut resp = (status, msg).into_response();
    for (name, value) in cors_headers(req_headers) {
        resp.headers_mut().insert(name, value);
    }
    resp
}

/// Strip the `Domain` attribute from a single `Set-Cookie` header value (see
/// the module doc comment on why: the upstream's own domain never matches
/// the hub's `127.0.0.1` origin, so the browser would otherwise silently
/// drop the cookie). Leaves every other attribute untouched.
fn strip_cookie_domain(value: &str) -> String {
    value
        .split(';')
        .filter(|part| !part.trim_start().to_ascii_lowercase().starts_with("domain="))
        .collect::<Vec<_>>()
        .join(";")
}

/// The host (plus non-default port, if any) to send as the outgoing `Host`
/// header for `base_url`, so an upstream doing name-based virtual hosting
/// sees its own hostname rather than whatever the original inbound request
/// carried (already stripped as hop-by-hop before this is applied).
fn upstream_host_header(base_url: &str) -> Option<String> {
    let url = reqwest::Url::parse(base_url).ok()?;
    let host = url.host_str()?;
    match url.port() {
        Some(p) => Some(format!("{host}:{p}")),
        None => Some(host.to_string()),
    }
}

fn record(state: &HubState, method: &Method, path: &str, status: u16, start: Instant) {
    let preset = state.inner.active.read().unwrap().preset_name.clone();
    state.inner.push_log(RequestLogEntry {
        ts: now_ms(),
        method: method.to_string(),
        path: path.to_string(),
        status,
        duration_ms: start.elapsed().as_millis() as u64,
        preset,
    });
}

async fn hub_handler(State(state): State<HubState>, req: Request) -> Response {
    let start = Instant::now();
    let method = req.method().clone();
    let path_and_query = path_and_query(&req);
    let req_headers_axum = req.headers().clone();

    // CORS preflight: answer directly, never forwarded upstream.
    if method == Method::OPTIONS {
        let mut builder = axum::http::Response::builder().status(StatusCode::NO_CONTENT);
        builder = apply_cors(builder, &req_headers_axum);
        let resp = builder
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::NO_CONTENT.into_response());
        record(&state, &method, &path_and_query, StatusCode::NO_CONTENT.as_u16(), start);
        return resp;
    }

    // Websocket/other-protocol upgrades cannot go through this fully-buffered
    // forwarder - fail loudly instead of returning a silently-broken response.
    if req_headers_axum.contains_key(header::UPGRADE) {
        log::error!(
            "proxy_hub: refusing an Upgrade request to {path_and_query} - websocket/upgrade traffic is not supported by this buffered HTTP forwarder"
        );
        record(&state, &method, &path_and_query, StatusCode::NOT_IMPLEMENTED.as_u16(), start);
        return error_response(
            StatusCode::NOT_IMPLEMENTED,
            "proxy hub: websocket/Upgrade requests are not supported",
            &req_headers_axum,
        );
    }

    let active = state.inner.active.read().unwrap().clone();
    let url = format!("{}{}", active.base_url, path_and_query);

    let mut req_headers = to_upstream_headers(&req_headers_axum);
    if let Some(host) = upstream_host_header(&active.base_url) {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&host) {
            req_headers.insert(reqwest::header::HOST, v);
        }
    }

    let reqwest_method = match to_reqwest_method(&method) {
        Ok(m) => m,
        Err(_) => {
            record(&state, &method, &path_and_query, StatusCode::BAD_GATEWAY.as_u16(), start);
            return error_response(StatusCode::BAD_GATEWAY, "proxy hub: bad method", &req_headers_axum);
        }
    };

    let body_bytes = match buffer_body(req.into_body()).await {
        Ok(b) => b,
        Err(_) => {
            record(&state, &method, &path_and_query, StatusCode::BAD_GATEWAY.as_u16(), start);
            return error_response(StatusCode::BAD_GATEWAY, "proxy hub: bad request body", &req_headers_axum);
        }
    };

    let upstream = match state
        .inner
        .client
        .request(reqwest_method, &url)
        .headers(req_headers)
        .body(body_bytes.to_vec())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            log::warn!("proxy_hub: upstream {url} unreachable: {e}");
            record(&state, &method, &path_and_query, StatusCode::BAD_GATEWAY.as_u16(), start);
            return error_response(
                StatusCode::BAD_GATEWAY,
                "proxy hub: upstream unreachable",
                &req_headers_axum,
            );
        }
    };

    let status = upstream.status();
    let upstream_headers = upstream.headers().clone();
    let body = match upstream.bytes().await {
        Ok(b) => b,
        Err(_) => {
            record(&state, &method, &path_and_query, StatusCode::BAD_GATEWAY.as_u16(), start);
            return error_response(
                StatusCode::BAD_GATEWAY,
                "proxy hub: upstream body error",
                &req_headers_axum,
            );
        }
    };

    let mut builder = axum::http::Response::builder()
        .status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY));
    builder = copy_response_headers(builder, &upstream_headers, |n| n.eq_ignore_ascii_case("set-cookie"));
    // Set-Cookie needs per-value Domain rewriting (see module docs), and
    // there can be several - `get_all` rather than the single-value getter.
    for value in upstream_headers.get_all(reqwest::header::SET_COOKIE) {
        if let Ok(s) = value.to_str() {
            let rewritten = strip_cookie_domain(s);
            if let Ok(hv) = axum::http::HeaderValue::from_str(&rewritten) {
                builder = builder.header(axum::http::header::SET_COOKIE, hv);
            }
        }
    }
    builder = apply_cors(builder, &req_headers_axum);

    record(&state, &method, &path_and_query, status.as_u16(), start);
    builder
        .body(Body::from(body.to_vec()))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// The project's active preset, resolved from `active_preset` and falling
/// back to the first preset when unset or stale (points at a since-removed
/// preset). `None` only when the project has no presets at all.
fn resolve_active_preset(project: &Project) -> Option<&UpstreamPreset> {
    project
        .active_preset
        .as_deref()
        .and_then(|id| project.presets.iter().find(|p| p.id == id))
        .or_else(|| project.presets.first())
}

// ----- Supervisor integration: lifecycle + preset CRUD + request log -----

impl Supervisor {
    /// Start a hub listener for every project that already has presets
    /// configured. Called once at startup (mirrors `readopt_orphans` /
    /// `start_autostart` - not folded into `Supervisor::new` itself, so
    /// tests that construct a bare `Supervisor` don't unexpectedly bind
    /// listeners).
    pub fn init_hubs(&self) {
        let projects = self.projects.lock().unwrap().clone();
        for project in &projects {
            if project.presets.is_empty() {
                continue;
            }
            if let Err(e) = self.start_hub_for(project) {
                log::error!("proxy_hub: failed to start hub for {}: {e}", project.id);
            }
        }
    }

    /// Spawn the hub listener for `project` on its fixed hub port, using its
    /// resolved active preset. No-op (Ok) if a hub is already running for
    /// this project, or if it has no presets yet.
    fn start_hub_for(&self, project: &Project) -> Result<(), String> {
        {
            let hubs = self.hubs.lock().unwrap();
            if hubs.contains_key(&project.id) {
                return Ok(());
            }
        }
        let Some(active) = resolve_active_preset(project) else {
            return Ok(());
        };
        let port = self.ports.project_hub_port(&project.id)?;
        let hub = spawn(port, active).map_err(|e| e.to_string())?;
        log::info!("proxy_hub: {} listening on http://127.0.0.1:{port}", project.id);
        self.hubs.lock().unwrap().insert(project.id.clone(), hub);
        Ok(())
    }

    /// The fixed port a project's hub listens on (or would listen on once it
    /// has at least one preset) - the stable address a dev app bakes in.
    pub fn hub_port(&self, project_id: &str) -> Result<u16, String> {
        self.ports.project_hub_port(project_id)
    }

    /// Add a named upstream preset to a project. The first preset added for a
    /// project starts its hub listener immediately (activated by default);
    /// subsequent presets are just added to the list until explicitly
    /// activated via `set_active_preset`.
    pub fn add_preset(
        &self,
        project_id: &str,
        name: String,
        base_url: String,
        danger: bool,
    ) -> Result<UpstreamPreset, String> {
        let name = name.trim().to_string();
        let base_url = base_url.trim().to_string();
        if name.is_empty() || base_url.is_empty() {
            return Err("preset name and base_url are required".to_string());
        }
        if reqwest::Url::parse(&base_url).is_err() {
            return Err(format!("'{base_url}' is not a valid URL"));
        }
        let (preset, project_snapshot, was_first) = {
            let mut projects = self.projects.lock().unwrap();
            let project = projects
                .iter_mut()
                .find(|p| p.id == project_id)
                .ok_or_else(|| format!("unknown project: {project_id}"))?;
            let id = crud::unique_id(&name, &|cand| project.presets.iter().any(|p| p.id == cand));
            let was_first = project.presets.is_empty();
            let preset = UpstreamPreset { id: id.clone(), name, base_url, danger };
            project.presets.push(preset.clone());
            if was_first {
                project.active_preset = Some(id);
            }
            let snapshot = project.clone();
            config::save(&self.data_dir, &projects);
            (preset, snapshot, was_first)
        };
        if was_first {
            if let Err(e) = self.start_hub_for(&project_snapshot) {
                log::error!("proxy_hub: failed to start hub for {project_id}: {e}");
            }
        }
        Ok(preset)
    }

    /// Remove a preset. If it was the active one, the next-first remaining
    /// preset becomes active (live-swapped into the running hub, no rebind).
    /// Removing the LAST preset stops and drops the hub entirely.
    pub fn remove_preset(&self, project_id: &str, preset_id: &str) -> Result<(), String> {
        let (project_snapshot, emptied) = {
            let mut projects = self.projects.lock().unwrap();
            let project = projects
                .iter_mut()
                .find(|p| p.id == project_id)
                .ok_or_else(|| format!("unknown project: {project_id}"))?;
            let before = project.presets.len();
            project.presets.retain(|p| p.id != preset_id);
            if project.presets.len() == before {
                return Err(format!("unknown preset: {preset_id}"));
            }
            if project.active_preset.as_deref() == Some(preset_id) {
                project.active_preset = project.presets.first().map(|p| p.id.clone());
            }
            let emptied = project.presets.is_empty();
            let snapshot = project.clone();
            config::save(&self.data_dir, &projects);
            (snapshot, emptied)
        };
        if emptied {
            self.stop_hub(project_id);
        } else if let Some(active) = resolve_active_preset(&project_snapshot) {
            self.swap_active(project_id, active);
        }
        Ok(())
    }

    /// Live-swap a project's hub to a different already-configured preset.
    /// The running listener is NOT rebound - only the shared upstream state
    /// it reads per request changes, so the app's baked-in address holds.
    pub fn set_active_preset(&self, project_id: &str, preset_id: &str) -> Result<(), String> {
        let project_snapshot = {
            let mut projects = self.projects.lock().unwrap();
            let project = projects
                .iter_mut()
                .find(|p| p.id == project_id)
                .ok_or_else(|| format!("unknown project: {project_id}"))?;
            if !project.presets.iter().any(|p| p.id == preset_id) {
                return Err(format!("unknown preset: {preset_id}"));
            }
            project.active_preset = Some(preset_id.to_string());
            let snapshot = project.clone();
            config::save(&self.data_dir, &projects);
            snapshot
        };
        // The project may not have a hub running yet (e.g. this is the very
        // first activation on a project whose presets were seeded by hand-
        // editing projects.json rather than through `add_preset`).
        if let Err(e) = self.start_hub_for(&project_snapshot) {
            log::error!("proxy_hub: failed to start hub for {project_id}: {e}");
        }
        if let Some(active) = resolve_active_preset(&project_snapshot) {
            self.swap_active(project_id, active);
        }
        Ok(())
    }

    fn swap_active(&self, project_id: &str, preset: &UpstreamPreset) {
        let hubs = self.hubs.lock().unwrap();
        if let Some(hub) = hubs.get(project_id) {
            hub.set_active(preset);
        }
    }

    /// Snapshot of a project's request log, oldest first. Empty (not an
    /// error) for a project with no hub running yet.
    pub fn hub_log(&self, project_id: &str) -> Vec<RequestLogEntry> {
        self.hubs
            .lock()
            .unwrap()
            .get(project_id)
            .map(|h| h.log_snapshot())
            .unwrap_or_default()
    }

    /// Stop and drop a project's hub listener, if any - called when the
    /// project is removed, or when its last preset is deleted.
    pub(super) fn stop_hub(&self, project_id: &str) {
        self.hubs.lock().unwrap().remove(project_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_cookie_domain_removes_only_the_domain_attribute() {
        let input = "sid=abc; Domain=example.com; Path=/; HttpOnly";
        assert_eq!(strip_cookie_domain(input), "sid=abc; Path=/; HttpOnly");
        // Case-insensitive attribute name.
        assert_eq!(strip_cookie_domain("sid=abc; DOMAIN=x.com"), "sid=abc");
        // No Domain present: unchanged.
        assert_eq!(strip_cookie_domain("sid=abc; Path=/"), "sid=abc; Path=/");
    }

    #[test]
    fn upstream_host_header_includes_nondefault_port_only() {
        assert_eq!(
            upstream_host_header("http://localhost:9000").as_deref(),
            Some("localhost:9000")
        );
        assert_eq!(
            upstream_host_header("https://staging.example.com").as_deref(),
            Some("staging.example.com")
        );
    }

    #[test]
    fn loopback_addr_is_always_loopback_never_external() {
        // spawn() has no code path that constructs a bind address any other
        // way, so proving this helper always yields a loopback address is
        // proving the listener can never bind externally.
        let addr = loopback_addr(4242);
        assert!(addr.ip().is_loopback());
        assert_eq!(addr, "127.0.0.1:4242".parse().unwrap());
    }

    #[test]
    fn is_loopback_origin_accepts_localhost_variants_only() {
        assert!(is_loopback_origin("http://127.0.0.1:5173"));
        assert!(is_loopback_origin("http://localhost:5173"));
        assert!(!is_loopback_origin("https://evil.example.com"));
        assert!(!is_loopback_origin("not a url"));
    }

    #[test]
    fn resolve_active_preset_falls_back_to_first_on_stale_or_unset_id() {
        let p1 = UpstreamPreset { id: "a".into(), name: "A".into(), base_url: "http://x".into(), danger: false };
        let p2 = UpstreamPreset { id: "b".into(), name: "B".into(), base_url: "http://y".into(), danger: false };
        let mut project = Project {
            id: "p".into(),
            name: "p".into(),
            root: ".".into(),
            commands: vec![],
            presets: vec![p1.clone(), p2.clone()],
            active_preset: None,
        };
        assert_eq!(resolve_active_preset(&project).unwrap().id, "a", "unset falls back to first");

        project.active_preset = Some("gone".to_string());
        assert_eq!(resolve_active_preset(&project).unwrap().id, "a", "stale id falls back to first");

        project.active_preset = Some("b".to_string());
        assert_eq!(resolve_active_preset(&project).unwrap().id, "b", "resolves the real active id");

        project.presets.clear();
        assert!(resolve_active_preset(&project).is_none(), "no presets: nothing to resolve");
    }
}
