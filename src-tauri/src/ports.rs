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
    ///
    /// OS-bound ports are detected two ways: the live LISTENING set from the OS
    /// TCP table (one netstat call, catches wildcard `[::]:port` / `0.0.0.0:port`
    /// holders that a loopback bind-probe misses on Windows) plus a bind-probe as
    /// a secondary check. Either signal marks the port taken.
    pub fn acquire(&self) -> Result<u16, String> {
        let taken = self.taken_set();
        let listening = listening_ports();
        let mut acq = self.acquired.lock().unwrap();
        for p in BASE..MAX {
            if taken.contains(&p) || acq.contains(&p) || listening.contains(&p) {
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

    /// Mark a port as in-use without going through `acquire` - used to restore a
    /// re-adopted process's dynamic port so it is not handed out again.
    pub fn mark_acquired(&self, port: u16) {
        self.acquired.lock().unwrap().insert(port);
    }

    fn taken_set(&self) -> std::collections::HashSet<u16> {
        let mut set: std::collections::HashSet<u16> =
            self.reserved.lock().unwrap().iter().map(|e| e.port).collect();
        set.extend(self.acquired.lock().unwrap().iter().copied());
        set
    }
}

/// A port counts as free only if it can be bound on BOTH the IPv4 and the IPv6
/// loopback. Servers (notably Node) frequently bind the IPv6 wildcard `[::]:port`,
/// which occupies the port for localhost clients while leaving the IPv4 bind free;
/// probing only `127.0.0.1` would then hand out a port that is actually taken.
/// On Windows `IPV6_V6ONLY` defaults to true, so the two binds are independent and
/// must both succeed. Either failure means "taken".
///
/// This is a secondary check: on Windows a process bound to the wildcard
/// `[::]:port` does NOT block a later bind to the specific loopback `[::1]:port`
/// (specific-vs-wildcard binds don't conflict without `SO_EXCLUSIVEADDRUSE`), so
/// the bind-probe alone can report such a port free. `listening_ports()` (the OS
/// TCP table) is the primary detector for those holders; see `acquire`.
fn port_free(port: u16) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
        && TcpListener::bind((Ipv6Addr::LOCALHOST, port)).is_ok()
}

/// The set of local ports currently in TCP LISTENING state, read once from the
/// OS via `netstat -ano`. Covers both IPv4 (`0.0.0.0:port`) and IPv6
/// (`[::]:port`) wildcard listeners regardless of how they were bound, which a
/// bind-probe cannot reliably detect on Windows. Best-effort: returns an empty
/// set if netstat is unavailable (the bind-probe still applies).
#[cfg(windows)]
fn listening_ports() -> std::collections::HashSet<u16> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut set = std::collections::HashSet::new();
    let Ok(out) = std::process::Command::new("netstat")
        .args(["-ano"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return set;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        // Columns: Proto, Local Address, Foreign Address, State, PID.
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 4 && cols[0].eq_ignore_ascii_case("TCP") && cols[3] == "LISTENING" {
            if let Some(port) = local_port(cols[1]) {
                set.insert(port);
            }
        }
    }
    set
}

#[cfg(not(windows))]
fn listening_ports() -> std::collections::HashSet<u16> {
    std::collections::HashSet::new()
}

/// Parse the port from a netstat local-address column: `0.0.0.0:42000`,
/// `[::]:42000`, `127.0.0.1:42000`, `[::1]:42000`. The port is the segment after
/// the final `:`.
#[cfg(windows)]
fn local_port(local: &str) -> Option<u16> {
    local.rsplit(':').next()?.parse().ok()
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

    #[test]
    fn mark_acquired_makes_port_taken() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        // BASE (42000) is what acquire() would normally hand out first. Mark it,
        // then acquire MUST skip it - proving the mark actually took effect.
        reg.mark_acquired(BASE);
        let got = reg.acquire().unwrap();
        assert_ne!(got, BASE, "acquire must skip a marked port");
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

    #[test]
    fn port_free_detects_ipv6_only_bind() {
        use std::net::{Ipv6Addr, TcpListener};
        // Bind the IPv6 loopback only (mirrors a server on `[::]:port`); IPv4 is
        // still free, so an IPv4-only probe would wrongly report the port free.
        let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!port_free(port), "IPv6-bound port must count as taken");
    }

    #[cfg(windows)]
    #[test]
    fn local_port_parses_netstat_addresses() {
        assert_eq!(local_port("0.0.0.0:42000"), Some(42000));
        assert_eq!(local_port("[::]:42000"), Some(42000));
        assert_eq!(local_port("127.0.0.1:6969"), Some(6969));
        assert_eq!(local_port("[::1]:1"), Some(1));
        assert_eq!(local_port("*:*"), None);
    }

    #[cfg(windows)]
    #[test]
    fn listening_ports_includes_a_bound_port() {
        use std::net::{Ipv4Addr, TcpListener};
        // Bind a real port and confirm the OS TCP table reports it as listening.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(
            listening_ports().contains(&port),
            "a freshly bound listener must show up in the OS LISTENING set"
        );
    }
}
