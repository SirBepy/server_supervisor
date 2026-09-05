//! Kill-path lifecycle for the Supervisor: `stop`, `stop_all`, `shutdown_all`.
//! A second `impl Supervisor` block, split from `registry` (struct +
//! construction/readopt/reap-tick/sample-tick/start). Reaches the supervisor's
//! `pub(super)` fields directly.

use super::Supervisor;
use crate::supervisor::proc::{ManagedProc, StopHandle};
use crate::supervisor::reaper;
use crate::types::ProcKind;

/// Snapshots one proc's ports + `StopHandle` under the lock; run the release
/// + `handle.finish()` only AFTER dropping it (here and in `crud`).
pub(in crate::supervisor) fn begin_stop_locked(p: &mut ManagedProc) -> (Option<u16>, Option<u16>, StopHandle) {
    let released = p.acquired_port();
    let released_internal = p.internal_port();
    (released, released_internal, p.begin_stop())
}

impl Supervisor {
    /// Stop every running process but keep the app alive (tray "Close Processes").
    /// Distinct from `shutdown_all`, which is the kill-then-exit path. Kills
    /// run in parallel, not a serial loop - see `shutdown_all`.
    pub fn stop_all(&self) {
        let entries: Vec<(Option<u16>, Option<u16>, StopHandle)> = {
            let mut guard = self.procs.lock().unwrap();
            guard
                .values_mut()
                .filter(|p| p.pid.is_some())
                .map(begin_stop_locked)
                .collect()
        };
        let mut handles = Vec::with_capacity(entries.len());
        for (released, released_internal, handle) in entries {
            if let Some(port) = released {
                self.ports.release(port);
            }
            if let Some(port) = released_internal {
                self.ports.release(port);
            }
            handles.push(handle);
        }
        self.persist_pids();
        std::thread::scope(|scope| {
            for handle in handles {
                scope.spawn(move || handle.finish());
            }
        });
    }

    /// Stop one process. The slow kill runs off `self.procs`'s lock, so it
    /// never blocks list()/start()/stop() of every OTHER process.
    pub fn stop(&self, id: &str) -> Result<(), String> {
        self.stop_inner(id, true)
    }

    /// `delete_ephemeral` is false only for `restart`, which stops and starts the
    /// SAME id: deleting an ephemeral entry between the two leaves `start` with
    /// nothing to look up, so the restart fails after the process is already dead.
    pub(in crate::supervisor) fn stop_inner(
        &self,
        id: &str,
        delete_ephemeral: bool,
    ) -> Result<(), String> {
        let (released, released_internal, handle, is_ephemeral) = {
            let mut guard = self.procs.lock().unwrap();
            let p = guard
                .get_mut(id)
                .ok_or_else(|| format!("unknown process id: {id}"))?;
            let is_ephemeral = p.spec.kind == ProcKind::Ephemeral;
            let (released, released_internal, handle) = begin_stop_locked(p);
            (released, released_internal, handle, is_ephemeral)
        };
        // internal_port is always distinct from acquired_port (separate
        // acquire), so no double-release risk releasing both.
        if let Some(port) = released {
            self.ports.release(port);
        }
        if let Some(port) = released_internal {
            self.ports.release(port);
        }
        self.persist_pids();
        handle.finish();
        // A manually-stopped ephemeral entry must not linger as `stopped`
        // either - mirrors `reap_tick`'s exit-triggered delete.
        if delete_ephemeral && is_ephemeral {
            if let Some((project_id, command_id)) = id.split_once(':') {
                if let Err(e) = self.remove_command(project_id, command_id) {
                    log::warn!("stop: could not remove ephemeral {id}: {e}");
                }
            }
        }
        Ok(())
    }

    /// Kill-then-exit path ("Stop all & quit"). Parallel kill, same as
    /// `stop_all` - a serial loop would make quitting itself hang for a while.
    pub fn shutdown_all(&self) {
        let handles: Vec<StopHandle> = {
            let mut guard = self.procs.lock().unwrap();
            guard
                .values_mut()
                .filter(|p| p.pid.is_some())
                .map(|p| begin_stop_locked(p).2)
                .collect()
        };
        std::thread::scope(|scope| {
            for handle in handles {
                scope.spawn(move || handle.finish());
            }
        });
        reaper::write_pids(&self.data_dir, &[]);
    }
}
