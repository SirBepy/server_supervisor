# Supervised server-run Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a single composite `POST /run` endpoint so a Claude instance can register-and-start a project's long-lived dev server through the supervisor in one call, plus a global `supervised-run` skill that uses it (falling back to a normal shell run when the supervisor is unreachable).

**Architecture:** The HTTP layer stays thin: a new `Supervisor::ensure_and_run` method composes the already-idempotent `add_project` (path dedup) + `add_command` (cmd dedup) + `start`, returns the resulting `ProcInfo`. The `POST /run` handler just parses the body and calls it. The skill is prose that discovers the token/port, probes `/health`, and POSTs `/run`.

**Tech Stack:** Rust (axum, serde, tokio) backend, reqwest in tests, a Markdown skill file in `~/.claude/skills/`.

**Commit convention:** This repo's rule is that subagents STAGE only and never commit; the main agent runs the `/commit` skill after each task's report-back. Each "Commit" step below means: stage the named files, then the main agent commits.

**Note on test flakiness:** Integration tests spawn a real node server on a dynamic port in `42000..49000`. If another project (e.g. `zng-api`) is holding that block on IPv6, a test can flake with `EADDRINUSE` (a known pre-existing IPv4-only-probe gap, `ai_todos/0003`). Re-run once; if it then passes, it's environmental.

---

## Task 1: `derive_name` helper

Derives a command's display name when the caller doesn't supply one. `npm run X` / `pnpm run X` / `yarn run X` / `yarn X` → `X`; otherwise the trimmed command.

**Files:**
- Modify: `src-tauri/src/supervisor/registry.rs` (add a module-private fn + an inline test module, mirroring how `ports.rs` keeps its own `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test.** At the very bottom of `registry.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::derive_name;

    #[test]
    fn derive_name_handles_runners_and_fallback() {
        assert_eq!(derive_name("npm run dev"), "dev");
        assert_eq!(derive_name("pnpm run build"), "build");
        assert_eq!(derive_name("yarn run start"), "start");
        assert_eq!(derive_name("yarn dev"), "dev");
        assert_eq!(derive_name("node server.js"), "node server.js");
        assert_eq!(derive_name("  flutter run  "), "flutter run");
    }
}
```

- [ ] **Step 2: Run it, verify it fails.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib derive_name`
Expected: FAIL - `cannot find function derive_name`.

- [ ] **Step 3: Implement `derive_name`.** Add this free function near the bottom of `registry.rs` (above the `#[cfg(test)]` module, alongside the other module-private helpers like `same_path`):

```rust
/// Short display name for a command: `npm|pnpm|yarn run X` and `yarn X` -> `X`,
/// otherwise the trimmed command string. Used when `POST /run` omits a name.
fn derive_name(cmd: &str) -> String {
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    match toks.as_slice() {
        [runner, "run", x, ..] if *runner == "npm" || *runner == "pnpm" || *runner == "yarn" => {
            x.to_string()
        }
        ["yarn", x, ..] => x.to_string(),
        _ => cmd.trim().to_string(),
    }
}
```

