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

/// Owner label for a project's proxy-hub port reservation. Starts with
/// `{project_id}:`, so `release_project` (which strips everything prefixed
/// `{project_id}:`) reclaims it along with every command's slot.
fn hub_owner(project_id: &str) -> String {
    format!("{project_id}:__proxyhub__")
}

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
            // Skip a project's still-unclaimed block interior too (not just its
            // literally-reserved offsets), so an ephemeral acquisition never
            // squats a slot a project's own commands will want later.
            if self.port_in_any_block(p) {
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

    /// Resolve (and persist) the stable port for one project command, reusing
    /// the same `reserved` pool `ports.json` already persists rather than a
    /// second store. `owner` is the command's stable composite id
    /// (`project:command` - see `types::unit_id`), which survives project and
    /// command renames since those only touch the display name.
    ///
    /// `override_port`: `Some(p)` pins the command to `p` - validated in-range
    /// and not already held by a *different* owner - and persists it verbatim
    /// (a manual override). `None` returns this owner's existing block-slot
    /// port if it already legitimately has one (stable across restarts and
    /// across re-clearing a since-removed override), else allocates the next
    /// free slot in the project's port block(s): a `BLOCK_SIZE`-wide range
    /// aligned to a multiple of `BLOCK_SIZE` inside `BASE..MAX`, extending to a
    /// fresh block once the current one(s) fill up (see `block_slot`).
    pub fn project_port(
        &self,
        project_id: &str,
        owner: &str,
        override_port: Option<u16>,
    ) -> Result<u16, String> {
        if let Some(p) = override_port {
            if !(BASE..MAX).contains(&p) {
                return Err(format!("port must be between {BASE} and {MAX}"));
            }
            // Reject a port already held by a different *real* owner (another
            // command, or another project's own manual override). Block
            // markers are internal bookkeeping, not a "real" owner, so they're
            // excluded here and reported via the block-range check below
            // instead - which gives a clearer message for that case.
            {
                let g = self.reserved.lock().unwrap();
                if let Some(clash) = g
                    .iter()
                    .find(|e| e.port == p && e.owner != owner && !e.owner.starts_with(BLOCK_PREFIX))
                {
                    return Err(format!("port {p} is already reserved by {}", clash.owner));
                }
            }
            // Reject a port inside a DIFFERENT project's allocated block, even
            // if no individual command in it has claimed that exact offset
            // yet - the whole block is that project's, not just its used slots.
            if let Some(other) = self.project_owning_block(p) {
                if other != project_id {
                    return Err(format!("port {p} is inside {other}'s port block"));
                }
            }
            self.set_owner_port(owner, p, "manual override");
            return Ok(p);
        }

        if let Some(p) = self.owner_port(owner) {
            if self.port_in_project_block(project_id, p) {
                return Ok(p);
            }
        }
        let p = self.block_slot(project_id)?;
        self.set_owner_port(owner, p, "project port block");
        Ok(p)
    }

    /// Resolve (and persist) the FIXED port for a project's reverse-proxy hub
    /// listener (see `supervisor::proxy_hub`): offset `HUB_OFFSET` (the top
    /// slot) of the project's FIRST allocated port block, i.e. `base+9`. A
    /// dev app bakes this address in at compile time, so it must never move -
    /// once assigned it is a real owner reservation (`project:__proxyhub__`,
    /// not a `BLOCK_PREFIX` marker), which also blocks a regular command from
    /// later claiming that same offset via `block_slot`.
    ///
    /// Idempotent: a project that already has a hub port keeps it. Ensures a
    /// block exists (allocating the project's first one if this is the very
    /// first port it has ever claimed). Errors if the offset is already held
    /// by a different real owner - e.g. a project with a full 10-command
    /// block that predates this feature; a rare edge case left as a clear
    /// error rather than silently reassigning someone else's port.
    pub fn project_hub_port(&self, project_id: &str) -> Result<u16, String> {
        let owner = hub_owner(project_id);
        if let Some(p) = self.owner_port(&owner) {
            return Ok(p);
        }
        let base = self.first_block_base(project_id)?;
        let port = base + HUB_OFFSET;
        {
            let g = self.reserved.lock().unwrap();
            if let Some(clash) = g
                .iter()
                .find(|e| e.port == port && e.owner != owner && !e.owner.starts_with(BLOCK_PREFIX))
            {
                return Err(format!(
                    "proxy hub port {port} is already claimed by {}",
                    clash.owner
                ));
            }
        }
        self.set_owner_port(&owner, port, "proxy hub");
        Ok(port)
    }

    /// The base port of `project_id`'s first allocated block, creating one
    /// (via `block_slot`, which on an empty project always lands offset 0 of
    /// a freshly allocated block - exactly the base) if it doesn't exist yet.
    fn first_block_base(&self, project_id: &str) -> Result<u16, String> {
        let prefix = block_owner_prefix(project_id);
        {
            let g = self.reserved.lock().unwrap();
            let mut bases: Vec<u16> = g
                .iter()
                .filter(|e| e.owner.starts_with(&prefix))
                .map(|e| e.port)
                .collect();
            bases.sort_unstable();
            if let Some(&b) = bases.first() {
                return Ok(b);
            }
        }
        self.block_slot(project_id)
    }

    /// Drop a single owner's port reservation (its project-block slot or
    /// manual override), freeing that port for reuse - called when its
    /// command is deleted.
    pub fn release_owner(&self, owner: &str) {
        let mut g = self.reserved.lock().unwrap();
        let before = g.len();
        g.retain(|e| e.owner != owner);
        if g.len() != before {
            save(&self.data_dir, &g);
        }
    }

    /// Release every port reservation belonging to a project - every
    /// command's slot plus the project's block marker(s) - so the whole block
    /// can be reclaimed by a future project. Called when a project is deleted.
    pub fn release_project(&self, project_id: &str) {
        let block_prefix = block_owner_prefix(project_id);
        let cmd_prefix = format!("{project_id}:");
        let mut g = self.reserved.lock().unwrap();
        let before = g.len();
        g.retain(|e| !(e.owner.starts_with(&block_prefix) || e.owner.starts_with(&cmd_prefix)));
        if g.len() != before {
            save(&self.data_dir, &g);
        }
    }

    /// True if `port` is free right now judged by live OS state alone - the
    /// same two checks `acquire` relies on (the OS LISTENING table plus a
    /// dual-stack loopback bind-probe). Used by the supervisor to validate a
    /// project's usual/override port at spawn time before trusting it over a
    /// dynamic fallback (see `supervisor::registry::Supervisor::start`).
    pub fn is_os_port_free(&self, port: u16) -> bool {
        !listening_ports().contains(&port) && port_free(port)
    }

    /// The port currently reserved for `owner`, if any.
    fn owner_port(&self, owner: &str) -> Option<u16> {
        self.reserved
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.owner == owner)
            .map(|e| e.port)
    }

    /// True if `port` falls inside a port block already allocated to `project_id`.
    fn port_in_project_block(&self, project_id: &str, port: u16) -> bool {
        let prefix = block_owner_prefix(project_id);
        self.reserved
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.owner.starts_with(&prefix))
            .any(|e| (e.port..e.port + BLOCK_SIZE).contains(&port))
    }

    /// True if `port` falls inside ANY project's allocated block, regardless
    /// of whether that exact offset has been individually claimed yet. Used
    /// to keep the ephemeral `acquire()` path out of a block's unclaimed
    /// interior, so it never squats a slot a project's own commands will want
    /// later.
    fn port_in_any_block(&self, port: u16) -> bool {
        self.reserved
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.owner.starts_with(BLOCK_PREFIX))
            .any(|e| (e.port..e.port + BLOCK_SIZE).contains(&port))
    }

    /// The id of the project whose block contains `port`, if any.
    fn project_owning_block(&self, port: u16) -> Option<String> {
        let g = self.reserved.lock().unwrap();
        g.iter().find_map(|e| {
            let rest = e.owner.strip_prefix(BLOCK_PREFIX)?;
            let (project_id, _block) = rest.rsplit_once(':')?;
            (e.port..e.port + BLOCK_SIZE)
                .contains(&port)
                .then(|| project_id.to_string())
        })
    }

    /// Ports literally held by a *real* owner - every reservation except the
    /// internal block markers - plus in-flight ephemeral acquisitions. Used to
    /// find a reusable offset within a project's own already-allocated
    /// block(s): the block's own marker must NOT count as "occupying" its
    /// base forever, or a released command's slot could never be reused (see
    /// `block_slot`).
    fn real_taken_set(&self) -> std::collections::HashSet<u16> {
        let mut set: std::collections::HashSet<u16> = self
            .reserved
            .lock()
            .unwrap()
            .iter()
            .filter(|e| !e.owner.starts_with(BLOCK_PREFIX))
            .map(|e| e.port)
            .collect();
        set.extend(self.acquired.lock().unwrap().iter().copied());
        set
    }

    /// Overwrite (or insert) the single reserved entry for `owner`, dropping
    /// any prior entry it held. Persists immediately.
    fn set_owner_port(&self, owner: &str, port: u16, note: &str) {
        let mut g = self.reserved.lock().unwrap();
        g.retain(|e| e.owner != owner);
        g.push(PortEntry { owner: owner.to_string(), port, note: note.to_string() });
        save(&self.data_dir, &g);
    }

    /// The next free slot for `project_id`: the first unoccupied offset in one
    /// of its already-allocated blocks, else the base of a freshly allocated
    /// block (persisted as a marker entry so future lookups find it again).
    /// Blocks are tried in the order they were created, so a project's ports
    /// stay compact rather than spreading out. `taken` combines every
    /// persisted reservation (this project's own slots, other projects'
    /// blocks/overrides, always-on-app reservations) with in-flight ephemeral
    /// acquisitions, so a slot squatted by something outside this bookkeeping
    /// is correctly skipped rather than silently double-assigned.
    fn block_slot(&self, project_id: &str) -> Result<u16, String> {
        let prefix = block_owner_prefix(project_id);
        let mut bases: Vec<u16> = {
            let g = self.reserved.lock().unwrap();
            g.iter()
                .filter(|e| e.owner.starts_with(&prefix))
                .map(|e| e.port)
                .collect()
        };
        bases.sort_unstable();

        // Real occupancy (excludes the blocks' own markers) - so a released
        // command's offset shows free again instead of being permanently
        // shadowed by the marker that carved out the block in the first place.
        let real_taken = self.real_taken_set();
        for base in &bases {
            for offset in 0..BLOCK_SIZE {
                let p = base + offset;
                if !real_taken.contains(&p) {
                    return Ok(p);
                }
            }
        }

        // Every existing block (if any) is full: allocate a fresh one, aligned
        // to BLOCK_SIZE, at the lowest range not already claimed by anyone -
        // this DOES need the marker-inclusive set, so we never re-pick a base
        // some other project's block already sits on.
        let taken = self.taken_set();
        debug_assert_eq!(BASE % BLOCK_SIZE, 0, "BASE must be block-aligned");
        let mut candidate = BASE;
        loop {
            if candidate.saturating_add(BLOCK_SIZE) > MAX {
                return Err(format!("no free port block available in {BASE}..{MAX}"));
            }
            if !taken.contains(&candidate) {
                break;
            }
            candidate += BLOCK_SIZE;
        }
        let owner = block_owner(project_id, bases.len() as u16);
        self.reserve(&owner, candidate, "project port block");
        Ok(candidate)
    }

    fn taken_set(&self) -> std::collections::HashSet<u16> {
        let mut set: std::collections::HashSet<u16> =
            self.reserved.lock().unwrap().iter().map(|e| e.port).collect();
        set.extend(self.acquired.lock().unwrap().iter().copied());
        set
    }
}

