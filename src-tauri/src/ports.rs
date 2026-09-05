//! Global port registry/allocator. server_supervisor is the single source of
//! truth for which ports are taken across the user's projects, handing out fresh
//! ones from a high, clash-safe block: above the common dev ports (3000, 5173,
//! 8080, 1420, ...) and below the Windows ephemeral range that starts at 49152.
//!
//! This file holds the struct definition, construction, the always-on-app
//! reservation helpers, the raw OS port check, and `ports.json` load/save. The
//! two allocation concerns are each a further `impl PortRegistry` block in a
//! sibling module: the persisted project-block allocator (`block`:
//! `project_port`/`project_hub_port`/`release_owner`/`release_project` and
//! their block-bookkeeping helpers) and the ephemeral per-run acquisition path
//! (`acquire`: `acquire`/`release`/`mark_acquired`). `taken_set` stays here
//! since all three - `reserve_next`, `acquire`, and `block_slot` - read it.

use crate::ports_os::{listening_ports, port_free};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use ts_rs::TS;

mod acquire;
mod block;

const FILE: &str = "ports.json";
const BASE: u16 = 42000;
const MAX: u16 = 49000;
/// Width of a project's port block. Commands within a project fill a block in
/// stable (first-assigned) order: base+0, base+1, ... base+(BLOCK_SIZE-1).
/// The 11th command overflows into a fresh block (see `block_slot`).
const BLOCK_SIZE: u16 = 10;
/// Owner-string prefix for a project's port block marker entries (see
/// `block_owner`). Never collides with a real command's owner
/// (`project:command`, which never contains `:` before the project id).
const BLOCK_PREFIX: &str = "__portblock__:";
/// Offset within a project's first port block reserved for its reverse-proxy
/// hub listener (the top slot: `base+9`, one past the last regular command
/// offset). See `PortRegistry::project_hub_port`.
const HUB_OFFSET: u16 = BLOCK_SIZE - 1;

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

    /// True if `port` is free right now judged by live OS state alone - the
    /// same two checks `acquire` relies on (the OS LISTENING table plus a
    /// dual-stack loopback bind-probe). Used by the supervisor to validate a
    /// project's usual/override port at spawn time before trusting it over a
    /// dynamic fallback (see `supervisor::registry::Supervisor::start`).
    pub fn is_os_port_free(&self, port: u16) -> bool {
        !listening_ports().contains(&port) && port_free(port)
    }

    fn taken_set(&self) -> std::collections::HashSet<u16> {
        let mut set: std::collections::HashSet<u16> =
            self.reserved.lock().unwrap().iter().map(|e| e.port).collect();
        set.extend(self.acquired.lock().unwrap().iter().copied());
        set
    }
}

fn load(data_dir: &Path) -> Vec<PortEntry> {
    std::fs::read_to_string(data_dir.join(FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(data_dir: &Path, entries: &[PortEntry]) {
    if let Ok(text) = serde_json::to_string_pretty(entries) {
        if let Err(e) = crate::fsutil::write_atomic(&data_dir.join(FILE), text.as_bytes()) {
            log::error!("ports: failed to write {FILE}: {e}");
        }
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
}