- [ ] **Step 4: Run it, verify it passes.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib derive_name`
Expected: PASS (1 test).

- [ ] **Step 5: Commit.** Stage `src-tauri/src/supervisor/registry.rs`. Main agent commits (suggested message: `FEAT: derive_name helper for command display names`).

---

## Task 2: `Supervisor::ensure_and_run`

Composes register-and-start and returns the started unit's `ProcInfo`. Idempotent via the existing dedup in `add_project` (canonical path) and `add_command` (exact cmd).

**Files:**
- Modify: `src-tauri/src/supervisor/registry.rs` (add a public method on `impl Supervisor`)
- Test: `src-tauri/tests/supervisor_test.rs` (add an integration test; reuse the existing `new_sup` helper)

- [ ] **Step 1: Write the failing test.** Append to `src-tauri/tests/supervisor_test.rs`. (Check the top of the file for the existing imports + `new_sup` helper; add `use server_supervisor_lib::types::{ProcKind, ProcStatus};` if not already imported.)

```rust
#[test]
fn ensure_and_run_registers_starts_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("server.js"),
        "const p=process.env.PORT;require('http').createServer((_,r)=>r.end('ok')).listen(p,()=>console.log('LISTENING '+p));",
    )
    .unwrap();
    let sup = new_sup(dir.path());
    let root = dir.path().to_str().unwrap();

    let info = sup
        .ensure_and_run(root, "node server.js", None, ProcKind::Generic, true)
        .unwrap();
    assert_eq!(info.status, ProcStatus::Running);
    let port = info.port.expect("dynamic port should be assigned");
    assert!((42000..49000).contains(&port));

    // Idempotent: same root+cmd reuses the same project/command (no duplicate).
    let info2 = sup
        .ensure_and_run(root, "node server.js", None, ProcKind::Generic, true)
        .unwrap();
    assert_eq!(info2.id, info.id);
    assert_eq!(sup.list().len(), 1, "no duplicate registration");

    sup.stop(&info.id).unwrap();
}
```

Note: confirm `new_sup` returns something you can call `.ensure_and_run` / `.stop` / `.list` on (it returns a `Supervisor`, not an `Arc` - if it returns `Arc<Supervisor>`, that's fine, methods deref through). If `new_sup` takes a different argument shape, match it (it currently takes `dir.path()`).

- [ ] **Step 2: Run it, verify it fails.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test supervisor_test ensure_and_run`
Expected: FAIL - `no method named ensure_and_run`.

- [ ] **Step 3: Implement `ensure_and_run`.** Add to `impl Supervisor` in `registry.rs` (place it near `add_command`/`start`). It uses the exact existing signatures: `add_project(&self, name: String, root: String) -> Result<Project, String>`, `add_command(&self, project_id: &str, name: String, cmd: String, kind: ProcKind, autostart: bool, use_dynamic_port: bool) -> Result<Command, String>`, `start(&self, id: &str) -> Result<(), String>`, `list(&self) -> Vec<ProcInfo>`, and the existing `unit_id` from `crate::types`.

```rust
/// Register a project (by folder) + a command (by cmd string) if not already
/// present - both are idempotent - then start it and return its ProcInfo.
/// The composite used by the `POST /run` API for one-call server launch.
pub fn ensure_and_run(
    &self,
    root: &str,
    cmd: &str,
    name: Option<String>,
    kind: ProcKind,
    use_dynamic_port: bool,
) -> Result<ProcInfo, String> {
    let project_name = std::path::Path::new(root)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(root)
        .to_string();
    let project = self.add_project(project_name, root.to_string())?;
    let command_name = name.unwrap_or_else(|| derive_name(cmd));
    let command = self.add_command(
        &project.id,
        command_name,
        cmd.to_string(),
        kind,
        false,
        use_dynamic_port,
    )?;
    let id = unit_id(&project.id, &command.id);
    self.start(&id)?;
    self.list()
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("started but not found in list: {id}"))
}
```

Confirm `ProcInfo`, `Project`, `Command`, `ProcKind`, `unit_id` are already imported at the top of `registry.rs` (they are - `use crate::types::{unit_id, Command, LogLine, ProcInfo, ProcKind, ProcSpec, Project};`). No new imports needed.

