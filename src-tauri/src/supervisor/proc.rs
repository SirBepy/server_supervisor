use crate::types::{LogLine, ProcInfo, ProcSpec, ProcStatus};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
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
    logs: Arc<Mutex<VecDeque<LogLine>>>,
}

impl ManagedProc {
    pub fn new(spec: ProcSpec) -> Self {
        Self {
            spec,
            status: ProcStatus::Stopped,
            pid: None,
            started_at: None,
            child: None,
            logs: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAP))),
        }
    }

    pub fn info(&self) -> ProcInfo {
        ProcInfo {
            id: self.spec.id.clone(),
            project: self.spec.project.clone(),
            name: self.spec.name.clone(),
            kind: self.spec.kind.clone(),
            status: self.status.clone(),
            pid: self.pid,
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
            }
        }
    }

    /// Spawn the process via `cmd /C <cmd>` in its own process group so the whole
    /// tree can be killed later. Returns the spawned PID.
    pub fn start(&mut self) -> std::io::Result<u32> {
        self.refresh();
        if matches!(self.status, ProcStatus::Running | ProcStatus::Starting) {
            return Ok(self.pid.unwrap_or(0));
        }

        let mut command = Command::new("cmd");
        command
            .arg("/C")
            .arg(&self.spec.cmd)
            .current_dir(&self.spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }

        let mut child = command.spawn()?;
        let pid = child.id();

        if let Some(out) = child.stdout.take() {
            spawn_reader(out, "stdout", self.logs.clone());
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, "stderr", self.logs.clone());
        }

        self.push_log("stdout", format!("[supervisor] started: {}", self.spec.cmd));
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
        self.status = ProcStatus::Stopped;
        self.pid = None;
        self.started_at = None;
        self.push_log("stdout", "[supervisor] stopped".to_string());
    }

    fn push_log(&self, stream: &str, text: String) {
        let mut buf = self.logs.lock().unwrap();
        if buf.len() >= LOG_CAP {
            buf.pop_front();
        }
        buf.push_back(LogLine {
            ts: now_ms(),
            stream: stream.to_string(),
            text,
        });
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    reader: R,
    stream: &'static str,
    logs: Arc<Mutex<VecDeque<LogLine>>>,
) {
    std::thread::spawn(move || {
        let buffered = BufReader::new(reader);
        for line in buffered.lines() {
            let Ok(text) = line else { break };
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
    });
}
