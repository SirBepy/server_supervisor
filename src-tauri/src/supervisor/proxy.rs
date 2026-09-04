//! Per-flutter-web reverse proxy with live-reload injection.
//!
//! A flutter web-server build serves plain HTTP on an internal ephemeral port.
//! We sit a tiny axum reverse proxy in front of it on the public port the
//! dashboard advertises. The proxy:
//!   - forwards every request to `http://127.0.0.1:<internal>` verbatim,
//!   - injects a small `<script>` into HTML responses that opens an
//!     EventSource to `/__supervisor_reload`,
//!   - hosts that SSE endpoint, pushing a "reload" event whenever the flutter
//!     daemon reports a finished (re)start (the `reload_tx` broadcast from the
//!     stdout reader).
//!
//! The proxy runs on its OWN std::thread hosting a current-thread tokio runtime,
//! so `proc.rs` can spawn it from a synchronous context with no ambient runtime.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::convert::Infallible;
use std::future::IntoFuture;
use std::sync::mpsc;
use tokio::sync::{broadcast, oneshot};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

/// The live-reload bootstrap injected into proxied HTML. Opens an EventSource to
/// the proxy's SSE endpoint and reloads the page on any message.
const RELOAD_SCRIPT: &str = r#"<script>(function(){try{var s=new EventSource("/__supervisor_reload");s.onmessage=function(){location.reload();};}catch(e){}})();</script>"#;

/// Hop-by-hop headers that must never be forwarded across a proxy boundary.
const HOP_BY_HOP: &[&str] = &[
    "host",
    "connection",
    "keep-alive",
    "transfer-encoding",
    "upgrade",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
];

pub(crate) fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.iter().any(|h| name.eq_ignore_ascii_case(h))
}

/// Install the rustls `ring` crypto provider exactly once. reqwest 0.13 compiles
/// the rustls connector in (pulled by tauri-plugin-updater via feature
/// unification even though we ask for no TLS), and its `Client::new()` panics if
/// no default provider is set. We never speak TLS - the proxy targets plain
/// http://127.0.0.1 - but the client constructor still needs a provider present.
/// Idempotent: `install_default` errors if one is already set, which we ignore.
/// Public so the integration tests (which build their own reqwest clients before
/// any proxy is spawned) can satisfy the same constructor requirement.
pub fn ensure_crypto_provider() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Spawn `app` on `addr` inside its own OS thread hosting a current-thread
/// tokio runtime, so a synchronous caller can start a server with no ambient
/// runtime. Returns once the listener is bound, so a bind error surfaces
/// synchronously.
pub(crate) fn spawn_loopback_server(
    addr: std::net::SocketAddr,
    app: Router,
    log_target: &'static str,
) -> std::io::Result<(oneshot::Sender<()>, std::thread::JoinHandle<()>)> {
    let (bound_tx, bound_rx) = mpsc::channel::<std::io::Result<()>>();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let handle = std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = bound_tx.send(Err(e));
                return;
            }
        };
        rt.block_on(async move {
            let listener = match tokio::net::TcpListener::bind(addr).await {
                Ok(l) => l,
                Err(e) => {
                    let _ = bound_tx.send(Err(e));
                    return;
                }
            };
            let _ = bound_tx.send(Ok(()));

            // Race serve against shutdown rather than a graceful drain: a
            // held-open stream (SSE, long-poll) could block a drain
            // indefinitely, and stop()'s join can't wedge while callers
            // hold the procs/projects lock.
            tokio::select! {
                res = axum::serve(listener, app).into_future() => {
                    if let Err(e) = res {
                        log::error!("{log_target}: serve ended with error: {e}");
                    }
                }
                _ = shutdown_rx => {}
            }
        });
    });

    match bound_rx.recv() {
        Ok(Ok(())) => Ok((shutdown_tx, handle)),
        Ok(Err(e)) => {
            let _ = handle.join();
            Err(e)
        }
        Err(_) => {
            let _ = handle.join();
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("{log_target} thread exited before binding"),
            ))
        }
    }
}

/// A running reverse proxy. Dropping it (or calling `stop`) signals graceful
/// shutdown and joins the proxy thread.
pub struct ProxyTask {
    shutdown: Option<oneshot::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ProxyTask {
    /// Signal graceful shutdown and join the proxy thread. Idempotent.
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for ProxyTask {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone)]
struct ProxyState {
    internal_port: u16,
    reload_tx: broadcast::Sender<()>,
    client: reqwest::Client,
}

/// Spawn a reverse proxy on `127.0.0.1:public_port` forwarding to
/// `http://127.0.0.1:internal_port`, injecting a live-reload script into HTML
/// and hosting the SSE endpoint the script subscribes to. Returns once the
/// listener is bound so a bind error surfaces synchronously.
pub fn spawn(
    public_port: u16,
    internal_port: u16,
    reload_tx: broadcast::Sender<()>,
) -> std::io::Result<ProxyTask> {
    ensure_crypto_provider();
    let state = ProxyState {
        internal_port,
        reload_tx,
        client: reqwest::Client::new(),
    };
    let app = Router::new()
        .route("/__supervisor_reload", get(sse_handler))
        .fallback(proxy_handler)
        .with_state(state);
    let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, public_port));

    let (shutdown_tx, handle) = spawn_loopback_server(addr, app, "proxy")?;
    Ok(ProxyTask {
        shutdown: Some(shutdown_tx),
        handle: Some(handle),
    })
}