- [ ] **Step 4: Run it, verify it passes.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test supervisor_test ensure_and_run`
Expected: PASS. (If `EADDRINUSE` flake, re-run once - see the flakiness note above.)

- [ ] **Step 5: Run the full suite + orphan check.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all green. Then verify no orphan node servers: `Get-CimInstance Win32_Process -Filter "Name='node.exe'" | Where-Object { $_.CommandLine -match 'server.js' }` - kill any leftover with `Stop-Process -Id <PID> -Force`.

- [ ] **Step 6: Commit.** Stage `src-tauri/src/supervisor/registry.rs` + `src-tauri/tests/supervisor_test.rs`. Main agent commits (suggested: `FEAT: Supervisor::ensure_and_run composes register + start`).

---

## Task 3: `POST /run` endpoint

Thin HTTP wrapper over `ensure_and_run`, behind the same bearer auth as `/procs`.

**Files:**
- Modify: `src-tauri/src/api.rs` (add `RunBody`, the `run` handler, the route, and a `ProcKind` import)
- Test: `src-tauri/tests/api_test.rs` (add an auth + smoke + idempotency test)

- [ ] **Step 1: Write the failing test.** Append to `src-tauri/tests/api_test.rs` (it already imports what's needed and has the `spawn_api` helper + `reqwest`):

```rust
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
```

- [ ] **Step 2: Run it, verify it fails.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test api_test run_registers`
Expected: FAIL - the `/run` route 404s (so `info["id"]` unwrap panics / status isn't as asserted).

- [ ] **Step 3: Add the import.** In `src-tauri/src/api.rs`, change the types import from `use crate::types::ProcInfo;` to `use crate::types::{ProcInfo, ProcKind};`.

- [ ] **Step 4: Add the request struct.** In `api.rs`, next to the existing `ReserveBody` struct, add:

```rust
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
```

- [ ] **Step 5: Add the handler.** In `api.rs`, near the other handlers (e.g. after `reserve_port`):

```rust
async fn run(State(s): State<ApiState>, Json(b): Json<RunBody>) -> Response {
    match s.sup.ensure_and_run(
        &b.root,
        &b.cmd,
        b.name,
        b.kind.unwrap_or(ProcKind::Generic),
        b.use_dynamic_port.unwrap_or(true),
    ) {
        Ok(info) => Json(info).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}
```

(`Response`, `StatusCode`, `IntoResponse`, `Json`, `State` are already imported in `api.rs` - the existing `unit_result`/`list_procs` handlers use them.)

- [ ] **Step 6: Register the route.** In the `router(...)` function in `api.rs`, add the `/run` route alongside the others that sit BEFORE the `.route_layer(middleware::from_fn_with_state(state.clone(), auth))` line (so it requires the bearer token, NOT after the layer where `/health` lives):

```rust
        .route("/run", post(run))
```

- [ ] **Step 7: Update the security doc comment.** At the top of `api.rs`, extend the module comment to note the new capability:

```rust
//! ... existing comment ... The `/run` endpoint additionally lets an authorized
//! caller register and start an arbitrary command (define-and-run), so the
//! loopback-only + bearer-token constraints are doubly important.
```

- [ ] **Step 8: Run the test, verify it passes.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test api_test run_registers`
Expected: PASS. (Re-run once on an `EADDRINUSE` flake.)

- [ ] **Step 9: Full suite + orphan check + ts types.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all green (lib incl. `derive_name`, supervisor incl. `ensure_and_run`, api incl. the new test).
`RunBody` is internal (no ts-rs export needed). Confirm no orphan node servers as in Task 2 Step 5.

- [ ] **Step 10: Commit.** Stage `src-tauri/src/api.rs` + `src-tauri/tests/api_test.rs`. Main agent commits (suggested: `FEAT: POST /run - register-and-start a server in one authed call`).

---

## Task 4: The `supervised-run` skill

A global skill (prose, no automated test) that teaches any Claude instance to launch long-lived servers through the supervisor.

**Files:**
- Create: `C:\Users\tecno\.claude\skills\supervised-run\SKILL.md` (Joe's global config, OUTSIDE this repo - it is not committed here)

- [ ] **Step 1: Write the skill file.** Create `C:\Users\tecno\.claude\skills\supervised-run\SKILL.md` with exactly:

```markdown
---
name: supervised-run
description: Use when you need to start a LONG-LIVED dev server / watcher for the current project (e.g. npm run dev, vite, next dev, flutter run, a backend that stays running). Routes it through the local server_supervisor app so it is visible in Joe's dashboard, owned (no orphans), and centrally logged - auto-registering it if needed. Do NOT use for one-off commands that exit (tests, builds, git, scripts); run those normally. Falls back to a normal shell run if the supervisor is not reachable.
---

# supervised-run

Start a long-lived dev server through server_supervisor instead of spawning it in your own shell.

## When this applies

- The command is a server / watcher that STAYS RUNNING (dev server, API, file watcher).
- NOT one-off commands that exit (tests, builds, lint, git) - run those normally.

## Steps

1. **Discover the API.** Read the data dir `%APPDATA%\com.sirbepy.server-supervisor\supervisor\`:
   - token = contents of `api_token.txt`
   - port = contents of `api_port.txt`
   If either file is missing, treat the supervisor as not running → go to Fallback.

2. **Probe health.** `GET http://127.0.0.1:<port>/health` (no auth). If it does not return 200 (connection refused, timeout, missing) → go to Fallback.

3. **Run it.** `POST http://127.0.0.1:<port>/run` with header `Authorization: Bearer <token>` and JSON body:
   ```json
   { "root": "<absolute path of the current project folder>", "cmd": "<the server command>", "kind": "generic", "use_dynamic_port": true }
   ```
   - Set `"kind": "flutter"` only for `flutter run` commands; otherwise `"generic"`.
   - For the dynamic port to actually take effect, template the port flag INTO the command where the tool supports one, using the literal `{PORT}` placeholder (the supervisor substitutes it and also sets the `PORT` env var):
     - Vite: `vite --port {PORT}` (or `npm run dev -- --port {PORT}`)
     - Next: `next dev -p {PORT}`
     - Flutter web: `flutter run -d chrome --web-port {PORT}`
     - Node servers reading `process.env.PORT`: no `{PORT}` needed; the env var is set automatically.
     - If you cannot make the tool honor a port, send `"use_dynamic_port": false` and accept its built-in port.
   - The response is the started process's info: `{ id, project, name, kind, status, pid, port }`.

4. **Report.** Tell Joe it's running, on which port, and that it's in the supervisor dashboard. Calling `/run` again with the same root+cmd is safe - it reuses the same entry and restarts it (no duplicates).

5. **Manage it afterward** via the same base URL + bearer token:
   - Logs: `GET /procs/<id>/logs`
   - Stop: `POST /procs/<id>/stop`
   - Restart: `POST /procs/<id>/restart`
   - List everything running: `GET /procs`

## Fallback (supervisor not reachable)

Run the server the normal way (in your own background shell), and tell Joe: "server_supervisor isn't running, so I ran <cmd> directly." Never block on the supervisor being up. Do NOT try to launch the supervisor app yourself.

## Notes

- The API binds 127.0.0.1 only and the token is per-machine; never send it anywhere off-localhost.
- One-off commands never go through here - this is only for processes that stay running.
```

- [ ] **Step 2: Verify the skill is well-formed.** Confirm the frontmatter `name`/`description` are present and the file is at `C:\Users\tecno\.claude\skills\supervised-run\SKILL.md`. (No automated test - it's instructions. Joe validates by triggering it in a real project.)

- [ ] **Step 3: No commit here.** This file lives in Joe's global config, not this repo. Tell Joe it's written so he can manage it in his dotfiles.

---

## Self-Review

- **Spec coverage:** `POST /run` composite (Task 3) ✓; idempotent register via existing dedup (Tasks 2/3 assert it) ✓; returns ProcInfo with port (Task 2/3) ✓; auth-gated (Task 3 Step 6, asserted Step 1) ✓; `derive_name` default command name (Task 1) ✓; dynamic-port default true + `{PORT}` templating guidance (Task 3 handler default + skill Step 3) ✓; skill discover→health→run→report→fallback (Task 4) ✓; long-lived-only scope + shell fallback + no auto-launch (skill description + Fallback) ✓; security note (Task 3 Step 7) ✓.
- **Placeholders:** none - every code step has complete code; the only `<...>` are runtime values the skill fills at use time (project path, token, port), which is correct for prose instructions.
- **Type consistency:** `ensure_and_run(root: &str, cmd: &str, name: Option<String>, kind: ProcKind, use_dynamic_port: bool) -> Result<ProcInfo, String>` is defined in Task 2 and called identically by the handler in Task 3. `RunBody` field names (`root`, `cmd`, `name`, `kind`, `use_dynamic_port`) match the JSON bodies in the Task 3 test and the skill. `derive_name(&str) -> String` defined in Task 1, used in Task 2.

## Non-goals (do not build)

- Granular project/command CRUD over HTTP (update/remove/list-projects).
- Auto-launching the supervisor app.
- Routing one-off/exiting commands through the supervisor.
