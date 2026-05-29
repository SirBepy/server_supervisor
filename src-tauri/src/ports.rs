//! Global port registry/allocator. server_supervisor is the single source of
//! truth for which ports are taken across the user's projects, handing out fresh
//! ones from a high, clash-safe block: above the common dev ports (3000, 5173,
//! 8080, 1420, ...) and below the Windows ephemeral range that starts at 49152.

use serde::{Deserialize, Serialize};
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
    /// Persistent reserved ports (seeds + always-on apps). Written to ports.json.
    reserved: Mutex<Vec<PortEntry>>,
    /// Ephemeral per-run acquisitions, in-memory only, freed on process exit.
    acquired: Mutex<std::collections::HashSet<u16>>,
    data_dir: PathBuf,
}

impl PortRegistry {
    pub fn new(data_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&data_dir);
        let reg = Self {
            reserved: Mutex::new(load(&data_dir)),
            acquired: Mutex::new(std::collections::HashSet::new()),
            data_dir,
        };
        reg.reserve("server_supervisor", 6969, "vite dev (self)");
        reg.reserve("server_supervisor_api", 6970, "localhost API (self)");
        reg.reserve("_blocked_default", 1420, "common Tauri default - never assign");
        reg
    }

    pub fn list(&self) -> Vec<PortEntry> {
        self.reserved.lock().unwrap().clone()
    }

    /// Record an exact port for an owner (idempotent on port). Persistent.
    pub fn reserve(&self, owner: &str, port: u16, note: &str) {
        let mut g = self.reserved.lock().unwrap();
        if g.iter().any(|e| e.port == port) {
            return;
        }
        g.push(PortEntry { owner: owner.into(), port, note: note.into() });
        save(&self.data_dir, &g);
    }

    /// Reserve a fresh persistent port for an always-on app (idempotent per owner).
    /// Returns the owner's existing reserved port, else the lowest free >= BASE.
    pub fn reserve_next(&self, owner: &str) -> u16 {
        {
            let g = self.reserved.lock().unwrap();
            if let Some(e) = g.iter().find(|e| e.owner == owner) {
                return e.port;
            }
        }
        let taken = self.taken_set();
        let port = (BASE..MAX).find(|p| !taken.contains(p)).unwrap_or(BASE);
        self.reserve(owner, port, "always-on app");
        port
    }

    /// Acquire an ephemeral per-run port: lowest free >= BASE not reserved, not
    /// already acquired, not OS-bound. Held in-memory until `release`.
    pub fn acquire(&self) -> Result<u16, String> {
        let taken = self.taken_set();
        let mut acq = self.acquired.lock().unwrap();
        for p in BASE..MAX {
            if taken.contains(&p) || acq.contains(&p) {
                continue;
            }
            if !port_free(p) {
                continue;
            }
            acq.insert(p);
            return Ok(p);
        }
        Err(format!("no free port available in {BASE}..{MAX}"))
    }

    pub fn release(&self, port: u16) {
        self.acquired.lock().unwrap().remove(&port);
    }

    fn taken_set(&self) -> std::collections::HashSet<u16> {
        let mut set: std::collections::HashSet<u16> =
            self.reserved.lock().unwrap().iter().map(|e| e.port).collect();
        set.extend(self.acquired.lock().unwrap().iter().copied());
        set
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
    fn seeds_block_1420_and_records_self_ports() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let reserved: Vec<u16> = reg.list().iter().map(|e| e.port).collect();
        assert!(reserved.contains(&6969));
        assert!(reserved.contains(&6970));
        assert!(reserved.contains(&1420));
    }

    #[test]
    fn reserve_persists_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        {
            let reg = PortRegistry::new(dir.path().to_path_buf());
            let p = reg.reserve_next("always-on-app");
            assert!((BASE..MAX).contains(&p));
            assert_eq!(reg.reserve_next("always-on-app"), p, "idempotent per owner");
        }
        // survives reload
        let reg2 = PortRegistry::new(dir.path().to_path_buf());
        assert!(reg2.list().iter().any(|e| e.owner == "always-on-app"));
    }

    #[test]
    fn acquire_skips_reserved_and_is_ephemeral() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let reserved = reg.reserve_next("app"); // takes BASE (42000)
        let a = reg.acquire().unwrap();
        let b = reg.acquire().unwrap();
        assert_ne!(a, reserved);
        assert_ne!(a, b, "two live acquisitions differ");
        // acquisitions are in-memory only: not written to ports.json
        let saved = std::fs::read_to_string(dir.path().join("ports.json")).unwrap();
        assert!(!saved.contains(&a.to_string()), "ephemeral ports must not persist");
        reg.release(a);
        let c = reg.acquire().unwrap();
        assert_eq!(c, a, "released port is reusable");
    }
}
