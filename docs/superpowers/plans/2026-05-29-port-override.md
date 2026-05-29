# Port Allocator + Override Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let server_supervisor run a project's dev server on its own clash-free port (e.g. 42001) without editing any project file, opt-in per command, while remembering reserved ports so projects never collide.

**Architecture:** Two separate mechanisms. (1) A **persistent ledger** (`ports.json`) of *reserved* ports that must never be auto-handed-out: seeds `7716` (server_supervisor's own vite) + `1420` (blocked common Tauri default) + always-on apps explicitly reserved (e.g. claude_usage = 42000). Survives restarts. (2) **Ephemeral per-run acquisition**: when a command has `useDynamicPort: true`, the supervisor acquires the lowest free port >= 42000 that is neither reserved (ledger) nor currently acquired (in-memory) nor OS-bound, injects it two ways (substitute `{PORT}` in the command string AND set `PORT=<n>` in the child env), then frees it when the process exits. Ephemeral acquisitions are in-memory only (rebuilt empty each start). Never edit project files.

**Tech Stack:** Rust (Tauri 2 backend), `std::process::Command` via `cmd /C`, `std::net::TcpListener` for free-port probing, `netstat`/`tasklist` for "who holds a port", ts-rs for Rust→TS types, vanilla TS + lit-html frontend.

---

## Current state (read before starting)

Repo: `C:\Users\tecno\Desktop\Projects\server_supervisor`. Windows, PowerShell 5.1. Build/test the Rust with:
`cargo test --manifest-path src-tauri/Cargo.toml` (first build is slow; the app exe must NOT be running or the link step fails with "Access is denied" — quit the tray app first).

Key existing files and shapes (Read them; do not assume):
- `src-tauri/src/types.rs` — `ProcKind { Generic, Flutter }`, `ProcStatus`, `Command { id, name, cmd, kind, autostart }`, `Project { id, name, root, commands: Vec<Command> }`, `ProcInfo { id, project, name, kind, status, pid }`, `ProcSpec { id, project, name, cmd, cwd, kind, autostart }` + `ProcSpec::from_unit(&Project, &Command)`, `unit_id(pid, cid)` (joins with `:`).
- `src-tauri/src/supervisor/proc.rs` — `ManagedProc { spec: ProcSpec, status, pid, started_at, child: Option<Child>, stdin: Option<ChildStdin>, logs, app_id }`. `start()` builds `Command::new("cmd").arg("/C").arg(&self.spec.cmd).current_dir(&self.spec.cwd)` with piped stdio + `CREATE_NEW_PROCESS_GROUP`, spawns readers, sets status Running. `stop()` tree-kills via `super::reaper::kill_tree(pid)`. `reload(full)` is flutter-only.
- `src-tauri/src/supervisor/registry.rs` — `Supervisor { projects: Mutex<Vec<Project>>, procs: Mutex<HashMap<String, ManagedProc>>, data_dir }`. `start(id)`/`stop(id)`/`restart(id)`/`reload(id,full)`/`list()`/`logs(id)` operate on the procs map by composite id. `list()` calls `p.refresh()` (detects self-exit) then `p.info()`.
- `src-tauri/src/ports.rs` — **EXISTS but is the wrong model and is NOT yet wired into `lib.rs`** (no `pub mod ports;`). It currently has `PortRegistry { entries: Mutex<Vec<PortEntry>>, data_dir }` with `new` (seeds 7716 + 1420), `list`, `reserve(owner,port,note)`, `allocate(owner)` (persistent + idempotent-per-owner), `port_free`, `load`/`save`, and tests. **Task 1 rewrites it** to the reserved-vs-acquired split below.
- `src-tauri/src/ipc/commands.rs` — Tauri commands take `State<Arc<Supervisor>>`. Pattern: `#[tauri::command] pub fn x(sup: State<Arc<Supervisor>>, ...) -> ...`.
- `src-tauri/src/api.rs` — axum router; `ApiState { sup: Arc<Supervisor>, token }`; `router(sup, token)`; bearer-auth middleware; routes like `/procs`, `/procs/:id/start`.
- `src-tauri/src/lib.rs` — `setup` builds `data_dir = app_data_dir()/"supervisor"`, `let supervisor = Arc::new(Supervisor::new(data_dir.clone()))`, spawns the API task with `api::serve(supervisor.clone(), port, token)`, `handle.manage(supervisor)`. `invoke_handler![...]` lists all commands.
- `src-tauri/tests/export_types.rs` — composes ts-rs decls into `src/types/ipc.generated.ts`. Add new types here.
- `src/shared/ipc.ts`, `src/views/dashboard/dashboard.ts` — frontend; `ProcInfo` already rendered grouped by project.

**Uncommitted baseline already in the tree (do not undo):** `vite.config.ts` dev port changed `1420 -> 7716`; `src-tauri/src/ports.rs` created (old model). These are committed as the baseline for this plan.

Commit policy: this repo imports full-auto, so commit after each task. Use the project's normal style (`FEAT:`/`FIX:`/`REFACTOR:`/`TEST:`). NEVER edit any file outside this repo except the one explicit step in Task 8 (claude_usage's own config — separate human-confirmed action).

