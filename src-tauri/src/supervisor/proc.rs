use super::proxy;
use crate::types::{LogLine, ProcInfo, ProcKind, ProcSpec, ProcStatus};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

/// Max log lines retained per process (ring buffer).
const LOG_CAP: usize = 2000;

/// A crash within this window of start counts as dead-on-arrival (never came up).
const DOA_WINDOW_MS: u64 = 8_000;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One supervised process: its spec, current child handle, and a bounded log buffer.
pub struct ManagedProc {
    pub spec: ProcSpec,
    pub status: ProcStatus,
    pub pid: Option<u32>,
    pub started_at: Option<u64>,
    /// When the process last transitioned Running -> Crashed (unix ms). Paired
    /// with `started_at` to measure crash uptime: a tiny uptime means the launch
    /// never really came up (dead-on-arrival), e.g. a port-conflict variant.
    pub crashed_at: Option<u64>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    logs: Arc<Mutex<VecDeque<LogLine>>>,
    /// Flutter daemon appId, captured from the `app.started` stdout event.
    app_id: Arc<Mutex<Option<String>>>,
    /// Dynamic port handed out by the registry for the current run, if any.
    /// The Supervisor reads this on stop to release it back to the registry.
    acquired_port: Option<u16>,
    /// True when this proc was re-adopted from a prior app instance: it has a
    /// live PID but no Child handle and no stdio pipes (logs are frozen until
    /// the user restarts it). `refresh` polls the OS for its liveness instead
    /// of `try_wait`.
    adopted: bool,
    /// Live-reload reverse proxy in front of a flutter web-server run, if any.
    /// Dropping it triggers a graceful shutdown + thread join.
    proxy: Option<proxy::ProxyTask>,
    /// Broadcast sender the stdout reader fires on a finished (re)start; the
    /// proxy's SSE endpoint forwards it to open browser tabs. Some only while a
    /// proxy is live.
    reload_tx: Option<tokio::sync::broadcast::Sender<()>>,
    /// The internal ephemeral port flutter binds when proxied (the proxy fronts
    /// it on the public port). The registry releases this on stop.
    internal_port: Option<u16>,
    /// Subtree resident bytes, cached by the background sampler. Read by `info()`;
    /// never computed on the UI poll path. `None` until the first sample after a
    /// start, and cleared when the proc stops/crashes.
    sampled_mem: Option<u64>,
    /// OS-detected listening port, cached by the background sampler (same value
    /// the old inline `fill_ports` computed). `info()` prefers this over the
    /// forced `acquired_port` so the dashboard shows the port actually bound.
    sampled_port: Option<u16>,
}

