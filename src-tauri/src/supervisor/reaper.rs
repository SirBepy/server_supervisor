//! Windows orphan-reaping: persist running PIDs to `pids.json`, kill process
//! trees on stop/quit, and reconcile leftover PIDs from a prior crash on startup.

use serde::{Deserialize, Serialize};
use std::path::Path;

const PIDS_FILE: &str = "pids.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidEntry {
    pub id: String,
    pub pid: u32,
    pub started_at: u64,
}

fn pids_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(PIDS_FILE)
}

pub fn write_pids(data_dir: &Path, entries: &[PidEntry]) {
    if let Ok(text) = serde_json::to_string_pretty(entries) {
        let _ = std::fs::write(pids_path(data_dir), text);
    }
}

fn read_pids(data_dir: &Path) -> Vec<PidEntry> {
    std::fs::read_to_string(pids_path(data_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// On startup, kill any tracked PID that survived a prior crash. Guards against
/// PID reuse by confirming the PID is still a `cmd.exe` (our wrapper) before killing.
pub fn reconcile(data_dir: &Path) {
    let entries = read_pids(data_dir);
    for e in &entries {
        if pid_is_our_wrapper(e.pid) {
            log::warn!("supervisor: reaping orphan from prior session id={} pid={}", e.id, e.pid);
            kill_tree(e.pid);
        }
    }
    // Clear the file; the current session re-populates it as it starts processes.
    let _ = std::fs::remove_file(pids_path(data_dir));
}

#[cfg(windows)]
pub fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

#[cfg(not(windows))]
pub fn kill_tree(pid: u32) {
    let _ = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .output();
}

#[cfg(windows)]
fn pid_is_our_wrapper(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("tasklist")
        .args([
            "/FI",
            &format!("PID eq {pid}"),
            "/FI",
            "IMAGENAME eq cmd.exe",
            "/NH",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()),
        Err(_) => false,
    }
}

#[cfg(not(windows))]
fn pid_is_our_wrapper(_pid: u32) -> bool {
    false
}

/// Best-effort: name of the process holding `port`, via netstat -> pid -> tasklist.
/// Used only for a diagnostic warning; returns None if nothing is found.
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
    if pid == "0" || pid.is_empty() {
        return None;
    }
    let t = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&t.stdout);
    name.split_whitespace()
        .next()
        .map(|s| s.to_string())
        .filter(|s| s != "INFO:")
}

#[cfg(not(windows))]
pub fn port_holder(_port: u16) -> Option<String> {
    None
}
