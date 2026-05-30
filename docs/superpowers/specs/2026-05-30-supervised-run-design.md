# Supervised server-run: `POST /run` + `supervised-run` skill

**Date:** 2026-05-30
**Status:** Approved design, pending implementation plan.

## Goal

Let any Claude Code instance start a project's **long-lived dev server** through server_supervisor instead of spawning it in its own shell. Routing through the supervisor gives shared visibility (it appears in Joe's dashboard), single ownership (the supervisor reaps it, no orphans), and centralized logs/control. The instance auto-registers the project/command if it isn't known yet, and falls back to a normal shell run if the supervisor isn't reachable.

This is two deliverables: (1) a backend HTTP endpoint `POST /run`; (2) a global, guarded skill `supervised-run` that uses it.

## Decisions (locked during brainstorming)

- **Scope:** only long-lived servers / watchers (e.g. `npm run dev`, `vite`, `flutter run`, a backend, file watchers). One-off commands that exit (tests, builds, git, scripts) run normally in Claude's own shell - never registered.
- **Unregistered server:** auto-register the project (root = current folder) + command and start it, then report. No prompt. (This is strictly *more* visible than the status quo of an invisible shell spawn.)
- **Not reachable:** if `/health` fails or the token/port files are missing, fall back to a normal shell run and say so. Do NOT auto-launch the GUI tray app. Never block.
- **Dynamic port:** on by default for AI-run servers. To make it real (not an ignored env var), the skill templates the port flag into the command where the tool supports one (`vite --port {PORT}`, `next dev -p {PORT}`, `flutter run --web-port {PORT}`, node via `PORT` env).
- **API shape:** a single composite `POST /run` (not granular CRUD), leaning on the existing idempotent `add_project` (path dedup) + `add_command` (cmd dedup) + `start`.

## Part 1 - Backend: `POST /run`

**Route:** added to the existing axum router in `src-tauri/src/api.rs`, **behind the same bearer-auth layer** as the `/procs` routes (added before `.route_layer(...auth)`; only `/health` remains unauthenticated).

**Request body** (`#[derive(Deserialize)]`):
```
{
  "root": "<absolute project folder>",   // required
  "cmd": "<command string>",             // required, may contain {PORT}
  "name": "<optional display name>",     // optional; defaults to derive_name(cmd)
  "kind": "generic" | "flutter",         // optional; defaults "generic"
  "use_dynamic_port": true               // optional; defaults true
}
```

**Handler behavior** (reuses existing `Supervisor` methods, all already idempotent):
1. `add_project(name.unwrap_or(basename(root)), root)` → returns the project (reuses an existing one whose canonical path matches - the path dedup already shipped).
2. `add_command(project.id, name.unwrap_or(derive_name(cmd)), cmd, kind.unwrap_or(Generic), autostart=false, use_dynamic_port)` → returns the command (reuses if an exact-`cmd` match already exists - the cmd dedup already shipped).
3. `start(unit_id(project.id, command.id))` → starts it; acquires a dynamic port if `use_dynamic_port`.
4. Respond with the `ProcInfo` for that unit (id, project, name, kind, status, pid, port) as JSON.

**Idempotency:** calling `POST /run` twice with the same root+cmd reuses the same project/command and simply (re)starts it - no duplicate entries. This is guaranteed by the dedup already in `add_project`/`add_command`.

**New code:** one route, one request struct, one handler, and a server-side `derive_name(cmd)` helper mirroring the frontend's (`npm|pnpm run X` / `yarn X` → `X`, else the trimmed cmd). No changes to `add_project`/`add_command`/`start`.

**Security:** `/run` lets a bearer-token holder *define and run* an arbitrary command - a widening from "control already-registered commands." This is the endpoint's explicit purpose; the API remains 127.0.0.1-only with the token read from the 0600 `api_token.txt` on every request. Note this in the `api.rs` module/handler doc comment.

## Part 2 - The `supervised-run` skill

**Location:** `~/.claude/skills/supervised-run/SKILL.md` (global config, outside this repo). Named distinctly from the built-in `run` skill (which is about running-to-verify-a-change); this one is specifically "start a long-lived server via the supervisor."

**Trigger:** when Claude needs to start a long-lived dev server/watcher for the current project. Explicitly not for one-off commands.

**Flow:**
1. **Discover:** read `api_token.txt` and `api_port.txt` from `%APPDATA%\com.sirbepy.server-supervisor\supervisor\`. (`api_port.txt` is written on startup by the API's bind-probe - already shipped.)
2. **Probe:** `GET http://127.0.0.1:<port>/health`. If it fails, or either file is missing → **fall back**: run the server in Claude's own background shell, state "supervisor not running, ran directly." Stop.
3. **Run:** `POST /run` (with the bearer token) - `root = cwd`, the command (port-templated where supported), `kind` (flutter for `flutter run`, else generic), `use_dynamic_port = true`.
4. **Report:** surface the returned port + status. It is now in Joe's dashboard.
5. **Afterward:** logs/stop/restart via the existing `/procs/:id/{logs,stop,restart}`.

**Guard (the load-bearing scoping):** the skill is globally available but only *engages* when (a) a long-lived server is genuinely needed and (b) `/health` passes. Otherwise it no-ops into normal shell running. This prevents the misfires that sink an unconditional global rule (roblox/Luau, libraries, headless/cron, app-not-running).

## Error handling

- Connection refused / `/health` non-200 / missing token-or-port file → shell fallback (non-blocking).
- `POST /run` returns 4xx/5xx (bad body, spawn failure) → report the error to Joe; fall back to a shell run.
- Token present but auth rejected (401) → report; do not retry blindly.

## Testing

- **Backend integration test** (`src-tauri/tests/`, mirrors `ports_override_test` style): write a tiny node project, `POST /run`, assert it registers a project + command and starts it, and that the returned `ProcInfo` has a port in the dynamic range. Assert **idempotency**: a second `POST /run` with the same root+cmd does not create a duplicate project/command.
- **API auth test** (`api_test.rs`): `POST /run` without the token → 401; with the token → 200.
- **Skill:** it is prose (instructions), not code - validated by manual use / QA, not an automated test.

## Placement summary

- `POST /run` route + handler + request struct + `derive_name` helper + tests → **this repo** (`src-tauri/src/api.rs`, `src-tauri/tests/`).
- `supervised-run` skill → `~/.claude/skills/supervised-run/` (**Joe's global config**, separate from this repo).
- This design doc → `docs/superpowers/specs/2026-05-30-supervised-run-design.md`.

## Non-goals (YAGNI)

- Granular project/command CRUD over HTTP (update/remove/list-projects) - not needed for the run flow; the dashboard's Tauri IPC covers management.
- Auto-launching the supervisor app from a Claude instance.
- Routing one-off/exiting commands through the supervisor.
- Multi-instance contention handling beyond what the idempotent endpoints already provide.
