//! The ephemeral per-run acquisition path: `acquire`/`release`/`mark_acquired`,
//! held in-memory only and freed on process exit. Distinct from the sibling
//! `block` module's persisted project-block allocator, which `acquire` only
//! ever reads around (`port_in_any_block`) so it never squats a project's
//! still-unclaimed block interior.

use super::{PortRegistry, BASE, BLOCK_PREFIX, BLOCK_SIZE, MAX};
use crate::ports_os::{listening_ports, port_free};

impl PortRegistry {
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