/// SSE endpoint: emit a "reload" event each time the daemon signals a finished
/// (re)start via the broadcast channel.
async fn sse_handler(
    State(state): State<ProxyState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.reload_tx.subscribe();
    let stream = BroadcastStream::new(rx)
        .map(|_| Ok::<_, Infallible>(Event::default().data("reload")));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Cap on a proxied request/response body. Generous: proxied dev assets and
/// API payloads are small, this just guards against a runaway upstream.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// The inbound request's path plus query string, verbatim.
pub(crate) fn path_and_query(req: &Request) -> String {
    req.uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string())
}

/// Copy `headers` into a fresh reqwest `HeaderMap`, skipping hop-by-hop
/// headers and reconstructing by parsing names/values (avoids an http-crate
/// version mismatch between axum and reqwest).
pub(crate) fn to_upstream_headers(headers: &axum::http::HeaderMap) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::new();
    for (name, value) in headers.iter() {
        if is_hop_by_hop(name.as_str()) {
            continue;
        }
        if let (Ok(n), Ok(v)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            out.insert(n, v);
        }
    }
    out
}

/// Convert an axum request method to reqwest's method type.
pub(crate) fn to_reqwest_method(method: &axum::http::Method) -> Result<reqwest::Method, ()> {
    reqwest::Method::from_bytes(method.as_str().as_bytes()).map_err(|_| ())
}

/// Buffer a request/response body up to `MAX_BODY_BYTES`.
pub(crate) async fn buffer_body(body: Body) -> Result<axum::body::Bytes, axum::Error> {
    axum::body::to_bytes(body, MAX_BODY_BYTES).await
}

/// Copy `headers` onto `builder`, skipping hop-by-hop headers and any name
/// for which `skip_extra` returns true (e.g. `set-cookie` when it needs
/// separate per-value rewriting, or a stale `content-length` after the body
/// changed).
pub(crate) fn copy_response_headers(
    mut builder: axum::http::response::Builder,
    headers: &reqwest::header::HeaderMap,
    skip_extra: impl Fn(&str) -> bool,
) -> axum::http::response::Builder {
    for (name, value) in headers.iter() {
        let n = name.as_str();
        if is_hop_by_hop(n) || skip_extra(n) {
            continue;
        }
        if let (Ok(hn), Ok(hv)) = (
            axum::http::HeaderName::from_bytes(n.as_bytes()),
            axum::http::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            builder = builder.header(hn, hv);
        }
    }
    builder
}

/// Reverse-proxy every other request to the internal flutter web-server,
/// injecting the live-reload script into HTML responses.
async fn proxy_handler(State(state): State<ProxyState>, req: Request) -> Response {
    let path_and_query = path_and_query(&req);
    let url = format!(
        "http://127.0.0.1:{}{}",
        state.internal_port, path_and_query
    );

    let method = req.method().clone();
    let req_headers = to_upstream_headers(req.headers());

    let body_bytes = match buffer_body(req.into_body()).await {
        Ok(b) => b,
        Err(_) => return (axum::http::StatusCode::BAD_GATEWAY, "proxy: bad request body").into_response(),
    };

    let method = match to_reqwest_method(&method) {
        Ok(m) => m,
        Err(_) => return (axum::http::StatusCode::BAD_GATEWAY, "proxy: bad method").into_response(),
    };

    let upstream = match state
        .client
        .request(method, &url)
        .headers(req_headers)
        .body(body_bytes.to_vec())
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => {
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                "proxy: upstream unreachable",
            )
                .into_response()
        }
    };

    let status = upstream.status();
    let upstream_headers = upstream.headers().clone();
    let body = match upstream.bytes().await {
        Ok(b) => b,
        Err(_) => {
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                "proxy: upstream body error",
            )
                .into_response()
        }
    };

    let content_type = upstream_headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let content_encoding = upstream_headers
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok());
    let do_inject = should_inject_html(content_type, content_encoding);

    let mut builder = axum::http::Response::builder()
        .status(axum::http::StatusCode::from_u16(status.as_u16()).unwrap_or(axum::http::StatusCode::BAD_GATEWAY));

    if do_inject {
        // Rewrite the HTML body: inject the reload script. We changed the body,
        // so drop the upstream content-length and content-encoding and set a
        // fresh content-length below.
        let injected = inject_reload_script(&String::from_utf8_lossy(&body));
        let out = injected.into_bytes();
        builder = copy_response_headers(builder, &upstream_headers, |n| {
            n.eq_ignore_ascii_case("content-length") || n.eq_ignore_ascii_case("content-encoding")
        });
        builder = builder.header(axum::http::header::CONTENT_LENGTH, out.len());
        builder
            .body(Body::from(out))
            .unwrap_or_else(|_| axum::http::StatusCode::BAD_GATEWAY.into_response())
    } else {
        // Pass through unchanged (non-HTML, or encoded HTML we won't touch).
        builder = copy_response_headers(builder, &upstream_headers, |_| false);
        builder
            .body(Body::from(body.to_vec()))
            .unwrap_or_else(|_| axum::http::StatusCode::BAD_GATEWAY.into_response())
    }
}

