//! Integration tests for the localhost control API: token auth + endpoints,
//! driven over real HTTP against the actual axum router.

use server_supervisor_lib::api;
use server_supervisor_lib::ports::PortRegistry;
use server_supervisor_lib::supervisor::Supervisor;
use std::sync::Arc;

fn write_procs(dir: &std::path::Path) {
    let root = dir.display().to_string().replace('\\', "/");
    let json = format!(
        r#"[{{"id":"test","name":"test","root":"{root}","commands":[{{"id":"job","name":"job","cmd":"ping -n 30 127.0.0.1","kind":"generic","autostart":false}}]}}]"#
    );
    std::fs::write(dir.join("projects.json"), json).unwrap();
}

async fn spawn_api(token: &str, dir: &std::path::Path) -> String {
    let ports = Arc::new(PortRegistry::new(dir.to_path_buf()));
    let sup = Arc::new(Supervisor::new(dir.to_path_buf(), ports.clone()));
    let app = api::router(sup, ports, token.to_string());
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
    assert!(list.iter().any(|p| p["id"] == "test:job"));
}

#[tokio::test]
async fn ports_requires_token_and_lists_seeds() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let no_token = client.get(format!("{base}/ports")).send().await.unwrap();
    assert_eq!(no_token.status(), 401);

    let ok = client
        .get(format!("{base}/ports"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let list: Vec<serde_json::Value> = ok.json().await.unwrap();
    assert!(list.iter().any(|p| p["port"] == 6969));
}

#[tokio::test]
async fn reserve_port_over_api() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let port: u16 = client
        .post(format!("{base}/ports/reserve"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "owner": "my-app" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!((42000..49000).contains(&port));
}

#[tokio::test]
async fn start_then_stop_over_api() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let start = client
        .post(format!("{base}/procs/test:job/start"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 200);

    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    let logs: Vec<serde_json::Value> = client
        .get(format!("{base}/procs/test:job/logs"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!logs.is_empty(), "logs should be captured after start");

    let stop = client
        .post(format!("{base}/procs/test:job/stop"))
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

#[tokio::test]
async fn run_registers_starts_requires_token_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("server.js"),
        "const p=process.env.PORT;require('http').createServer((_,r)=>r.end('ok')).listen(p,()=>console.log('LISTENING '+p));",
    )
    .unwrap();
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();
    let root = dir.path().display().to_string();
    let body = serde_json::json!({ "root": root, "cmd": "node server.js" });

    // Auth required.
    let no_token = client
        .post(format!("{base}/run"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(no_token.status(), 401);

    // With token: registers + starts, returns ProcInfo with a dynamic port.
    let info: serde_json::Value = client
        .post(format!("{base}/run"))
        .bearer_auth("secret")
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = info["id"].as_str().unwrap().to_string();
    let port = info["port"].as_u64().unwrap();
    assert!((42000..49000).contains(&(port as u16)));

    // Idempotent: a second /run with the same root+cmd reuses the same unit.
    let info2: serde_json::Value = client
        .post(format!("{base}/run"))
        .bearer_auth("secret")
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info2["id"].as_str().unwrap(), id);

    // Teardown.
    let _ = client
        .post(format!("{base}/procs/{id}/stop"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
}