impl ManagedProc {
    pub fn new(spec: ProcSpec) -> Self {
        Self {
            spec,
            status: ProcStatus::Stopped,
            pid: None,
            started_at: None,
            crashed_at: None,
            child: None,
            stdin: None,
            logs: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAP))),
            app_id: Arc::new(Mutex::new(None)),
            acquired_port: None,
            adopted: false,
            proxy: None,
            reload_tx: None,
            internal_port: None,
            sampled_mem: None,
            sampled_port: None,
        }
    }

    /// Store the latest background-sampler reading (subtree RAM + detected port).
    /// Called only from `Supervisor::sample_tick`, never on the UI poll path.
    pub fn set_sample(&mut self, mem: Option<u64>, port: Option<u16>) {
        self.sampled_mem = mem;
        self.sampled_port = port;
    }

    /// True when this proc is a flutter web-server launch with a `{PORT}`
    /// placeholder we can redirect: only these can sit behind a live-reload
    /// proxy (we move flutter onto an internal port and front it on the public
    /// one). Anything else runs unproxied.
    pub fn wants_proxy(&self) -> bool {
        self.spec.kind == ProcKind::Flutter
            && self.spec.cmd.contains("web-server")
            && self.spec.cmd.contains("{PORT}")
    }

    /// The internal port flutter binds behind the proxy, if proxied. The registry
    /// releases it on stop (it is acquired separately from the public port).
    pub fn internal_port(&self) -> Option<u16> {
        self.internal_port
    }

    /// The dynamic port currently held for this run, if any.
    pub fn acquired_port(&self) -> Option<u16> {
        self.acquired_port
    }

    /// Dead-on-arrival: crashed within `DOA_WINDOW_MS` of starting, i.e. the
    /// launch never really came up. Used to auto-prune failed `/run` attempts
    /// while sparing a real server that ran a long time and then crashed.
    pub fn is_dead_on_arrival(&self) -> bool {
        matches!(self.status, ProcStatus::Crashed)
            && match (self.started_at, self.crashed_at) {
                (Some(s), Some(c)) => c.saturating_sub(s) < DOA_WINDOW_MS,
                _ => false,
            }
    }

    pub fn is_adopted(&self) -> bool {
        self.adopted
    }

    /// Re-attach to a process from a prior app instance. We have only its PID
    /// (no Child, no pipes), so mark it Running+adopted, restore start time and
    /// port, and push one line explaining the frozen log pane.
    pub fn adopt(&mut self, pid: u32, started_at: u64, port: Option<u16>) {
        self.status = ProcStatus::Running;
        self.pid = Some(pid);
        self.started_at = Some(started_at);
        self.crashed_at = None;
        self.acquired_port = port;
        self.adopted = true;
        self.push_log(
            "stdout",
            "[supervisor] re-adopted after restart - live logs paused until you restart this process".to_string(),
        );
    }

    pub fn info(&self) -> ProcInfo {
        ProcInfo {
            id: self.spec.id.clone(),
            project: self.spec.project.clone(),
            name: self.spec.name.clone(),
            kind: self.spec.kind.clone(),
            status: self.status.clone(),
            pid: self.pid,
            // Prefer the sampler's OS-detected port; fall back to the forced
            // port until the first sample lands (≤ one sampler tick after start).
            port: self.sampled_port.or(self.acquired_port),
            // Cached by the background sampler, never computed here: the UI poll
            // path must not enumerate the process table (that was the lag).
            mem_bytes: self.sampled_mem,
        }
    }

    pub fn logs_snapshot(&self) -> Vec<LogLine> {
        self.logs.lock().unwrap().iter().cloned().collect()
    }

    /// Detect a process that exited on its own (crash or self-stop) and update status.
    pub fn refresh(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                self.status = if status.success() {
                    ProcStatus::Stopped
                } else {
                    self.crashed_at = Some(now_ms());
                    ProcStatus::Crashed
                };
                self.pid = None;
                self.child = None;
                self.stdin = None;
                // No longer running: drop the cached RAM/port so the dashboard
                // doesn't show a frozen figure until the next sampler tick.
                self.sampled_mem = None;
                self.sampled_port = None;
                // The child died on its own. Drop the proxy so its TcpListener on
                // the public port is freed immediately (otherwise it keeps serving
                // 502s until an explicit stop/restart); dropping ProxyTask runs its
                // graceful stop + thread join. Also drop the reload sender. Do NOT
                // clear internal_port: reap_tick reads internal_port() to release
                // the registry entry, and clearing it here would leak that
                // bookkeeping.
                self.proxy = None;
                self.reload_tx = None;
            }
            return;
        }
        // Adopted: no Child to wait on. Poll the OS - if the wrapper PID is gone,
        // the process ended on its own; we can't know the exit code, so mark it
        // Stopped (neutral) and drop adoption.
        if self.adopted {
            if let Some(pid) = self.pid {
                if !super::reaper::pid_is_our_wrapper(pid) {
                    self.status = ProcStatus::Stopped;
                    self.pid = None;
                    self.adopted = false;
                    self.push_log("stdout", "[supervisor] re-adopted process exited".to_string());
                }
            }
        }
    }

    /// Spawn the process via `cmd /C <cmd>` in its own process group so the whole
    /// tree can be killed later. Returns the spawned PID.
    ///
    /// `dynamic_port` is the port the child actually binds (the `{PORT}`
    /// substitution + PORT env), exactly as before. `proxy_public_port` is
    /// `Some(P)` only when this proc should sit behind a live-reload proxy: the
    /// child then binds `dynamic_port` (an internal ephemeral port) and the proxy
    /// fronts it on `P`, the port the dashboard advertises. `None` => no proxy.
    pub fn start(
        &mut self,
        dynamic_port: Option<u16>,
        proxy_public_port: Option<u16>,
    ) -> std::io::Result<u32> {
        self.refresh();
        if matches!(self.status, ProcStatus::Running | ProcStatus::Starting) {
            return Ok(self.pid.unwrap_or(0));
        }

        // Tear down any proxy left over from a prior (crashed) run before we
        // re-spawn, so the new proxy can re-bind the same public port. Dropping
        // it signals graceful shutdown + joins its thread.
        self.proxy = None;
        self.reload_tx = None;
        self.internal_port = None;

        // Stand up the live-reload proxy BEFORE spawning flutter so a bind failure
        // degrades cleanly. When the proxy binds, flutter binds an internal port
        // and the proxy fronts it on the public port. When it cannot bind, we run
        // flutter straight on the public port (no auto-reload) rather than leaving
        // an advertised-but-dead port. `child_port` is the port flutter ends up
        // binding either way.
        let mut proxy_task: Option<proxy::ProxyTask> = None;
        let mut reload_tx: Option<tokio::sync::broadcast::Sender<()>> = None;
        let mut internal_for_proxy: Option<u16> = None;
        let child_port = match (proxy_public_port, dynamic_port) {
            (Some(public), Some(internal)) => {
                let (tx, _rx) = tokio::sync::broadcast::channel(16);
                match proxy::spawn(public, internal, tx.clone()) {
                    Ok(task) => {
                        proxy_task = Some(task);
                        reload_tx = Some(tx);
                        internal_for_proxy = Some(internal);
                        self.push_log(
                            "stdout",
                            format!(
                                "[supervisor] live-reload proxy on 127.0.0.1:{public} -> flutter :{internal}"
                            ),
                        );
                        Some(internal)
                    }
                    Err(e) => {
                        self.push_log(
                            "stderr",
                            format!("[supervisor] live-reload proxy failed to bind {public}: {e}; serving flutter directly on {public}, auto-reload off"),
                        );
                        Some(public)
                    }
                }
            }
            _ => dynamic_port,
        };

        // Apply the port override (no project files touched): substitute any
        // `{PORT}` placeholder, and for a recognized framework that did not
        // express its own port, append the right CLI port flag (best-effort
        // force - a flag beats a hardcoded config port). The PORT env var is also
        // set below. `ports_detect` reports whatever it actually bound, so this is
        // a convenience, not load-bearing.
        let mut cmd_str = match child_port {
            Some(p) => super::port_inject::resolve_port(&self.spec.cmd, &self.spec.kind, p),
            None => self.spec.cmd.clone(),
        };

        // Flutter: force `--machine` so the daemon speaks the JSON-RPC protocol.
        // Machine mode is what lets us drive an `app.restart` over stdin for a
        // fast hot restart (and later a browser-reload signal); without it the
        // daemon ignores our requests. The reader in spawn_reader humanizes the
        // resulting JSON-RPC stdout back into readable log lines.
        if self.spec.kind == ProcKind::Flutter && !cmd_str.contains("--machine") {
            let mut tokens: Vec<String> = cmd_str.split_whitespace().map(|t| t.to_string()).collect();
            match tokens.iter().position(|t| t == "run") {
                Some(i) => tokens.insert(i + 1, "--machine".to_string()),
                None => tokens.push("--machine".to_string()),
            }
            cmd_str = tokens.join(" ");
        }

        let mut command = Command::new("cmd");
        command
            .arg("/C")
            .arg(&cmd_str)
            .current_dir(&self.spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Build the child PATH from the persisted machine+user registry PATH so
        // per-user toolchains (node via nvm, cargo/rustup) resolve regardless of
        // how the supervisor itself was launched: a logon-autostarted supervisor
        // may inherit only a reduced PATH that lacks the user entries, and
        // children would otherwise inherit that stripped PATH. Fall back to our
        // own inherited PATH if the registry read fails. Then resolve symlinked
        // PATH directories to their real targets so a toolchain installed behind
        // a junction (notably nvm-windows: `C:\nvm4w\nodejs` -> `...\nvm\v<ver>`)
        // launches without the "untrusted mount point" traversal failure. Done
        // before the per-command env overrides so an explicit `PATH=` override
        // still wins.
        #[cfg(windows)]
        {
            let base = super::spawn_env::registry_merged_path()
                .or_else(|| std::env::var("PATH").ok());
            if let Some(path) = base {
                command.env("PATH", super::spawn_env::resolve_path_dirs(&path));
            }
        }

        // Per-command env overrides (applied before PORT so a dynamic port still
        // wins). A `PATH=` here is now a fallback/override on top of the resolved
        // PATH above, not the only way to reach a junction-installed toolchain.
        for (k, v) in super::spawn_env::parse_env(&self.spec.env) {
            command.env(k, v);
        }

        if let Some(p) = child_port {
            command.env("PORT", p.to_string()); // env channel for process.env.PORT tools
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            // No console window: stdio is piped, so children (dev servers) never
            // need one. Without this every spawn flashes a terminal on Windows.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        }

        let mut child = command.spawn()?;
        let pid = child.id();

        // reload_tx was decided above (Some only when the proxy actually bound).
        self.reload_tx = reload_tx.clone();

        // Reset appId for the new run; stdout reader re-captures it.
        *self.app_id.lock().unwrap() = None;
        if let Some(out) = child.stdout.take() {
            spawn_reader(
                out,
                "stdout",
                self.logs.clone(),
                Some(self.app_id.clone()),
                self.reload_tx.clone(),
            );
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, "stderr", self.logs.clone(), None, None);
        }
        self.stdin = child.stdin.take();
        // The dashboard advertises the public port: when proxied that is the
        // proxy's port, not the internal port the child actually bound.
        self.acquired_port = proxy_public_port.or(dynamic_port);
        self.adopted = false; // a real Child supersedes any prior adoption

        // Store the proxy decided + spawned above. `internal_for_proxy` is Some
        // only when the proxy bound (flutter on the internal port); on a bind
        // failure flutter took the public port and there is no proxy to track.
        self.internal_port = internal_for_proxy;
        self.proxy = proxy_task;

        self.push_log("stdout", format!("[supervisor] started: {cmd_str}"));
        self.child = Some(child);
        self.pid = Some(pid);
        self.started_at = Some(now_ms());
        self.crashed_at = None;
        self.status = ProcStatus::Running;
        Ok(pid)
    }

    /// Kill the process tree and mark stopped.
    pub fn stop(&mut self) {
        if let Some(pid) = self.pid {
            super::reaper::kill_tree(pid);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        // Drop the proxy first: this signals its graceful shutdown and joins
        // its thread, freeing the public port before we report stopped.
        self.proxy = None;
        self.reload_tx = None;
        self.internal_port = None;
        self.stdin = None;
        self.status = ProcStatus::Stopped;
        self.pid = None;
        self.started_at = None;
        self.sampled_mem = None;
        self.sampled_port = None;
        *self.app_id.lock().unwrap() = None;
        self.push_log("stdout", "[supervisor] stopped".to_string());
    }

    /// Hot reload / restart a Flutter process by writing an `app.restart` message
    /// to the `flutter run --machine` daemon's stdin. Web uses `full=true` because
    /// hot reload is upstream-broken there.
    pub fn reload(&mut self, full: bool) -> Result<(), String> {
        if self.spec.kind != ProcKind::Flutter {
            return Err("reload is only supported for flutter processes".to_string());
        }
        let app_id = self
            .app_id
            .lock()
            .unwrap()
            .clone()
            .ok_or("flutter daemon not ready yet (no appId seen on stdout)")?;
        let stdin = self
            .stdin
            .as_mut()
            .ok_or("process has no stdin handle (not running?)")?;
        let msg = format!(
            "[{{\"id\":0,\"method\":\"app.restart\",\"params\":{{\"appId\":\"{}\",\"fullRestart\":{}}}}}]\n",
            app_id, full
        );
        stdin.write_all(msg.as_bytes()).map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())?;
        self.push_log(
            "stdout",
            format!("[supervisor] sent app.restart fullRestart={full}"),
        );
        Ok(())
    }

    fn push_log(&self, stream: &str, text: String) {
        push_line(&self.logs, stream, text);
    }
}