/// Owner label for a project's Nth port block marker - a `PortEntry` whose
/// literal `port` is that block's base (see `PortRegistry::block_slot`).
/// Never collides with a real command's owner (`project:command`, no
/// `BLOCK_PREFIX` prefix).
fn block_owner(project_id: &str, block: u16) -> String {
    format!("{BLOCK_PREFIX}{project_id}:{block}")
}

fn block_owner_prefix(project_id: &str) -> String {
    format!("{BLOCK_PREFIX}{project_id}:")
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
/// set if netstat is unavailable (the bind-probe still applies). Derived from the
/// single `listeners()` netstat reader, dropping the owning PID.
fn listening_ports() -> std::collections::HashSet<u16> {
    listeners().into_iter().map(|(p, _)| p).collect()
}

/// `(port, owning pid)` for every TCP listener, read once from the OS via
/// `netstat -ano`. This is the single netstat call site in the codebase; other
/// modules (e.g. `supervisor::sampler`) consume this. Best-effort: empty on any
/// failure (callers keep forced ports / fall back to the bind-probe).
#[cfg(windows)]
pub(crate) fn listeners() -> Vec<(u16, u32)> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(out) = std::process::Command::new("netstat")
        .args(["-ano"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };
    parse_listeners(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(windows))]
pub(crate) fn listeners() -> Vec<(u16, u32)> {
    Vec::new()
}

/// Pure parser for `netstat -ano` output. Columns: Proto, Local Address, Foreign
/// Address, State, PID. We keep only TCP rows in the LISTENING state. Kept pure
/// (separate from the `Command` invocation) so it stays unit-testable on any
/// platform.
pub(crate) fn parse_listeners(text: &str) -> Vec<(u16, u32)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 5 && cols[0].eq_ignore_ascii_case("TCP") && cols[3] == "LISTENING" {
            if let (Some(port), Ok(pid)) = (local_port(cols[1]), cols[4].parse::<u32>()) {
                out.push((port, pid));
            }
        }
    }
    out
}

