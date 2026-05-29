use crate::types::{LogLine, ProcInfo, ProcKind, ProcSpec, ProcStatus};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Max log lines retained per process (ring buffer).
const LOG_CAP: usize = 2000;

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
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    logs: Arc<Mutex<VecDeque<LogLine>>>,
    /// Flutter daemon appId, captured from the `app.started` stdout event.
    app_id: Arc<Mutex<Option<String>>>,
    /// Dynamic port handed out by the registry for the current run, if any.
    /// The Supervisor reads this on stop to release it back to the registry.
    acquired_port: Option<u16>,
}

impl ManagedProc {
    pub fn new(spec: ProcSpec) -> Self {
        Self {
            spec,
            status: ProcStatus::Stopped,
            pid: None,
            started_at: None,
            child: None,
            stdin: None,
            logs: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAP))),
            app_id: Arc::new(Mutex::new(None)),
            acquired_port: None,
        }
    }

    /// The dynamic port currently held for this run, if any.
    pub fn acquired_port(&self) -> Option<u16> {
        self.acquired_port
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
                    ProcStatus::Crashed
                };
                self.pid = None;
                self.child = None;
                self.stdin = None;
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

        self.push_log("stdout", format!("[supervisor] started: {cmd_str}"));
        self.child = Some(child);
        self.pid = Some(pid);
        self.started_at = Some(now_ms());
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
