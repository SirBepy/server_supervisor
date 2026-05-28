//! Integration tests for the localhost control API: token auth + endpoints,
//! driven over real HTTP against the actual axum router.

use server_supervisor_lib::api;
use server_supervisor_lib::supervisor::Supervisor;
use std::sync::Arc;

fn write_procs(dir: &std::path::Path) {
    let cwd = dir.display().to_string().replace('\\', "/");
    let json = format!(
        r#"[{{"id":"t1","project":"test","name":"job","cmd":"ping -n 30 127.0.0.1","cwd":"{cwd}","kind":"generic","autostart":false}}]"#
    );
    std::fs::write(dir.join("procs.json"), json).unwrap();
}

async fn spawn_api(token: &str, dir: &std::path::Path) -> String {
    let sup = Arc::new(Supervisor::new(dir.to_path_buf()));
    let app = api::router(sup, token.to_string());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn health_is_unauthenticated() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let r = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn procs_requires_token() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let no_token = client.get(format!("{base}/procs")).send().await.unwrap();
    assert_eq!(no_token.status(), 401);

    let wrong = client
        .get(format!("{base}/procs"))
        .bearer_auth("nope")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);

    let ok = client
        .get(format!("{base}/procs"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let list: Vec<serde_json::Value> = ok.json().await.unwrap();
    assert!(list.iter().any(|p| p["id"] == "t1"));
}

#[tokio::test]
async fn start_then_stop_over_api() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let start = client
        .post(format!("{base}/procs/t1/start"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 200);

    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    let logs: Vec<serde_json::Value> = client
        .get(format!("{base}/procs/t1/logs"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!logs.is_empty(), "logs should be captured after start");

    let stop = client
        .post(format!("{base}/procs/t1/stop"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(stop.status(), 200);
}

#[tokio::test]
async fn unknown_proc_start_is_400() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let r = client
        .post(format!("{base}/procs/ghost/start"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}
