//! Global port registry/allocator. server_supervisor is the single source of
//! truth for which ports are taken across the user's projects, handing out fresh
//! ones from a high, clash-safe block: above the common dev ports (3000, 5173,
//! 8080, 1420, ...) and below the Windows ephemeral range that starts at 49152.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use ts_rs::TS;

const FILE: &str = "ports.json";
const BASE: u16 = 42000;
const MAX: u16 = 49000;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct PortEntry {
    pub owner: String,
    pub port: u16,
    pub note: String,
}

pub struct PortRegistry {
    entries: Mutex<Vec<PortEntry>>,
    data_dir: PathBuf,
}

impl PortRegistry {
    pub fn new(data_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&data_dir);
        let reg = Self {
            entries: Mutex::new(load(&data_dir)),
            data_dir,
        };
        // Seed known/blocked ports (idempotent).
        reg.reserve("server_supervisor", 7716, "vite dev (self)");
        reg.reserve("_blocked_default", 1420, "common Tauri default - never assign");
        reg
    }

    pub fn list(&self) -> Vec<PortEntry> {
        self.entries.lock().unwrap().clone()
    }

    /// Record a port under `owner` if that exact port isn't already tracked.
    pub fn reserve(&self, owner: &str, port: u16, note: &str) {
        let mut g = self.entries.lock().unwrap();
        if g.iter().any(|e| e.port == port) {
            return;
        }
        g.push(PortEntry {
            owner: owner.into(),
            port,
            note: note.into(),
        });
        save(&self.data_dir, &g);
    }

    /// Idempotent per owner: returns the owner's existing port, otherwise
    /// allocates the lowest free port >= BASE that is neither already tracked
    /// nor currently bound on the OS, records it, and returns it.
    pub fn allocate(&self, owner: &str) -> Result<u16, String> {
        let mut g = self.entries.lock().unwrap();
        if let Some(e) = g.iter().find(|e| e.owner == owner) {
            return Ok(e.port);
        }
        let taken: HashSet<u16> = g.iter().map(|e| e.port).collect();
        for port in BASE..MAX {
            if taken.contains(&port) || !port_free(port) {
                continue;
            }
            g.push(PortEntry {
                owner: owner.into(),
                port,
                note: "auto-allocated".into(),
            });
            save(&self.data_dir, &g);
            return Ok(port);
        }
        Err(format!("no free port available in {BASE}..{MAX}"))
    }
}

fn port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn load(data_dir: &Path) -> Vec<PortEntry> {
    std::fs::read_to_string(data_dir.join(FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(data_dir: &Path, entries: &[PortEntry]) {
    if let Ok(text) = serde_json::to_string_pretty(entries) {
        let _ = std::fs::write(data_dir.join(FILE), text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_known_ports() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let ports: Vec<u16> = reg.list().iter().map(|e| e.port).collect();
        assert!(ports.contains(&7716));
        assert!(ports.contains(&1420));
    }

    #[test]
    fn allocates_in_range_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let p = reg.allocate("proj-a").unwrap();
        assert!((BASE..MAX).contains(&p));
        assert_eq!(reg.allocate("proj-a").unwrap(), p, "same owner -> same port");
        let q = reg.allocate("proj-b").unwrap();
        assert_ne!(p, q, "different owners -> different ports");
    }

    #[test]
    fn never_allocates_a_reserved_port() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        reg.reserve("x", BASE, "taken");
        assert_ne!(reg.allocate("proj").unwrap(), BASE);
    }

    #[test]
    fn persists_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let p = {
            let reg = PortRegistry::new(dir.path().to_path_buf());
            reg.allocate("proj").unwrap()
        };
        let reg2 = PortRegistry::new(dir.path().to_path_buf());
        assert_eq!(reg2.allocate("proj").unwrap(), p, "should remember across reload");
    }
}