---

## File Structure

- Modify `src-tauri/src/ports.rs` — reserved ledger + ephemeral acquire/release.
- Modify `src-tauri/src/types.rs` — `Command.use_dynamic_port`, `ProcInfo.port`, carry through `ProcSpec`.
- Modify `src-tauri/src/supervisor/proc.rs` — apply port override (substitute + env) at spawn; track acquired port; expose it on `ProcInfo`.
- Modify `src-tauri/src/supervisor/registry.rs` — `Supervisor` holds `Arc<PortRegistry>`; acquire-before-start, release-on-stop/exit; probe + reallocate.
- Modify `src-tauri/src/lib.rs` — build/manage `PortRegistry`, pass into `Supervisor::new`, register IPC.
- Modify `src-tauri/src/ipc/commands.rs` + `src-tauri/src/api.rs` — `list_ports`, `reserve_port` commands + `/ports` routes.
- Modify `src-tauri/tests/export_types.rs` — export `PortEntry`.
- Add `src-tauri/tests/ports_override_test.rs` — integration tests incl. node-binds-env-port.

---

## Task 1: PortRegistry — reserved ledger + ephemeral acquire

**Files:**
- Modify: `src-tauri/src/ports.rs` (full rewrite of the impl; keep `PortEntry`, `BASE`, `MAX`, `port_free`, `load`, `save`)