/// Decide whether to rewrite this response to inject the reload script. Only
/// `text/html` responses qualify, and only when the body is uncompressed:
/// editing a gzip/br-encoded body as text would corrupt it, so an encoded HTML
/// response is passed through untouched. Absent or `identity` encoding is safe.
fn should_inject_html(content_type: Option<&str>, content_encoding: Option<&str>) -> bool {
    let is_html = content_type
        .map(|ct| ct.trim_start().to_ascii_lowercase().starts_with("text/html"))
        .unwrap_or(false);
    if !is_html {
        return false;
    }
    let encoded = match content_encoding {
        Some(enc) => {
            let e = enc.trim().to_ascii_lowercase();
            !e.is_empty() && e != "identity"
        }
        None => false,
    };
    !encoded
}

/// Inject the live-reload script into an HTML document. Insert immediately before
/// `</body>` (case-insensitive); else before `</head>`; else append to the end.
fn inject_reload_script(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    if let Some(idx) = lower.rfind("</body>") {
        let mut out = String::with_capacity(html.len() + RELOAD_SCRIPT.len());
        out.push_str(&html[..idx]);
        out.push_str(RELOAD_SCRIPT);
        out.push_str(&html[idx..]);
        return out;
    }
    if let Some(idx) = lower.rfind("</head>") {
        let mut out = String::with_capacity(html.len() + RELOAD_SCRIPT.len());
        out.push_str(&html[..idx]);
        out.push_str(RELOAD_SCRIPT);
        out.push_str(&html[idx..]);
        return out;
    }
    let mut out = String::with_capacity(html.len() + RELOAD_SCRIPT.len());
    out.push_str(html);
    out.push_str(RELOAD_SCRIPT);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_before_body_close() {
        let html = "<html><head></head><body><h1>hi</h1></body></html>";
        let out = inject_reload_script(html);
        let body_idx = out.find("</body>").unwrap();
        let script_idx = out.find("EventSource").unwrap();
        assert!(script_idx < body_idx, "script must land before </body>");
        // Exactly one injection.
        assert_eq!(out.matches("EventSource").count(), 1);
    }

    #[test]
    fn injects_before_head_close_when_no_body() {
        let html = "<html><head><title>x</title></head></html>";
        let out = inject_reload_script(html);
        let head_idx = out.find("</head>").unwrap();
        let script_idx = out.find("EventSource").unwrap();
        assert!(script_idx < head_idx, "script must land before </head>");
    }

    #[test]
    fn appends_when_no_body_or_head() {
        let html = "<div>fragment</div>";
        let out = inject_reload_script(html);
        assert!(out.starts_with("<div>fragment</div>"));
        assert!(out.trim_end().ends_with("</script>"));
        assert_eq!(out.matches("EventSource").count(), 1);
    }

    #[test]
    fn injection_is_case_insensitive() {
        let html = "<HTML><BODY>hi</BODY></HTML>";
        let out = inject_reload_script(html);
        let body_idx = out.find("</BODY>").unwrap();
        let script_idx = out.find("EventSource").unwrap();
        assert!(script_idx < body_idx);
    }

    #[test]
    fn should_inject_only_uncompressed_html() {
        // Plain HTML, no encoding: inject.
        assert!(should_inject_html(Some("text/html; charset=utf-8"), None));
        assert!(should_inject_html(Some("text/html"), Some("identity")));
        // HTML but gzipped / br: skip (would corrupt the encoded body).
        assert!(!should_inject_html(Some("text/html"), Some("gzip")));
        assert!(!should_inject_html(Some("text/html"), Some("br")));
        // Non-HTML: never inject.
        assert!(!should_inject_html(Some("application/javascript"), None));
        assert!(!should_inject_html(Some("image/png"), None));
        assert!(!should_inject_html(None, None));
    }

    #[test]
    fn hop_by_hop_detection_is_case_insensitive() {
        assert!(is_hop_by_hop("Connection"));
        assert!(is_hop_by_hop("transfer-encoding"));
        assert!(is_hop_by_hop("HOST"));
        assert!(!is_hop_by_hop("content-type"));
        assert!(!is_hop_by_hop("x-custom"));
    }
}
