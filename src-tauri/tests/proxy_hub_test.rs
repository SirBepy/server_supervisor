//! Integration tests for the multi-project reverse-proxy hub
//! (`supervisor::proxy_hub`): real stub upstream HTTP servers, driven through
//! the real `Supervisor` preset CRUD, hitting the hub's real loopback
//! listener over real HTTP.

use axum::extract::State as AxumState;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use server_supervisor_lib::ports::PortRegistry;
use server_supervisor_lib::supervisor::proxy::ensure_crypto_provider;
use server_supervisor_lib::supervisor::Supervisor;
use std::sync::{Arc, Mutex};

/// Spawn a tiny stub "upstream" HTTP server on an OS-assigned loopback port.
/// `tag` is echoed back in the body and an `x-upstream` header so a test can
/// tell which upstream actually served a request. Every `/echo` response also
/// carries a hop-by-hop `Proxy-Authenticate` header (must be stripped by the
/// hub) and a `Set-Cookie` whose `Domain` names the upstream's own fake host
/// (must have that attribute stripped by the hub). Returns the stub's base
/// URL plus a shared slot the last request's headers are captured into, so a
/// test can assert what the hub actually forwarded.
async fn spawn_stub(tag: &'static str) -> (String, Arc<Mutex<Option<HeaderMap>>>) {
    let captured: Arc<Mutex<Option<HeaderMap>>> = Arc::new(Mutex::new(None));

    async fn echo(
        AxumState((tag, captured)): AxumState<(&'static str, Arc<Mutex<Option<HeaderMap>>>)>,
        headers: HeaderMap,
    ) -> Response {
        *captured.lock().unwrap() = Some(headers);
        axum::http::Response::builder()
            .status(200)
            .header("x-upstream", tag)
            .header("proxy-authenticate", "Basic realm=stub") // hop-by-hop: must be stripped
            .header(
                "set-cookie",
                format!("sid=abc-{tag}; Domain=upstream-{tag}.internal; Path=/; HttpOnly"),
            )
            .body(axum::body::Body::from(tag))
            .unwrap()
            .into_response()
    }

    let app = Router::new()
        .route("/echo", get(echo))
        .with_state((tag, captured.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), captured)
}

/// Each test in this file binds a REAL OS socket on its project's fixed hub
/// port (`base+9` of a fresh `PortRegistry`'s first block). A fresh registry
/// is otherwise fully isolated per test (its own tempdir), but the
/// deterministic arithmetic means every test's "first project ever" wants
/// the SAME absolute port (42009) - fine for the purely-arithmetic unit
/// tests in `ports.rs`, but a real bind collision when this file's
/// `#[tokio::test]` functions run in parallel threads (the default). Seed
/// each test with a unique number of one-port-per-block shims so its project
/// lands in a block none of this file's other tests use. 10 mirrors
/// `ports::BLOCK_SIZE`, which isn't exported to integration tests.
static NEXT_BLOCK: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

fn new_isolated_sup(dir: &std::path::Path) -> Supervisor {
    let ports = PortRegistry::new(dir.to_path_buf());
    let i = NEXT_BLOCK.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    for n in 0..i {
        ports.reserve(&format!("shim-{n}"), 42000 + n * 10, "test isolation shim");
    }
    Supervisor::new(dir.to_path_buf(), Arc::new(ports))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forwards_swaps_live_strips_hop_by_hop_and_rewrites_cookie_domain() {
    ensure_crypto_provider();
    let dir = tempfile::tempdir().unwrap();
    let sup = new_isolated_sup(dir.path());

    let (url_a, captured_a) = spawn_stub("a").await;
    let (url_b, _captured_b) = spawn_stub("b").await;

    let project = sup
        .add_project("hubtest".to_string(), dir.path().display().to_string())
        .unwrap();
    let preset_a = sup.add_preset(&project.id, "local".to_string(), url_a, false).unwrap();
    let preset_b = sup.add_preset(&project.id, "develop".to_string(), url_b, false).unwrap();

    // Adding the FIRST preset must have started the hub on its fixed port.
    let hub_port = sup.hub_port(&project.id).unwrap();
    let hub_base = format!("http://127.0.0.1:{hub_port}");

    let client = reqwest::Client::new();

    // --- request 1: default-active preset A ---
    let r1 = client
        .get(format!("{hub_base}/echo"))
        .header("proxy-authorization", "should-not-be-forwarded") // hop-by-hop
        .header("x-my-app", "keep-me") // ordinary header: must be forwarded
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), 200);
    assert_eq!(r1.headers().get("x-upstream").unwrap(), "a");
    // Hop-by-hop response header must be stripped.
    assert!(r1.headers().get("proxy-authenticate").is_none());
    // Set-Cookie's Domain must be gone; the rest of the cookie survives.
    let cookie = r1.headers().get("set-cookie").unwrap().to_str().unwrap();
    assert!(!cookie.to_lowercase().contains("domain="), "domain must be stripped: {cookie}");
    assert!(cookie.contains("sid=abc-a"));
    assert!(cookie.contains("HttpOnly"));
    let body_a = r1.text().await.unwrap();
    assert_eq!(body_a, "a");

    // The upstream must have received the ordinary header but not the
    // hop-by-hop one, proving real filtering (not "drop everything").
    let seen = captured_a.lock().unwrap().take().unwrap();
    assert_eq!(seen.get("x-my-app").unwrap(), "keep-me");
    assert!(seen.get("proxy-authorization").is_none(), "hop-by-hop request header must not reach upstream");
    assert!(seen.get("connection").is_none());

    // --- live swap: SAME hub_port, no rebind, must now forward to B ---
    sup.set_active_preset(&project.id, &preset_b.id).unwrap();
    let hub_port_after_swap = sup.hub_port(&project.id).unwrap();
    assert_eq!(hub_port_after_swap, hub_port, "the fixed hub port must never change");

    let r2 = client.get(format!("{hub_base}/echo")).send().await.unwrap();
    assert_eq!(r2.status(), 200);
    assert_eq!(r2.headers().get("x-upstream").unwrap(), "b");
    let body_b = r2.text().await.unwrap();
    assert_eq!(body_b, "b");

    // --- request log records both requests with the correct serving preset ---
    let log = sup.hub_log(&project.id);
    assert!(log.len() >= 2, "expected at least 2 entries, got {}", log.len());
    let last_two = &log[log.len() - 2..];
    assert_eq!(last_two[0].preset, "local");
    assert_eq!(last_two[0].status, 200);
    assert_eq!(last_two[0].method, "GET");
    assert_eq!(last_two[0].path, "/echo");
    assert_eq!(last_two[1].preset, "develop");
    assert_eq!(last_two[1].status, 200);

    // Swap back to A explicitly by id to also prove `preset_a` id round-trips.
    sup.set_active_preset(&project.id, &preset_a.id).unwrap();
    let r3 = client.get(format!("{hub_base}/echo")).send().await.unwrap();
    assert_eq!(r3.headers().get("x-upstream").unwrap(), "a");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_log_ring_buffer_caps_at_500() {
    ensure_crypto_provider();
    let dir = tempfile::tempdir().unwrap();
    let sup = new_isolated_sup(dir.path());
    let (url, _captured) = spawn_stub("only").await;

    let project = sup
        .add_project("ringtest".to_string(), dir.path().display().to_string())
        .unwrap();
    sup.add_preset(&project.id, "only".to_string(), url, false).unwrap();
    let hub_port = sup.hub_port(&project.id).unwrap();
    let hub_base = format!("http://127.0.0.1:{hub_port}");

    let client = reqwest::Client::new();
    for _ in 0..520 {
        let r = client.get(format!("{hub_base}/echo")).send().await.unwrap();
        assert_eq!(r.status(), 200);
    }

    let log = sup.hub_log(&project.id);
    assert_eq!(log.len(), 500, "ring buffer must cap at 500 entries even after 520 requests");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn options_preflight_answered_directly_without_hitting_upstream() {
    ensure_crypto_provider();
    let dir = tempfile::tempdir().unwrap();
    let sup = new_isolated_sup(dir.path());
    let (url, captured) = spawn_stub("cors").await;

    let project = sup
        .add_project("corstest".to_string(), dir.path().display().to_string())
        .unwrap();
    sup.add_preset(&project.id, "only".to_string(), url, false).unwrap();
    let hub_port = sup.hub_port(&project.id).unwrap();
    let hub_base = format!("http://127.0.0.1:{hub_port}");

    let client = reqwest::Client::new();
    let r = client
        .request(reqwest::Method::OPTIONS, format!("{hub_base}/echo"))
        .header("origin", "http://127.0.0.1:5173")
        .header("access-control-request-method", "GET")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    assert_eq!(
        r.headers().get("access-control-allow-origin").unwrap(),
        "http://127.0.0.1:5173"
    );
    assert_eq!(r.headers().get("access-control-allow-credentials").unwrap(), "true");
    // The preflight must never have reached the stub upstream.
    assert!(captured.lock().unwrap().is_none(), "OPTIONS preflight must not be forwarded upstream");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upgrade_request_is_rejected_not_silently_forwarded() {
    ensure_crypto_provider();
    let dir = tempfile::tempdir().unwrap();
    let sup = new_isolated_sup(dir.path());
    let (url, captured) = spawn_stub("ws").await;

    let project = sup
        .add_project("wstest".to_string(), dir.path().display().to_string())
        .unwrap();
    sup.add_preset(&project.id, "only".to_string(), url, false).unwrap();
    let hub_port = sup.hub_port(&project.id).unwrap();
    let hub_base = format!("http://127.0.0.1:{hub_port}");

    let client = reqwest::Client::new();
    let r = client
        .get(format!("{hub_base}/echo"))
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 501, "an Upgrade request must be rejected, not mangled");
    assert!(captured.lock().unwrap().is_none(), "an Upgrade request must never reach the upstream");
}