/// Parse the port from a netstat local-address column: `0.0.0.0:42000`,
/// `[::]:42000`, `127.0.0.1:42000`, `[::1]:42000`. The port is the segment after
/// the final `:`. The single local-address port parser for the codebase.
pub(crate) fn local_port(local: &str) -> Option<u16> {
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

    #[test]
    fn local_port_parses_netstat_addresses() {
        assert_eq!(local_port("0.0.0.0:42000"), Some(42000));
        assert_eq!(local_port("[::]:42000"), Some(42000));
        assert_eq!(local_port("127.0.0.1:6969"), Some(6969));
        assert_eq!(local_port("[::1]:1"), Some(1));
        assert_eq!(local_port("*:*"), None);
    }

    #[test]
    fn parse_listeners_keeps_only_listening_tcp_with_pid() {
        // Mixed netstat output: a header, a LISTENING IPv4 row, an ESTABLISHED row
        // (must be dropped), a LISTENING IPv6 wildcard row, and a UDP row.
        let text = "\
Active Connections
  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:8080           0.0.0.0:0              LISTENING       1234
  TCP    127.0.0.1:6970         127.0.0.1:51000        ESTABLISHED     1234
  TCP    [::]:42013             [::]:0                 LISTENING       5678
  UDP    0.0.0.0:5353           *:*                                    900";
        let mut got = parse_listeners(text);
        got.sort();
        assert_eq!(got, vec![(8080, 1234), (42013, 5678)]);
    }

    #[test]
    fn project_port_allocates_an_aligned_block_in_stable_order() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let p0 = reg.project_port("proj", "proj:c0", None).unwrap();
        assert_eq!(p0, BASE, "first project claims the very first block");
        for i in 1..10u16 {
            let owner = format!("proj:c{i}");
            let p = reg.project_port("proj", &owner, None).unwrap();
            assert_eq!(p, p0 + i, "commands fill the block in stable (first-assigned) order");
        }
        // Re-asking for an already-assigned owner (e.g. a later start()) must
        // return the identical port, not a new one.
        assert_eq!(reg.project_port("proj", "proj:c0", None).unwrap(), p0);
    }

    #[test]
    fn project_port_exhausts_into_a_second_aligned_block() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let mut first_base = 0u16;
        for i in 0..10u16 {
            let p = reg.project_port("proj", &format!("proj:c{i}"), None).unwrap();
            if i == 0 {
                first_base = p;
            }
        }
        let overflow = reg.project_port("proj", "proj:c10", None).unwrap();
        assert_eq!(overflow % BLOCK_SIZE, 0, "the overflow block is itself block-aligned");
        for i in 0..10u16 {
            assert_ne!(overflow, first_base + i, "must not collide into its own first block");
        }
    }

    #[test]
    fn project_port_second_block_skips_a_neighbouring_projects_range() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        // A neighbour claims the very first block.
        let neighbor_port = reg.project_port("neighbor", "neighbor:web", None).unwrap();
        assert_eq!(neighbor_port, BASE);

        // "big" fills a full block of 10 - forced past the neighbour's range.
        let mut big_base = 0u16;
        for i in 0..10u16 {
            let p = reg.project_port("big", &format!("big:c{i}"), None).unwrap();
            if i == 0 {
                big_base = p;
            }
        }
        assert_ne!(big_base, neighbor_port, "big must not reuse the neighbour's slot");

        // big's 11th command overflows into a third block - neither the
        // neighbour's range nor its own first block.
        let overflow = reg.project_port("big", "big:c10", None).unwrap();
        assert_ne!(overflow, neighbor_port);
        for i in 0..10u16 {
            assert_ne!(overflow, big_base + i);
        }
    }

    #[test]
    fn project_port_override_rejects_collision_with_another_owner() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let taken = reg.project_port("proj-a", "proj-a:web", None).unwrap();

        let err = reg
            .project_port("proj-b", "proj-b:web", Some(taken))
            .unwrap_err();
        assert!(err.contains("proj-a:web"), "error names the conflicting owner: {err}");

        // The SAME owner re-affirming its own port is fine (idempotent), and a
        // manual override always wins over whatever slot it already held.
        assert_eq!(
            reg.project_port("proj-a", "proj-a:web", Some(taken)).unwrap(),
            taken
        );
    }

    #[test]
    fn project_port_override_rejects_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        assert!(reg.project_port("proj", "proj:web", Some(80)).is_err());
        assert!(reg.project_port("proj", "proj:web", Some(65000)).is_err());
    }

    #[test]
    fn project_port_persists_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let p0;
        {
            let reg = PortRegistry::new(dir.path().to_path_buf());
            p0 = reg.project_port("proj", "proj:web", None).unwrap();
            // A manual override on a second command must also survive reload.
            reg.project_port("proj", "proj:api", Some(48000)).unwrap();
        }
        let reg2 = PortRegistry::new(dir.path().to_path_buf());
        assert_eq!(
            reg2.project_port("proj", "proj:web", None).unwrap(),
            p0,
            "auto-assigned block slot survives a restart"
        );
        // A caller that still wants the override keeps re-supplying it (the
        // supervisor always re-reads it from the persisted Command); the
        // registry's job is just to keep honoring the same owner+port pairing.
        assert_eq!(
            reg2.project_port("proj", "proj:api", Some(48000)).unwrap(),
            48000,
            "manual override survives a restart when re-supplied"
        );
        // The raw reservation is genuinely on disk, not just re-derived.
        assert!(reg2.list().iter().any(|e| e.owner == "proj:api" && e.port == 48000));
    }

    #[test]
    fn release_owner_frees_its_slot_for_reuse() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let p0 = reg.project_port("proj", "proj:c0", None).unwrap();
        reg.release_owner("proj:c0");
        assert!(reg.list().iter().all(|e| e.owner != "proj:c0"));
        // A fresh command in the same project reclaims the freed slot rather
        // than being pushed into a new block.
        let p1 = reg.project_port("proj", "proj:c1", None).unwrap();
        assert_eq!(p1, p0, "freed slot is reused");
    }

    #[test]
    fn release_project_frees_the_whole_block() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let p0 = reg.project_port("proj", "proj:c0", None).unwrap();
        reg.release_project("proj");
        assert!(
            reg.list().iter().all(|e| !e.owner.starts_with("proj") && !e.owner.contains(":proj:")),
            "both the command slot and the block marker are gone"
        );
        // A different project can now claim the same freed range.
        let reclaimed = reg.project_port("other", "other:c0", None).unwrap();
        assert_eq!(reclaimed, p0);
    }

    #[test]
    fn project_hub_port_is_base_plus_nine_and_stable() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PortRegistry::new(dir.path().to_path_buf());
        let hub = reg.project_hub_port("proj").unwrap();
        assert_eq!(hub, BASE + (BLOCK_SIZE - 1), "hub claims the block's top slot");
        // Idempotent: re-asking returns the identical port.
        assert_eq!(reg.project_hub_port("proj").unwrap(), hub);
        // A command allocated afterward must skip the offset the hub owns.
        let cmd_port = reg.project_port("proj", "proj:c0", None).unwrap();
        assert_ne!(cmd_port, hub, "a command must not collide with the hub's reserved offset");
    }

    #[test]
    fn project_hub_port_survives_reload_and_ten_full_commands_skip_it() {
        let dir = tempfile::tempdir().unwrap();
        let hub;
        {
            let reg = PortRegistry::new(dir.path().to_path_buf());
            hub = reg.project_hub_port("proj").unwrap();
            // Fill the rest of the block: 9 commands should fit in the 9
            // remaining offsets (0..8), the 10th must overflow to a new block
            // rather than colliding with the hub's offset 9.
            for i in 0..9u16 {
                let p = reg.project_port("proj", &format!("proj:c{i}"), None).unwrap();
                assert_ne!(p, hub);
            }
            let overflow = reg.project_port("proj", "proj:c9", None).unwrap();
            assert_ne!(overflow, hub);
            assert_eq!(overflow % BLOCK_SIZE, 0, "the 10th command overflows into a fresh aligned block");
        }
        let reg2 = PortRegistry::new(dir.path().to_path_buf());
        assert_eq!(reg2.project_hub_port("proj").unwrap(), hub, "hub port survives reload");
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
