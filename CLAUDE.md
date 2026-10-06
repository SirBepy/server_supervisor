@~/.claude/snippets/full-auto.md

# server_supervisor

A local process supervisor. One headless owner (the Tauri Rust backend) launches and owns dev servers across projects (Flutter, Node, etc.), survives window close (runs in the tray), and by default leaves its children running across a quit or self-update, re-adopting them on the next launch. A webview dashboard is the human UI; a localhost HTTP API (bearer-token; plus one SSE endpoint inside the per-process reload proxy) lets an AI agent list / start / stop / restart processes and read logs.

## Stack

Tauri 2, Rust backend, vanilla TypeScript + Vite + lit-html, plain CSS per feature, ts-rs for Rust to TS types. Shared building blocks come from `vendor/tauri_kit` (settings store + schema-driven settings UI). Windows / PowerShell 5.1.

## Hard rules

- The Rust backend owns every spawned process. Closing the window hides to tray (servers keep running). Quitting LEAVES servers running by default so a self-update does not nuke them; on next launch the backend re-adopts the still-alive processes (Running but log-frozen until restarted, since their stdio pipes died with the old instance). The tray offers "Close Processes" to stop everything, and Quit asks whether to stop or leave. Tradeoff (accepted): a force-kill or a lost `pids.json` while detached leaks processes until found manually. (This intentionally retires the former "no orphans, ever" rule.)
- The localhost API binds `127.0.0.1` only and requires a bearer token read from a 0600 config file on every request. It can spawn arbitrary commands, so it must never bind externally.
- Flutter processes launch with `flutter run --machine`; reload/restart is an `app.restart` JSON message to the daemon's stdin. Flutter web hot reload is upstream-broken, so web uses fullRestart only.

## Testing

The global verification floor applies (Rust `cargo test`, `tsc --noEmit`, `vite build`); do NOT `@import` the `test-e2e` snippet.

UI behavior is Claude's to verify, not Joe's: `npm run e2e:build` then `npm run e2e` drives the real webview of an isolated debug build over WebView2's CDP port (`e2e/`, playwright-core). The build uses its own identifier (`com.sirbepy.server-supervisor-e2e`), so it gets its own app-data dir, tray icon, single-instance lock and API port (6979), and never touches Joe's installed supervisor or its servers. Each run reseeds that dir from `e2e/fixture.mjs`, screenshots every step into `.for_bepy/screenshots/<session id>/`, and stops every child before killing the app. Add a spec step for each UI behavior change. Only pure visual judgment (does it look right) is left for Joe.

## Structure

Follows `~/.claude/skills/migrate-structure/structure/tauri.md`: domain-grouped Rust modules (sibling `.rs` + folder, no `mod.rs`), one view per folder on the frontend, ts-rs `prebuild` emits `src/types/ipc.generated.ts` (gitignored).
