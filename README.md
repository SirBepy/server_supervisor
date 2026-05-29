# server_supervisor

> A local process supervisor that owns and outlives your dev sessions.

---

## About

`server_supervisor` is a Tauri 2 desktop app that acts as a single headless owner for all your local dev servers - Flutter, Node, or anything else. Close the window and it hides to the system tray; servers keep running. Quit from the tray and every child process is killed cleanly before exit. No orphans, ever.

A webview dashboard lets you see, start, stop, and restart processes at a glance, with live log streaming. A localhost HTTP + WebSocket API (bearer-token protected, `127.0.0.1` only) lets an AI agent drive the same controls programmatically, making it a useful companion to tools like Claude Code or Cursor.

Flutter processes are managed via `flutter run --machine` with proper daemon JSON protocol for hot reload and restart. Flutter web hot reload is upstream-broken; web targets use full restart only.

## Stack

| Layer | Tech |
|---|---|
| Desktop shell | Tauri 2 |
| Backend | Rust + Tokio + Axum |
| Frontend | Vanilla TypeScript + Vite + lit-html |
| Types | ts-rs (Rust to TS codegen) |
| Shared utilities | `vendor/tauri_kit` |
| Platform | Windows / PowerShell 5.1 |

## How to run

```powershell
# Install dependencies
npm install

# Run in dev mode (starts Vite + Tauri)
npm run tauri dev

# Build for release
npm run tauri build
```

> Before building, `npm run prebuild` auto-generates `src/types/ipc.generated.ts` from Rust types via ts-rs.

## API

The supervisor exposes a local HTTP + WebSocket API on `127.0.0.1` only. Bearer token is read from a `0600` config file on each request. Endpoints:

- `GET /processes` - list all supervised processes
- `POST /processes/:id/start` - start a process
- `POST /processes/:id/stop` - stop a process
- `POST /processes/:id/restart` - restart a process
- `GET /processes/:id/logs` - fetch captured logs
- `WS /processes/:id/logs/stream` - live log stream

## Project structure

Domain-grouped Rust modules (sibling `.rs` + folder, no `mod.rs`). One view per folder on the frontend. Generated types are gitignored and rebuilt on every `prebuild`.
