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

/// Project "test" with a templated command "job" (a `device` axis, values
/// `web-server`/`chrome`, defaulting to `web-server`) plus a plain untemplated
/// command "job2", for the params HTTP tests below. The param text lands
/// inside a harmless `set` no-op rather than directly on `ping`'s own argument
/// list, so an unrecognized flag can never make `ping` exit immediately.
fn write_procs_with_params(dir: &std::path::Path) {
    let root = dir.display().to_string().replace('\\', "/");
    let json = format!(
        r#"[{{"id":"test","name":"test","root":"{root}","commands":[
            {{"id":"job","name":"job","cmd":"set CHOICE={{DEVICE}} & ping -n 30 127.0.0.1","kind":"generic","autostart":false,
              "params":[{{"name":"device","label":"Device","values":[
                {{"value":"web-server","label":"Web Server","flag":"-d web-server"}},
                {{"value":"chrome","label":"Chrome","flag":"-d chrome"}}
              ],"last_value":"web-server"}}]}},
            {{"id":"job2","name":"job2","cmd":"cmd /C exit 0","kind":"generic","autostart":false}}
        ]}}]"#
    );
    std::fs::write(dir.join("projects.json"), json).unwrap();
}

async fn spawn_api(token: &str, dir: &std::path::Path) -> String {
    // reqwest 0.13's Client::new() requires a rustls crypto provider be installed
    // (the connector is compiled in via feature unification). Every test builds a
    // client after this call, so installing here covers them all.
    server_supervisor_lib::supervisor::proxy::ensure_crypto_provider();
    let ports = Arc::new(PortRegistry::new(dir.to_path_buf()));
    let sup = Arc::new(Supervisor::new(dir.to_path_buf(), ports.clone()));
    // No Tauri app in these tests (see `router`'s own doc): every route below
    // works with `app_handle: None` except `/procs/:id/dock`, which needs a
    // real one to marshal onto the main thread.
    let app = api::router(sup, ports, token.to_string(), None, dir.to_path_buf(), None);
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
async fn command_crud_requires_token_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path()); // project "test" with command "job"
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    // Auth enforced: an unauthed update is rejected before touching state.
    let unauthed = client
        .patch(format!("{base}/projects/test/commands/job"))
        .json(&serde_json::json!({ "name": "job", "cmd": "ping -n 1 127.0.0.1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthed.status(), 401);

    // Update (happy path): full field replace, returns the updated Command.
    let updated: serde_json::Value = client
        .patch(format!("{base}/projects/test/commands/job"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "name": "renamed", "cmd": "ping -n 5 127.0.0.1" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated["id"], "job", "update keeps the stable command id");
    assert_eq!(updated["name"], "renamed");
    assert_eq!(updated["cmd"], "ping -n 5 127.0.0.1");

    // Add a second command; it shows up as a new unit in /procs.
    let added: serde_json::Value = client
        .post(format!("{base}/projects/test/commands"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "name": "build", "cmd": "ping -n 2 127.0.0.1" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let added_id = added["id"].as_str().unwrap().to_string();
    let procs: Vec<serde_json::Value> = client
        .get(format!("{base}/procs"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(procs.iter().any(|p| p["id"] == format!("test:{added_id}")));

    // Remove the original command (not running, so allowed); it disappears.
    let removed = client
        .delete(format!("{base}/projects/test/commands/job"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), 200);
    let after: Vec<serde_json::Value> = client
        .get(format!("{base}/procs"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!after.iter().any(|p| p["id"] == "test:job"), "removed unit is gone");

    // Unknown project/command -> 400, mirroring the other unit handlers.
    let ghost = client
        .delete(format!("{base}/projects/test/commands/ghost"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(ghost.status(), 400);
}

#[tokio::test]
async fn hub_port_without_presets_is_404() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path()); // project "test" has no presets
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let no_token = client
        .get(format!("{base}/projects/test/hub-port"))
        .send()
        .await
        .unwrap();
    assert_eq!(no_token.status(), 401);

    let r = client
        .get(format!("{base}/projects/test/hub-port"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn hub_port_with_preset_returns_the_port() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let added = client
        .post(format!("{base}/projects/test/presets"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "name": "local", "base_url": "http://127.0.0.1:3000" }))
        .send()
        .await
        .unwrap();
    assert_eq!(added.status(), 200);

    let port: u16 = client
        .get(format!("{base}/projects/test/hub-port"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!((42000..49000).contains(&port));
}

#[tokio::test]
async fn set_group_on_unknown_project_is_404() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path()); // project "test" exists; "ghost" does not
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let r = client
        .patch(format!("{base}/projects/ghost/group"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "group_id": null }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "unknown project_id must 404, matching the preset handlers");
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

#[tokio::test]
async fn procs_payload_includes_window_field() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path()); // project "test", command "job", not started
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let procs: Vec<serde_json::Value> = client
        .get(format!("{base}/procs"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let job = procs.iter().find(|p| p["id"] == "test:job").unwrap();
    // Not started -> no pid -> nothing to probe, so the window key must
    // still be present (extending the payload, not replacing it) but null.
    assert!(job.get("window").is_some(), "window key must be present on every proc");
    assert!(job["window"].is_null(), "a never-started proc has no pid to probe a window for");
}

#[tokio::test]
async fn windows_route_lists_none_for_a_console_process() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    client.post(format!("{base}/procs/test:job/start")).bearer_auth("secret").send().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // `ping` is a console process with no top-level window: the route must
    // still answer 200 with an empty list, not 404/500, since "no window"
    // and "unknown proc" are different failure shapes.
    let r = client
        .get(format!("{base}/procs/test:job/windows"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let windows: Vec<serde_json::Value> = r.json().await.unwrap();
    assert!(windows.is_empty(), "a console process owns no top-level window");

    client.post(format!("{base}/procs/test:job/stop")).bearer_auth("secret").send().await.unwrap();
}

#[tokio::test]
async fn windows_route_requires_token() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let r = client.get(format!("{base}/procs/test:job/windows")).send().await.unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn screenshot_rejects_a_window_outside_the_procs_tree() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    client.post(format!("{base}/procs/test:job/start")).bearer_auth("secret").send().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // A made-up hwnd belongs to no live window at all, let alone this proc's
    // pid tree - `resolve_window` must reject it before `/screenshot` ever
    // tries to capture an arbitrary window of an unrelated process.
    let r = client
        .get(format!("{base}/procs/test:job/screenshot?window=999999999"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    client.post(format!("{base}/procs/test:job/stop")).bearer_auth("secret").send().await.unwrap();
}

#[tokio::test]
async fn add_command_with_dock_window_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    // Hole 1: the HTTP add-command body can turn docking on; the hardcoded
    // `false` this used to carry regardless of caller intent is gone.
    let added: serde_json::Value = client
        .post(format!("{base}/projects/test/commands"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "name": "dockable", "cmd": "ping -n 2 127.0.0.1", "dock_window": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(added["dock_window"], true);

    // Omitting the field still defaults to undocked, matching the on-disk
    // `#[serde(default)]` and the dashboard's own add-command flow. A
    // distinct `cmd` string is required: `add_command` is idempotent on the
    // exact `cmd` within a project (see `crud::command::add_command`), so
    // reusing the first command's `cmd` here would just return it unchanged.
    let added_default: serde_json::Value = client
        .post(format!("{base}/projects/test/commands"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "name": "not-dockable", "cmd": "ping -n 3 127.0.0.1" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(added_default["dock_window"], false);
}

#[tokio::test]
async fn dock_route_requires_token() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let no_token = client
        .post(format!("{base}/procs/test:job/dock"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(no_token.status(), 401, "the dock route must sit behind the same bearer check as every other route");

    let wrong = client
        .post(format!("{base}/procs/test:job/dock"))
        .bearer_auth("nope")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
}

#[tokio::test]
async fn dock_route_without_app_handle_fails_gracefully() {
    // These integration tests build the router with no Tauri app (see
    // `spawn_api`), which is exactly the state the dock route must not panic
    // in - it must report unavailable, not crash the request thread.
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let r = client
        .post(format!("{base}/procs/test:job/dock"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "rect": { "left": 0, "top": 0, "right": 100, "bottom": 100 } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);

    // The undock direction (no `rect`) is the same graceful path.
    let r2 = client
        .post(format!("{base}/procs/test:job/dock"))
        .bearer_auth("secret")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 503);
}

#[tokio::test]
async fn run_rejects_unknown_param_name_and_value_and_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    write_procs_with_params(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();
    let root = dir.path().display().to_string();
    let templated_cmd = "set CHOICE={DEVICE} & ping -n 30 127.0.0.1";

    let bad_name = client
        .post(format!("{base}/run"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "root": root, "cmd": templated_cmd, "params": { "flavor": "dev" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_name.status(), 400);
    let body = bad_name.text().await.unwrap();
    assert!(body.contains("device"), "error must list the valid param name: {body}");

    let bad_value = client
        .post(format!("{base}/run"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "root": root, "cmd": templated_cmd, "params": { "device": "firefox" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_value.status(), 400);
    let body2 = bad_value.text().await.unwrap();
    assert!(body2.contains("web-server") || body2.contains("chrome"), "error must list the valid value ids: {body2}");

    // Neither rejected request started anything or touched `last_value`.
    let procs: Vec<serde_json::Value> = client
        .get(format!("{base}/procs"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let job = procs.iter().find(|p| p["id"] == "test:job").unwrap();
    assert_eq!(job["status"], "stopped", "a rejected params object must start nothing");
    assert_eq!(job["resolved_cmd"], "set CHOICE=-d web-server & ping -n 30 127.0.0.1", "last_value must stay the default");
}

#[tokio::test]
async fn run_with_valid_params_starts_the_requested_variant() {
    let dir = tempfile::tempdir().unwrap();
    write_procs_with_params(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();
    let root = dir.path().display().to_string();

    let info: serde_json::Value = client
        .post(format!("{base}/run"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "root": root,
            "cmd": "set CHOICE={DEVICE} & ping -n 30 127.0.0.1",
            "params": { "device": "chrome" }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["id"], "test:job", "the templated command's own id, not a fork");
    assert_eq!(info["status"], "running");
    assert_eq!(info["resolved_cmd"], "set CHOICE=-d chrome & ping -n 30 127.0.0.1");
    assert!(
        info.get("param_mismatch").is_none(),
        "an explicit params object on a stopped command is never a mismatch"
    );

    let _ = client.post(format!("{base}/procs/test:job/stop")).bearer_auth("secret").send().await.unwrap();
}

#[tokio::test]
async fn run_reports_param_mismatch_against_a_running_different_combo() {
    let dir = tempfile::tempdir().unwrap();
    write_procs_with_params(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();
    let root = dir.path().display().to_string();

    client.post(format!("{base}/procs/test:job/start")).bearer_auth("secret").send().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    // Posting the already-rendered CHROME cmd with no explicit `params`
    // object, while the command is running on its default `web-server`
    // combo: nothing written, nothing restarted.
    let r = client
        .post(format!("{base}/run"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "root": root, "cmd": "set CHOICE=-d chrome & ping -n 30 127.0.0.1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["id"], "test:job");
    let mismatch = &body["param_mismatch"];
    assert_eq!(mismatch["running"]["device"], "web-server");
    assert_eq!(mismatch["requested"]["device"], "chrome");

    let procs: Vec<serde_json::Value> = client
        .get(format!("{base}/procs"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let job = procs.iter().find(|p| p["id"] == "test:job").unwrap();
    assert_eq!(job["resolved_cmd"], "set CHOICE=-d web-server & ping -n 30 127.0.0.1", "the running combo must be untouched");

    let _ = client.post(format!("{base}/procs/test:job/stop")).bearer_auth("secret").send().await.unwrap();
}

#[tokio::test]
async fn procs_payload_includes_params_and_resolved_cmd() {
    let dir = tempfile::tempdir().unwrap();
    write_procs_with_params(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let procs: Vec<serde_json::Value> = client
        .get(format!("{base}/procs"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let templated = procs.iter().find(|p| p["id"] == "test:job").unwrap();
    assert_eq!(templated["params"][0]["name"], "device");
    assert_eq!(templated["resolved_cmd"], "set CHOICE=-d web-server & ping -n 30 127.0.0.1");

    let plain = procs.iter().find(|p| p["id"] == "test:job2").unwrap();
    assert_eq!(plain["params"], serde_json::json!([]), "an untemplated command still carries the (empty) params key");
    assert!(plain["resolved_cmd"].is_null(), "an untemplated command's resolved_cmd must be absent/null");
}

#[tokio::test]
async fn add_command_rejects_a_param_named_port() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path()); // project "test" exists
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    let r = client
        .post(format!("{base}/projects/test/commands"))
        .bearer_auth("secret")
        .json(&serde_json::json!({
            "name": "bad",
            "cmd": "echo {PORT}",
            "params": [{ "name": "port", "label": "Port", "values": [{ "value": "a", "label": "a", "flag": "" }] }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "a param named \"port\" must be rejected (would shadow {{PORT}})");
}

#[tokio::test]
async fn update_command_params_none_keeps_some_empty_clears() {
    let dir = tempfile::tempdir().unwrap();
    write_procs_with_params(dir.path());
    let base = spawn_api("secret", dir.path()).await;
    let client = reqwest::Client::new();

    // Omitted `params` key: the full-replace PATCH must keep the existing ones.
    let kept: serde_json::Value = client
        .patch(format!("{base}/projects/test/commands/job"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "name": "job", "cmd": "set CHOICE={DEVICE} & ping -n 30 127.0.0.1" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(kept["params"].as_array().unwrap().len(), 1, "omitted params must keep the existing ones");

    // Explicit empty array: clears them.
    let cleared: serde_json::Value = client
        .patch(format!("{base}/projects/test/commands/job"))
        .bearer_auth("secret")
        .json(&serde_json::json!({ "name": "job", "cmd": "set CHOICE={DEVICE} & ping -n 30 127.0.0.1", "params": [] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cleared["params"], serde_json::json!([]), "an explicit empty array must clear params");
}
