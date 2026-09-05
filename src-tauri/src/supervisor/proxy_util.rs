//! Shared plumbing for `proxy.rs`'s per-process flutter-reload proxy and
//! `proxy_hub.rs`'s per-project hub: the loopback-listener bootstrap,
//! request/response forwarding helpers, and hop-by-hop header filtering.
//! Neither proxy owns this layer; both depend on it.

use axum::body::Body;
use axum::extract::Request;
use std::future::IntoFuture;
use std::sync::mpsc;
use tokio::sync::oneshot;

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
/// no default provider is set. We never speak TLS - the proxies target plain
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
    app: axum::Router,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_by_hop_detection_is_case_insensitive() {
        assert!(is_hop_by_hop("Connection"));
        assert!(is_hop_by_hop("transfer-encoding"));
        assert!(is_hop_by_hop("HOST"));
        assert!(!is_hop_by_hop("content-type"));
        assert!(!is_hop_by_hop("x-custom"));
    }
}
