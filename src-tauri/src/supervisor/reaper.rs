//! Windows process tracking: persist running PIDs to `pids.json` and kill
//! process trees on stop / explicit stop-and-quit. Leftover PIDs from a prior
//! session are NOT reaped on startup - the Supervisor re-adopts the still-alive
//! ones instead (see `Supervisor::readopt_orphans`); this module just provides
//! the PID read/write + liveness helpers it uses.

use serde::{Deserialize, Serialize};
use std::path::Path;

const PIDS_FILE: &str = "pids.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidEntry {
    pub id: String,
    pub pid: u32,
    pub started_at: u64,
    /// Dynamic port held by this run, restored on re-adopt. `None` for commands
    /// without a dynamic port. `#[serde(default)]` keeps pre-port files loadable.
    #[serde(default)]
    pub port: Option<u16>,
}

fn pids_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(PIDS_FILE)
}

pub fn write_pids(data_dir: &Path, entries: &[PidEntry]) {
    if let Ok(text) = serde_json::to_string_pretty(entries) {
        let _ = std::fs::write(pids_path(data_dir), text);
    }
}

pub(super) fn read_pids(data_dir: &Path) -> Vec<PidEntry> {
    std::fs::read_to_string(pids_path(data_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
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
pub(super) fn pid_is_our_wrapper(pid: u32) -> bool {
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
pub(super) fn pid_is_our_wrapper(_pid: u32) -> bool {
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

#[cfg(test)]
mod tests {
    use super::PidEntry;

    #[test]
    fn pid_entry_round_trips_with_optional_port() {
        let e = PidEntry { id: "p:c".into(), pid: 1234, started_at: 99, port: Some(42013) };
        let json = serde_json::to_string(&e).unwrap();
        let back: PidEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.port, Some(42013));
        assert_eq!(back.id, "p:c");

        // Legacy file (pre-port) must still deserialize - port defaults to None.
        let legacy = r#"{"id":"p:c","pid":1234,"started_at":99}"#;
        let back: PidEntry = serde_json::from_str(legacy).unwrap();
        assert_eq!(back.port, None);
    }
}
