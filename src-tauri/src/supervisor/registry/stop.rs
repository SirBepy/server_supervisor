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
    ///
    /// Ephemeral entries are deleted here too, same as `stop_inner` - the app
    /// stays alive afterward, so there is nothing racing the `config::save`
    /// each `remove_command` does. Contrast `shutdown_all`, where landing that
    /// write before exit is the harder question (see its own doc comment).
    pub fn stop_all(&self) {
        let entries: Vec<(Option<u16>, Option<u16>, StopHandle, bool, String)> = {
            let mut guard = self.procs.lock().unwrap();
            guard
                .values_mut()
                .filter(|p| p.pid.is_some())
                .map(|p| {
                    let is_ephemeral = p.spec.kind == ProcKind::Ephemeral;
                    let id = p.spec.id.clone();
                    let (released, released_internal, handle) = begin_stop_locked(p);
                    (released, released_internal, handle, is_ephemeral, id)
                })
                .collect()
        };
        let mut handles = Vec::with_capacity(entries.len());
        let mut ephemeral_ids: Vec<String> = Vec::new();
        for (released, released_internal, handle, is_ephemeral, id) in entries {
            if let Some(port) = released {
                self.ports.release(port);
            }
            if let Some(port) = released_internal {
                self.ports.release(port);
            }
            handles.push(handle);
            if is_ephemeral {
                ephemeral_ids.push(id);
            }
        }
        self.persist_pids();
        std::thread::scope(|scope| {
            for handle in handles {
                scope.spawn(move || handle.finish());
            }
        });
        // Ephemeral entries don't linger as `stopped` after a bulk stop either -
        // mirrors `stop_inner`'s and `reap_tick`'s exit-triggered delete.
        for id in ephemeral_ids {
            if let Some((project_id, command_id)) = id.split_once(':') {
                if let Err(e) = self.remove_command(project_id, command_id) {
                    log::warn!("stop_all: could not remove ephemeral {id}: {e}");
                }
            }
        }
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
    ///
    /// Also deletes ephemeral entries, same as `stop_all`/`stop_inner`. That is
    /// safe here only because of how this is called: `supervisor::shutdown_all`
    /// (src-tauri/src/supervisor.rs) runs from `lib.rs`'s `RunEvent::ExitRequested`
    /// handler, which Tauri invokes synchronously on the main thread and does not
    /// wrap in `prevent_exit()` - so the process only actually exits once this
    /// function returns. `remove_command`'s `config::save` is a blocking
    /// `fs::write` (via `fsutil::write_atomic`), not spawned onto another thread
    /// or an async task, so it is guaranteed to complete before that return. If a
    /// future refactor moves this call off the main thread or behind `spawn`,
    /// re-check this guarantee before keeping the delete here.
    pub fn shutdown_all(&self) {
        let entries: Vec<(StopHandle, bool, String)> = {
            let mut guard = self.procs.lock().unwrap();
            guard
                .values_mut()
                .filter(|p| p.pid.is_some())
                .map(|p| {
                    let is_ephemeral = p.spec.kind == ProcKind::Ephemeral;
                    let id = p.spec.id.clone();
                    (begin_stop_locked(p).2, is_ephemeral, id)
                })
                .collect()
        };
        let mut handles = Vec::with_capacity(entries.len());
        let mut ephemeral_ids: Vec<String> = Vec::new();
        for (handle, is_ephemeral, id) in entries {
            handles.push(handle);
            if is_ephemeral {
                ephemeral_ids.push(id);
            }
        }
        std::thread::scope(|scope| {
            for handle in handles {
                scope.spawn(move || handle.finish());
            }
        });
        reaper::write_pids(&self.data_dir, &[]);
        for id in ephemeral_ids {
            if let Some((project_id, command_id)) = id.split_once(':') {
                if let Err(e) = self.remove_command(project_id, command_id) {
                    log::warn!("shutdown_all: could not remove ephemeral {id}: {e}");
                }
            }
        }
    }
}