fn push_line(logs: &Arc<Mutex<VecDeque<LogLine>>>, stream: &str, text: String) {
    let mut buf = logs.lock().unwrap();
    if buf.len() >= LOG_CAP {
        buf.pop_front();
    }
    buf.push_back(LogLine {
        ts: now_ms(),
        stream: stream.to_string(),
        text,
    });
}

/// Parse a `flutter run --machine` line for the `app.started` event and return its appId.
fn parse_flutter_app_id(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') || !trimmed.contains("app.started") {
        return None;
    }
    let arr: Vec<serde_json::Value> = serde_json::from_str(trimmed).ok()?;
    for evt in arr {
        if evt.get("event").and_then(|e| e.as_str()) == Some("app.started") {
            if let Some(id) = evt
                .get("params")
                .and_then(|p| p.get("appId"))
                .and_then(|a| a.as_str())
            {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// The humanized result of parsing one `flutter run --machine` JSON-RPC line:
/// the readable text line(s) to push to the log pane, plus whether this line
/// signals a completed (re)start that should trigger a browser reload.
#[derive(Debug, Default, PartialEq)]
struct FlutterLog {
    /// Readable lines to push verbatim into the log buffer (already prefixed).
    lines: Vec<String>,
    /// True when this line means a restart finished and an open tab should reload.
    fire_reload: bool,
}

/// Parse a `flutter run --machine` JSON-RPC line (an array of event/response
/// objects) into readable log lines plus a reload-completion signal. Returns
/// `None` when the line is not machine JSON (caller falls back to verbatim).
fn parse_flutter_machine_line(line: &str) -> Option<FlutterLog> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') {
        return None;
    }
    let arr: Vec<serde_json::Value> = serde_json::from_str(trimmed).ok()?;
    let mut out = FlutterLog::default();
    for el in &arr {
        if let Some(event) = el.get("event").and_then(|e| e.as_str()) {
            let params = el.get("params");
            match event {
                // A full restart refreshing an already-open tab: announce + reload.
                "app.started" => {
                    out.lines.push("[flutter] app started".to_string());
                    out.fire_reload = true;
                }
                "app.progress" => {
                    if let Some(msg) = params
                        .and_then(|p| p.get("message"))
                        .and_then(|m| m.as_str())
                        .filter(|m| !m.is_empty())
                    {
                        let finished = params
                            .and_then(|p| p.get("finished"))
                            .and_then(|f| f.as_bool())
                            .unwrap_or(false);
                        if finished {
                            out.lines.push(format!("[flutter] {msg} (done)"));
                        } else {
                            out.lines.push(format!("[flutter] {msg}"));
                        }
                    }
                    // No message: emit nothing (transient progress with no text).
                }
                // The app's own stdout/print output: pass through with no prefix.
                "app.log" => {
                    if let Some(log) = params.and_then(|p| p.get("log")).and_then(|l| l.as_str()) {
                        out.lines.push(log.to_string());
                    }
                }
                "daemon.logMessage" => {
                    if let Some(msg) =
                        params.and_then(|p| p.get("message")).and_then(|m| m.as_str())
                    {
                        out.lines.push(format!("[flutter] {msg}"));
                    }
                }
                "app.webLaunchUrl" => {
                    if let Some(url) = flutter_url(params) {
                        out.lines.push(format!("[flutter] serving at {url}"));
                    }
                }
                other => {
                    if let Some(url) = flutter_url(params) {
                        out.lines.push(format!("[flutter] serving at {url}"));
                    } else {
                        out.lines.push(format!("[flutter] {other}"));
                    }
                }
            }
        } else if el.get("id").is_some() {
            // A response to one of our requests (e.g. the id:0 app.restart).
            if let Some(error) = el.get("error") {
                out.lines
                    .push(format!("[flutter] reload error: {error}"));
            } else if let Some(result) = el.get("result") {
                let code_ok = result
                    .get("code")
                    .and_then(|c| c.as_i64())
                    .map(|c| c == 0)
                    .unwrap_or(true); // no code field == success
                if code_ok {
                    out.lines.push("[flutter] reload complete".to_string());
                    out.fire_reload = true;
                } else {
                    out.lines.push(format!("[flutter] reload error: {result}"));
                }
            }
        }
    }
    Some(out)
}

/// Extract a serving URL from an event's params (`url` or `wsUri`), if present.
fn flutter_url(params: Option<&serde_json::Value>) -> Option<String> {
    let p = params?;
    p.get("url")
        .or_else(|| p.get("wsUri"))
        .and_then(|u| u.as_str())
        .map(|s| s.to_string())
}

fn spawn_reader<R: Read + Send + 'static>(
    reader: R,
    stream: &'static str,
    logs: Arc<Mutex<VecDeque<LogLine>>>,
    app_id: Option<Arc<Mutex<Option<String>>>>,
    reload_tx: Option<broadcast::Sender<()>>,
) {
    std::thread::spawn(move || {
        let buffered = BufReader::new(reader);
        for line in buffered.lines() {
            let Ok(text) = line else { break };
            // Capture the Flutter daemon appId from the raw JSON, exactly as before.
            if let Some(slot) = &app_id {
                if slot.lock().unwrap().is_none() {
                    if let Some(id) = parse_flutter_app_id(&text) {
                        *slot.lock().unwrap() = Some(id);
                    }
                }
            }
            // Humanize machine JSON into readable lines; on any non-JSON line
            // (pre-daemon "Launching...", plain stderr) push it verbatim once.
            match parse_flutter_machine_line(&text) {
                Some(parsed) => {
                    for l in parsed.lines {
                        push_line(&logs, stream, l);
                    }
                    if parsed.fire_reload {
                        if let Some(tx) = &reload_tx {
                            let _ = tx.send(());
                        }
                    }
                }
                None => push_line(&logs, stream, text),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::parse_flutter_app_id;
    use super::ManagedProc;
    use crate::types::{ProcKind, ProcSpec, ProcStatus};

    fn test_spec() -> ProcSpec {
        ProcSpec {
            id: "proj:cmd".to_string(),
            project: "proj".to_string(),
            name: "cmd".to_string(),
            cmd: "flutter run".to_string(),
            cwd: ".".to_string(),
            kind: ProcKind::Flutter,
            autostart: false,
            use_dynamic_port: true,
            env: String::new(),
        }
    }

    #[test]
    fn dead_on_arrival_only_for_young_crashes() {
        let mut p = ManagedProc::new(test_spec());

        // Never started: not DOA.
        assert!(!p.is_dead_on_arrival());

        // Crashed 3s after start: DOA.
        p.status = ProcStatus::Crashed;
        p.started_at = Some(1_000);
        p.crashed_at = Some(4_000);
        assert!(p.is_dead_on_arrival());

        // Crashed 20min after start: a real crash, NOT DOA.
        p.crashed_at = Some(1_000 + 20 * 60 * 1_000);
        assert!(!p.is_dead_on_arrival());

        // Running (not crashed): not DOA regardless of timestamps.
        p.status = ProcStatus::Running;
        p.crashed_at = Some(4_000);
        assert!(!p.is_dead_on_arrival());

        // Exactly at the window edge (started_at + DOA_WINDOW_MS): NOT DOA - the
        // predicate uses strict `<`. Pins the off-by-one so the constant and the
        // comparison can never silently drift apart.
        p.status = ProcStatus::Crashed;
        p.started_at = Some(1_000);
        p.crashed_at = Some(1_000 + 8_000);
        assert!(!p.is_dead_on_arrival());
    }

    #[test]
    fn adopt_marks_running_without_a_child() {
        let mut p = ManagedProc::new(test_spec());
        p.adopt(4321, 1_000, Some(42013));
        assert_eq!(p.status, ProcStatus::Running);
        assert_eq!(p.pid, Some(4321));
        assert_eq!(p.started_at, Some(1_000));
        assert_eq!(p.acquired_port(), Some(42013));
        assert!(p.is_adopted());
        // Adopted with no child handle: is_dead_on_arrival must stay false
        // (it only fires on a Crashed status, which adopt never sets).
        assert!(!p.is_dead_on_arrival());
    }

    #[test]
    fn restart_clears_adoption() {
        // A re-adopted proc that is later restarted owns a real Child, so the
        // adopted flag must clear. start() spawns `cmd`, so drive it through a
        // command that exits immediately and assert the flag flipped.
        let mut spec = test_spec();
        spec.cmd = "cmd /C exit 0".to_string(); // trivial, exits instantly
        spec.kind = ProcKind::Generic;
        let mut p = ManagedProc::new(spec);
        p.adopt(4321, 1_000, None);
        assert!(p.is_adopted());
        let _ = p.start(None, None); // real spawn -> sets a Child, must clear adopted
        assert!(!p.is_adopted(), "start() must supersede adoption");
    }

    #[test]
    fn wants_proxy_gates_flutter_web_server_with_port_placeholder() {
        // flutter + web-server + {PORT}: proxiable.
        let mut spec = test_spec();
        spec.kind = ProcKind::Flutter;
        spec.cmd = "flutter run -d web-server --web-port {PORT}".to_string();
        assert!(ManagedProc::new(spec).wants_proxy());

        // flutter but no web-server (e.g. chrome device): not proxiable.
        let mut spec = test_spec();
        spec.kind = ProcKind::Flutter;
        spec.cmd = "flutter run -d chrome --web-port {PORT}".to_string();
        assert!(!ManagedProc::new(spec).wants_proxy());

        // flutter web-server but no {PORT} placeholder to redirect: not proxiable.
        let mut spec = test_spec();
        spec.kind = ProcKind::Flutter;
        spec.cmd = "flutter run -d web-server".to_string();
        assert!(!ManagedProc::new(spec).wants_proxy());

        // generic command: never proxiable.
        let mut spec = test_spec();
        spec.kind = ProcKind::Generic;
        spec.cmd = "npm run dev -- --port {PORT}".to_string();
        assert!(!ManagedProc::new(spec).wants_proxy());
    }

    #[test]
    fn parses_app_started_event() {
        let line = r#"[{"event":"app.started","params":{"appId":"abc123","supportsRestart":true}}]"#;
        assert_eq!(parse_flutter_app_id(line), Some("abc123".to_string()));
    }

    #[test]
    fn ignores_non_started_lines() {
        assert_eq!(parse_flutter_app_id("Performing hot restart..."), None);
        assert_eq!(parse_flutter_app_id(r#"[{"event":"app.progress"}]"#), None);
        assert_eq!(parse_flutter_app_id("[not json"), None);
    }

    use super::parse_flutter_machine_line;

    #[test]
    fn machine_line_app_started_fires_reload() {
        let line = r#"[{"event":"app.started","params":{"appId":"abc"}}]"#;
        let out = parse_flutter_machine_line(line).unwrap();
        assert_eq!(out.lines, vec!["[flutter] app started".to_string()]);
        assert!(out.fire_reload);
    }

    #[test]
    fn machine_line_restart_response_completes() {
        let line = r#"[{"id":0,"result":{"code":0,"message":"ok"}}]"#;
        let out = parse_flutter_machine_line(line).unwrap();
        assert_eq!(out.lines, vec!["[flutter] reload complete".to_string()]);
        assert!(out.fire_reload);
    }

    #[test]
    fn machine_line_restart_error_does_not_fire() {
        let line = r#"[{"id":0,"error":{"code":-32000,"message":"boom"}}]"#;
        let out = parse_flutter_machine_line(line).unwrap();
        assert!(out.lines[0].starts_with("[flutter] reload error:"));
        assert!(!out.fire_reload);
    }

    #[test]
    fn machine_line_progress_message_and_done() {
        let started = r#"[{"event":"app.progress","params":{"message":"Hot restarting"}}]"#;
        assert_eq!(
            parse_flutter_machine_line(started).unwrap().lines,
            vec!["[flutter] Hot restarting".to_string()]
        );
        let done = r#"[{"event":"app.progress","params":{"message":"Hot restart","finished":true}}]"#;
        assert_eq!(
            parse_flutter_machine_line(done).unwrap().lines,
            vec!["[flutter] Hot restart (done)".to_string()]
        );
        // No message: no line emitted.
        let empty = r#"[{"event":"app.progress","params":{}}]"#;
        assert!(parse_flutter_machine_line(empty).unwrap().lines.is_empty());
    }

    #[test]
    fn machine_line_app_log_passes_through() {
        let line = r#"[{"event":"app.log","params":{"log":"hello from app"}}]"#;
        assert_eq!(
            parse_flutter_machine_line(line).unwrap().lines,
            vec!["hello from app".to_string()]
        );
    }

    #[test]
    fn non_machine_line_returns_none() {
        assert_eq!(parse_flutter_machine_line("Launching lib/main.dart"), None);
        assert_eq!(parse_flutter_machine_line("[not json"), None);
    }

    #[test]
    fn flutter_start_injects_machine_after_run() {
        let mut spec = test_spec();
        spec.cmd = "flutter run -d chrome".to_string();
        spec.kind = ProcKind::Flutter;
        // Drive start with a command we won't actually keep running by checking
        // the transform indirectly is hard, so assert the token logic directly.
        let injected = inject_machine_for_test("flutter run -d chrome");
        assert_eq!(injected, "flutter run --machine -d chrome");
        let already = inject_machine_for_test("flutter run --machine -d chrome");
        assert_eq!(already, "flutter run --machine -d chrome");
        let no_run = inject_machine_for_test("flutter");
        assert_eq!(no_run, "flutter --machine");
        let _ = spec;
    }

    // Mirror of the injection logic in start() for unit testing without spawning.
    fn inject_machine_for_test(cmd: &str) -> String {
        let mut cmd_str = cmd.to_string();
        if !cmd_str.contains("--machine") {
            let mut tokens: Vec<String> =
                cmd_str.split_whitespace().map(|t| t.to_string()).collect();
            match tokens.iter().position(|t| t == "run") {
                Some(i) => tokens.insert(i + 1, "--machine".to_string()),
                None => tokens.push("--machine".to_string()),
            }
            cmd_str = tokens.join(" ");
        }
        cmd_str
    }
}
