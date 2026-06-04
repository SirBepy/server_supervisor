use crate::types::{LogLine, ProcInfo, ProcKind, ProcSpec, ProcStatus};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
        }
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
            port: self.acquired_port,
            // Filled in a single shared refresh pass by `mem::fill_memory` after
            // the list is built; per-proc sampling here would build one System
            // per process, which the spec forbids.
            mem_bytes: None,
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
    pub fn start(&mut self, dynamic_port: Option<u16>) -> std::io::Result<u32> {
        self.refresh();
        if matches!(self.status, ProcStatus::Running | ProcStatus::Starting) {
            return Ok(self.pid.unwrap_or(0));
        }

        // Apply the port override (no project files touched): substitute any
        // `{PORT}` placeholder in the command, and set the PORT env var below.
        let cmd_str = match dynamic_port {
            Some(p) => self.spec.cmd.replace("{PORT}", &p.to_string()),
            None => self.spec.cmd.clone(),
        };

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

        if let Some(p) = dynamic_port {
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

        // Reset appId for the new run; stdout reader re-captures it.
        *self.app_id.lock().unwrap() = None;
        if let Some(out) = child.stdout.take() {
            spawn_reader(out, "stdout", self.logs.clone(), Some(self.app_id.clone()));
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, "stderr", self.logs.clone(), None);
        }
        self.stdin = child.stdin.take();
        self.acquired_port = dynamic_port;
        self.adopted = false; // a real Child supersedes any prior adoption

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
        self.stdin = None;
        self.status = ProcStatus::Stopped;
        self.pid = None;
        self.started_at = None;
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

fn spawn_reader<R: Read + Send + 'static>(
    reader: R,
    stream: &'static str,
    logs: Arc<Mutex<VecDeque<LogLine>>>,
    app_id: Option<Arc<Mutex<Option<String>>>>,
) {
    std::thread::spawn(move || {
        let buffered = BufReader::new(reader);
        for line in buffered.lines() {
            let Ok(text) = line else { break };
            if let Some(slot) = &app_id {
                if slot.lock().unwrap().is_none() {
                    if let Some(id) = parse_flutter_app_id(&text) {
                        *slot.lock().unwrap() = Some(id);
                    }
                }
            }
            push_line(&logs, stream, text);
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
        let _ = p.start(None); // real spawn -> sets a Child, must clear adopted
        assert!(!p.is_adopted(), "start() must supersede adoption");
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

}