- [ ] **Step 1: Write failing tests** (replace the existing `#[cfg(test)] mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_block_1420_and_records_7716() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let reserved: Vec<u16> = reg.list().iter().map(|e| e.port).collect();
        assert!(reserved.contains(&7716));
        assert!(reserved.contains(&1420));
    }

    #[test]
    fn reserve_persists_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        {
            let reg = PortRegistry::new(dir.path().to_path_buf());
            let p = reg.reserve_next("always-on-app");
            assert!((BASE..MAX).contains(&p));
            assert_eq!(reg.reserve_next("always-on-app"), p, "idempotent per owner");
        }
        // survives reload
        let reg2 = PortRegistry::new(dir.path().to_path_buf());
        assert!(reg2.list().iter().any(|e| e.owner == "always-on-app"));
    }

    #[test]
    fn acquire_skips_reserved_and_is_ephemeral() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let reserved = reg.reserve_next("app"); // takes BASE (42000)
        let a = reg.acquire().unwrap();
        let b = reg.acquire().unwrap();
        assert_ne!(a, reserved);
        assert_ne!(a, b, "two live acquisitions differ");
        // acquisitions are in-memory only: not written to ports.json
        let saved = std::fs::read_to_string(dir.path().join("ports.json")).unwrap();
        assert!(!saved.contains(&a.to_string()), "ephemeral ports must not persist");
        reg.release(a);
        let c = reg.acquire().unwrap();
        assert_eq!(c, a, "released port is reusable");
    }
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib ports`
Expected: FAIL (`reserve_next` / `acquire` / `release` not found).

- [ ] **Step 3: Rewrite the `PortRegistry` impl**

Keep the top of the file (`PortEntry`, `BASE: u16 = 42000`, `MAX: u16 = 49000`, `FILE`, `port_free`, `load`, `save`). Replace the `struct PortRegistry` + `impl` with:

```rust
pub struct PortRegistry {
    /// Persistent reserved ports (seeds + always-on apps). Written to ports.json.
    reserved: Mutex<Vec<PortEntry>>,
    /// Ephemeral per-run acquisitions, in-memory only, freed on process exit.
    acquired: Mutex<std::collections::HashSet<u16>>,
    data_dir: PathBuf,
}

impl PortRegistry {
    pub fn new(data_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&data_dir);
        let reg = Self {
            reserved: Mutex::new(load(&data_dir)),
            acquired: Mutex::new(std::collections::HashSet::new()),
            data_dir,
        };
        reg.reserve("server_supervisor", 7716, "vite dev (self)");
        reg.reserve("_blocked_default", 1420, "common Tauri default - never assign");
        reg
    }

    pub fn list(&self) -> Vec<PortEntry> {
        self.reserved.lock().unwrap().clone()
    }

    /// Record an exact port for an owner (idempotent on port). Persistent.
    pub fn reserve(&self, owner: &str, port: u16, note: &str) {
        let mut g = self.reserved.lock().unwrap();
        if g.iter().any(|e| e.port == port) {
            return;
        }
        g.push(PortEntry { owner: owner.into(), port, note: note.into() });
        save(&self.data_dir, &g);
    }

    /// Reserve a fresh persistent port for an always-on app (idempotent per owner).
    /// Returns the owner's existing reserved port, else the lowest free >= BASE.
    pub fn reserve_next(&self, owner: &str) -> u16 {
        {
            let g = self.reserved.lock().unwrap();
            if let Some(e) = g.iter().find(|e| e.owner == owner) {
                return e.port;
            }
        }
        let taken = self.taken_set();
        let port = (BASE..MAX).find(|p| !taken.contains(p)).unwrap_or(BASE);
        self.reserve(owner, port, "always-on app");
        port
    }

    /// Acquire an ephemeral per-run port: lowest free >= BASE not reserved, not
    /// already acquired, not OS-bound. Held in-memory until `release`.
    pub fn acquire(&self) -> Result<u16, String> {
        let taken = self.taken_set();
        let mut acq = self.acquired.lock().unwrap();
        for p in BASE..MAX {
            if taken.contains(&p) || acq.contains(&p) {
                continue;
            }
            if !port_free(p) {
                continue;
            }
            acq.insert(p);
            return Ok(p);
        }
        Err(format!("no free port available in {BASE}..{MAX}"))
    }

    pub fn release(&self, port: u16) {
        self.acquired.lock().unwrap().remove(&port);
    }

    fn taken_set(&self) -> std::collections::HashSet<u16> {
        let mut set: std::collections::HashSet<u16> =
            self.reserved.lock().unwrap().iter().map(|e| e.port).collect();
        set.extend(self.acquired.lock().unwrap().iter().copied());
        set
    }
}
```

Note: `reserve_next` releases its `reserved` lock before calling `taken_set()` (which re-locks) to avoid deadlock — already structured that way above.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib ports`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add src-tauri/src/ports.rs
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "REFACTOR: split port registry into reserved ledger + ephemeral acquire"
```

---

## Task 2: Wire PortRegistry into lib.rs and Supervisor

**Files:**
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/supervisor/registry.rs`

- [ ] **Step 1: `Supervisor` holds the registry.** In `registry.rs`, add the field + param:

```rust
use crate::ports::PortRegistry;
use std::sync::Arc;

pub struct Supervisor {
    projects: Mutex<Vec<Project>>,
    procs: Mutex<HashMap<String, ManagedProc>>,
    data_dir: PathBuf,
    ports: Arc<PortRegistry>,
}

impl Supervisor {
    pub fn new(data_dir: PathBuf, ports: Arc<PortRegistry>) -> Self {
        let _ = std::fs::create_dir_all(&data_dir);
        let projects = config::load(&data_dir);
        let mut map = HashMap::new();
        for project in &projects {
            ensure_procs(&mut map, project);
        }
        Self { projects: Mutex::new(projects), procs: Mutex::new(map), data_dir, ports }
    }

    pub fn ports(&self) -> &Arc<PortRegistry> {
        &self.ports
    }
    // ... existing methods unchanged for now ...
}
```

- [ ] **Step 2: Build + manage in `lib.rs` setup.** Add `pub mod ports;` to the module list at top. In `setup`, before constructing the supervisor:

```rust
let ports = std::sync::Arc::new(ports::PortRegistry::new(data_dir.clone()));
let supervisor = std::sync::Arc::new(supervisor::Supervisor::new(data_dir.clone(), ports.clone()));
// ... existing reconcile_orphans / start_autostart / api::serve / handle.manage(supervisor) ...
handle.manage(ports);
```

- [ ] **Step 3: Build**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: compiles (no callers broke — `Supervisor::new` now needs the `ports` arg; the only caller is `lib.rs` setup, updated above, plus tests in Task 4+ which you will update).

- [ ] **Step 4: Fix existing tests' `Supervisor::new` calls.** `src-tauri/tests/supervisor_test.rs` and `src-tauri/tests/api_test.rs` call `Supervisor::new(dir)`. Update each to:

```rust
use server_supervisor_lib::ports::PortRegistry;
use std::sync::Arc;
// ...
let sup = Supervisor::new(dir.path().to_path_buf(), Arc::new(PortRegistry::new(dir.path().to_path_buf())));
```

(In `api_test.rs`'s `spawn_api`, build the registry the same way before `Supervisor::new`.)

- [ ] **Step 5: Run full suite + commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all existing tests PASS.

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add src-tauri/src/lib.rs src-tauri/src/supervisor/registry.rs src-tauri/tests/supervisor_test.rs src-tauri/tests/api_test.rs
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "FEAT: manage PortRegistry and give Supervisor a handle to it"
```

---

## Task 3: Config + view fields (useDynamicPort, ProcInfo.port)

**Files:**
- Modify: `src-tauri/src/types.rs`
- Modify: `src-tauri/tests/export_types.rs`

- [ ] **Step 1: Add fields.** In `types.rs`:

`Command` gains:
```rust
    #[serde(default)]
    pub use_dynamic_port: bool,
```
`ProcSpec` gains (so the runtime spec carries it):
```rust
    #[serde(default)]
    pub use_dynamic_port: bool,
```
`ProcInfo` gains:
```rust
    pub port: Option<u16>,
```
Update `ProcSpec::from_unit` to set `use_dynamic_port: command.use_dynamic_port`.

- [ ] **Step 2: Export the type + new shape.** In `tests/export_types.rs`, add `use server_supervisor_lib::ports::PortEntry;` and `out.push_str(&decl::<PortEntry>());`. (`Command`, `ProcSpec`, `ProcInfo` already exported — they regenerate with the new fields.)

- [ ] **Step 3: Update `ManagedProc::info()`** in `proc.rs` to include the port (placeholder until Task 4 tracks it):
```rust
            port: self.acquired_port,
```
(You will add the `acquired_port` field in Task 4; for now, if doing strict TDD, add the field in Task 4 and this line compiles then. To keep Task 3 compiling standalone, temporarily use `port: None` and change to `self.acquired_port` in Task 4.)

- [ ] **Step 4: Run + commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test export_types`
Expected: PASS; `src/types/ipc.generated.ts` now shows `use_dynamic_port` on Command/ProcSpec and `port: number | null` on ProcInfo.

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add src-tauri/src/types.rs src-tauri/src/supervisor/proc.rs src-tauri/tests/export_types.rs
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "FEAT: add useDynamicPort config field and ProcInfo.port"
```

---

## Task 4: Apply the port override at spawn (the core)

**Files:**
- Modify: `src-tauri/src/supervisor/proc.rs`
- Modify: `src-tauri/src/supervisor/registry.rs`
- Test: `src-tauri/tests/ports_override_test.rs` (create)

- [ ] **Step 1: Write failing integration test** (create `ports_override_test.rs`)

```rust
//! Proves the port override actually reaches a real server: a tiny node script
//! that binds process.env.PORT, started via the supervisor with useDynamicPort,
//! ends up listening on the acquired port.

use server_supervisor_lib::ports::PortRegistry;
use server_supervisor_lib::supervisor::Supervisor;
use std::sync::Arc;
use std::time::Duration;

fn write_node_project(dir: &std::path::Path) {
    let root = dir.display().to_string().replace('\\', "/");
    // Server prints the port it bound so we can assert from logs.
    std::fs::write(
        dir.join("server.js"),
        "const p=process.env.PORT;require('http').createServer((_,r)=>r.end('ok')).listen(p,()=>console.log('LISTENING '+p));",
    )
    .unwrap();
    let json = format!(
        r#"[{{"id":"np","name":"np","root":"{root}","commands":[{{"id":"web","name":"web","cmd":"node server.js","kind":"generic","autostart":false,"use_dynamic_port":true}}]}}]"#
    );
    std::fs::write(dir.join("projects.json"), json).unwrap();
}

#[test]
fn dynamic_port_env_reaches_node_server() {
    let dir = tempfile::tempdir().unwrap();
    write_node_project(dir.path());
    let ports = Arc::new(PortRegistry::new(dir.path().to_path_buf()));
    let sup = Supervisor::new(dir.path().to_path_buf(), ports);

    sup.start("np:web").unwrap();
    std::thread::sleep(Duration::from_millis(1800));

    let info = sup.list().into_iter().find(|p| p.id == "np:web").unwrap();
    let port = info.port.expect("a dynamic port should be assigned");
    assert!((42000..49000).contains(&port));

    let logs = sup.logs("np:web").unwrap();
    let bound = logs.iter().any(|l| l.text.contains(&format!("LISTENING {port}")));
    assert!(bound, "node server should bind the injected PORT env; logs: {logs:?}");

    sup.stop("np:web").unwrap();
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test ports_override_test`
Expected: FAIL (`info.port` is None / server binds nothing).

- [ ] **Step 3: Track the acquired port on `ManagedProc`.** In `proc.rs`, add field `acquired_port: Option<u16>` (init `None` in `new`), set `info().port = self.acquired_port`, and change `start()` to accept an optional port + apply both channels:

```rust
pub fn start(&mut self, dynamic_port: Option<u16>) -> std::io::Result<u32> {
    self.refresh();
    if matches!(self.status, ProcStatus::Running | ProcStatus::Starting) {
        return Ok(self.pid.unwrap_or(0));
    }
    // Apply the port override (no project files touched).
    let cmd_str = match dynamic_port {
        Some(p) => self.spec.cmd.replace("{PORT}", &p.to_string()),
        None => self.spec.cmd.clone(),
    };
    let mut command = Command::new("cmd");
    command.arg("/C").arg(&cmd_str).current_dir(&self.spec.cwd)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(p) = dynamic_port {
        command.env("PORT", p.to_string()); // env channel for process.env.PORT tools
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    let mut child = command.spawn()?;
    let pid = child.id();
    *self.app_id.lock().unwrap() = None;
    if let Some(out) = child.stdout.take() { spawn_reader(out, "stdout", self.logs.clone(), Some(self.app_id.clone())); }
    if let Some(err) = child.stderr.take() { spawn_reader(err, "stderr", self.logs.clone(), None); }
    self.stdin = child.stdin.take();
    self.acquired_port = dynamic_port;
    self.push_log("stdout", format!("[supervisor] started: {cmd_str}"));
    self.child = Some(child);
    self.pid = Some(pid);
    self.started_at = Some(now_ms());
    self.status = ProcStatus::Running;
    Ok(pid)
}
```
In `stop()`, after killing, add `self.acquired_port = None;`. In `refresh()` (self-exit branch), the registry releases the port (Task 4 step 4) — `ManagedProc` just clears it: add `self.acquired_port = None;` there too, BUT the registry release happens in `Supervisor` (it owns the registry). To let `Supervisor` know which port to release, expose a getter: `pub fn acquired_port(&self) -> Option<u16> { self.acquired_port }`. Keep the field set until the supervisor reads + releases it; clear it inside `stop()` only after the supervisor has released (see step 4).

Simplest ownership: do NOT clear `acquired_port` inside `proc.rs` stop/refresh. The `Supervisor` reads it, calls `ports.release`, then clears. (Step 4.)

- [ ] **Step 4: Supervisor acquires/probes/releases.** In `registry.rs`, replace `start`/`stop` bodies:

```rust
pub fn start(&self, id: &str) -> Result<(), String> {
    // Acquire a probed, free port if this command opts in (before locking procs,
    // since acquire does OS work; reserve-before-spawn prevents concurrent races).
    let want_dynamic = {
        let g = self.procs.lock().unwrap();
        g.get(id).map(|p| p.spec.use_dynamic_port).unwrap_or(false)
    };
    let port = if want_dynamic { Some(self.acquire_free_port(id)?) } else { None };
    let res = {
        let mut g = self.procs.lock().unwrap();
        let p = g.get_mut(id).ok_or_else(|| format!("unknown process id: {id}"))?;
        p.start(port).map_err(|e| e.to_string())
    };
    if res.is_err() {
        if let Some(p) = port { self.ports.release(p); }
    }
    res?;
    self.persist_pids();
    Ok(())
}

pub fn stop(&self, id: &str) -> Result<(), String> {
    let released;
    {
        let mut g = self.procs.lock().unwrap();
        let p = g.get_mut(id).ok_or_else(|| format!("unknown process id: {id}"))?;
        released = p.acquired_port();
        p.stop();
    }
    if let Some(port) = released { self.ports.release(port); }
    self.persist_pids();
    Ok(())
}
```

Add the probe helper (acquire + netstat warn; acquire() already OS-probes via `port_free`, so this mostly logs if somehow taken):

```rust
fn acquire_free_port(&self, id: &str) -> Result<u16, String> {
    let port = self.ports.acquire()?;
    if let Some(holder) = super::reaper::port_holder(port) {
        log::warn!("supervisor: port {port} appears held by {holder} despite probe; using it for {id} anyway");
    }
    Ok(port)
}
```

- [ ] **Step 5: Add `port_holder` to `reaper.rs`** (netstat -> pid -> tasklist name):

```rust
#[cfg(windows)]
pub fn port_holder(port: u16) -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("cmd")
        .args(["/C", &format!("netstat -ano | findstr :{port}")])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let pid: &str = text.split_whitespace().last()?;
    if pid == "0" || pid.is_empty() { return None; }
    let t = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&t.stdout);
    name.split_whitespace().next().map(|s| s.to_string()).filter(|s| s != "INFO:")
}

#[cfg(not(windows))]
pub fn port_holder(_port: u16) -> Option<String> { None }
```

- [ ] **Step 6: `restart`/`reload` callers.** `restart` already calls `stop` then `start` (handled). `reload` does not touch ports.

- [ ] **Step 7: Run the override test + full suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: `dynamic_port_env_reaches_node_server` PASSES (proves env injection reaches a real node server — the 10/10 verification), all others PASS. Requires `node` on PATH.

- [ ] **Step 8: Commit**

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add src-tauri/src/supervisor/proc.rs src-tauri/src/supervisor/registry.rs src-tauri/src/supervisor/reaper.rs src-tauri/tests/ports_override_test.rs
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "FEAT: inject dynamic port via {PORT} substitution + PORT env, free on stop"
```

---

## Task 5: {PORT} substitution test + EADDRINUSE retry-once

**Files:**
- Modify: `src-tauri/src/supervisor/registry.rs`
- Test: `src-tauri/tests/ports_override_test.rs`

- [ ] **Step 1: Add a `{PORT}` flag-substitution test.** Append to `ports_override_test.rs` a project whose `cmd` is `node -e "const p=process.argv[2];require('http').createServer((_,r)=>r.end()).listen(p,()=>console.log('FLAG '+p))" {PORT}` with `use_dynamic_port:true`, start it, assert logs contain `FLAG <port>` where `<port>` equals `info.port`. (Proves `{PORT}` substitution into the command string works independent of env.)

- [ ] **Step 2: Run, verify it passes** (substitution already implemented in Task 4 step 3). If it fails, the bug is in `cmd.replace("{PORT}", ...)`; fix there.

- [ ] **Step 3: EADDRINUSE retry-once (best-effort).** In `registry.rs` `start`, after `p.start(port)` succeeds, spawn a short watchdog that, if the child exits within ~1500ms AND its logs contain `EADDRINUSE`, releases the port and retries once on a fresh acquire. Because `start` is non-blocking and holds no lock after returning, implement as: re-check inside `list()`/a dedicated `recover_failed(id)` is overkill — instead do a bounded inline check:

```rust
// after p.start(port) in start(), still inside start (no lock held):
if let Some(p_port) = port {
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let crashed_addrinuse = {
        let mut g = self.procs.lock().unwrap();
        if let Some(proc) = g.get_mut(id) {
            proc.refresh();
            proc.status == crate::types::ProcStatus::Crashed
                && proc.logs_snapshot().iter().any(|l| l.text.contains("EADDRINUSE"))
        } else { false }
    };
    if crashed_addrinuse {
        self.ports.release(p_port);
        log::warn!("supervisor: {id} hit EADDRINUSE on {p_port}, retrying once");
        let retry = self.acquire_free_port(id)?;
        let mut g = self.procs.lock().unwrap();
        if let Some(proc) = g.get_mut(id) { proc.start(Some(retry)).map_err(|e| e.to_string())?; }
    }
}
```

Note: this makes `start` block ~1500ms for dynamic-port commands. Acceptable (start is user/AI-initiated, not hot-path). If you prefer non-blocking, document that the retry is skipped and rely on probe + warn only — the probe already prevents the common case. Pick blocking-retry for the 9/10.

- [ ] **Step 4: Run full suite + commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all PASS.

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add src-tauri/src/supervisor/registry.rs src-tauri/tests/ports_override_test.rs
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "FEAT: {PORT} flag substitution test + EADDRINUSE retry-once"
```

---

## Task 6: IPC + API for the ports ledger

**Files:**
- Modify: `src-tauri/src/ipc/commands.rs`, `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/api.rs`, `src-tauri/tests/api_test.rs`

- [ ] **Step 1: IPC commands.** In `commands.rs`:

```rust
use crate::ports::{PortEntry, PortRegistry};

#[tauri::command]
pub fn list_ports(reg: State<Arc<PortRegistry>>) -> Vec<PortEntry> { reg.list() }

#[tauri::command]
pub fn reserve_port(reg: State<Arc<PortRegistry>>, owner: String) -> u16 { reg.reserve_next(&owner) }
```
Register both in `lib.rs` `invoke_handler![...]`.

- [ ] **Step 2: API.** Add `ports: Arc<PortRegistry>` to `ApiState`; change `router(sup, ports, token)` and `serve(sup, ports, port, token)`; in `lib.rs` pass `ports.clone()` to `api::serve`. Add routes (auth-protected):
```rust
.route("/ports", get(list_ports))
.route("/ports/reserve", post(reserve_port))
```
with handlers reading `State<ApiState>` and calling `s.ports.list()` / `s.ports.reserve_next(&body.owner)` (add a `#[derive(Deserialize)] struct ReserveBody { owner: String }`).

- [ ] **Step 3: Update `api_test.rs`** `spawn_api` to build a `PortRegistry` and pass it to both `Supervisor::new` and `api::router`.

- [ ] **Step 4: Run + commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS.

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add src-tauri/src/ipc/commands.rs src-tauri/src/lib.rs src-tauri/src/api.rs src-tauri/tests/api_test.rs
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "FEAT: expose port ledger over IPC + API"
```

---

## Task 7: Dashboard surfaces the port + useDynamicPort toggle

**Files:**
- Modify: `src/shared/ipc.ts`, `src/views/dashboard/dashboard.ts`

- [ ] **Step 1:** In `dashboard.ts` `commandRow`, when `statusById[id]?.port` is set, render it next to the pid: `port ${port}`. (ProcInfo.port is already in the generated types from Task 3.)
- [ ] **Step 2:** In the add-command modal, add a checkbox bound to a `useDynamicPort` field; pass it through `ipc.addCommand(...)` (extend the IPC wrapper + the `add_command` Tauri command signature to accept `use_dynamic_port: bool`). In the add-project wizard, default `useDynamicPort` false for detected commands.
- [ ] **Step 3: Verify** `node_modules/.bin/tsc -p tsconfig.json --noEmit` (no output = pass) and `node_modules/.bin/vite build --config vite.config.ts` (bundles).
- [ ] **Step 4: Commit**

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add src/shared/ipc.ts src/views/dashboard/dashboard.ts src-tauri/src/ipc/commands.rs
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "FEAT: dashboard shows assigned port + useDynamicPort toggle"
```

(Note: extending `add_command` to take `use_dynamic_port` also touches `registry.rs::add_command` + the IPC command signature — keep the Rust + TS in sync.)

---

## Task 8: Reserve claude_usage's port (the original ask)

**Files:**
- Runtime data (not the repo): `%APPDATA%\com.sirbepy.server-supervisor\supervisor\ports.json`
- External (human-confirmed): claude_usage_in_taskbar's `vite.config.ts`, `src-tauri/tauri.conf.json` devUrl, `e2e/wdio.conf.js`

- [ ] **Step 1:** With the app NOT running, reserve claude_usage in the live ledger. Either launch the app and call the API `POST /ports/reserve {"owner":"claude_usage_in_taskbar"}` (returns the port — should be 42000, the first free reserved slot), or directly add to `ports.json`:
```json
{ "owner": "claude_usage_in_taskbar", "port": 42000, "note": "always-on tray app; vacated 1420" }
```
Confirm the returned/assigned port is **42000**.
- [ ] **Step 2:** Report the number to the user. **Do NOT edit claude_usage's files** — that is the user's separate, confirmed action (the user/another instance updates claude_usage's 3 spots to the assigned port). This plan never edits another project.

---

## Self-Review

- **Spec coverage:** opt-in `useDynamicPort` (Task 3) ✓; allocate from 42000+ reserved-aware ledger (Task 1) ✓; reserve-before-spawn (acquire marks in-memory before spawn, Task 1/4) ✓; free-on-exit (stop releases; ephemeral set rebuilt empty each start since `acquired` starts empty in `new`, Task 1/4) ✓; `{PORT}` substitution + PORT env both channels (Task 4) ✓; probe + warn (Task 4/5, `port_holder`) ✓; EADDRINUSE retry-once (Task 5) ✓; ProcInfo.port surfaced (Task 3/7) ✓; seed 7716 + 1420-blocked (Task 1) ✓; claude_usage=42000 (Task 8) ✓; never edit project files (env + `{PORT}` in the supervisor's own command string only) ✓.
- **Type consistency:** `reserve`/`reserve_next`/`acquire`/`release`/`list` used consistently; `Supervisor::new(data_dir, ports)` updated at all call sites (lib.rs + both test files); `start(Option<u16>)` signature updated at its only caller (`Supervisor::start`); `acquired_port()` getter used by `Supervisor::stop`.
- **Open follow-ups (not blocking):** live WebSocket log tail (multi-AI already works via polling); auto-reassign for non-`useDynamicPort` commands is intentionally NOT built (team projects untouched).
