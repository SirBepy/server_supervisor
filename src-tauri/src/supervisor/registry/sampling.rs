//! Periodic reconciliation ticks, driven by a timer in `lib.rs` (not the UI
//! poll - see each method's own doc for why that matters).

use super::Supervisor;
use crate::types::ProcKind;

impl Supervisor {
    /// Backend reconcile pass, meant to run on a timer rather than only when
    /// `list()` is called from the UI/API. When the window is hidden to the tray
    /// and nothing polls, nothing else refreshes the procs - so a crashed child
    /// would keep a stale Running status, its dynamic port would leak in the
    /// registry, and pids.json would name a dead PID the next launch might wrongly
    /// re-adopt. This refreshes every proc, releases the port of any that just
    /// transitioned out of running on its own, and rewrites pids.json.
    pub fn reap_tick(&self) {
        let mut released: Vec<u16> = Vec::new();
        // (project_id, command_id) of every Ephemeral proc that just exited on
        // its own - collected under the procs lock, deleted after dropping it
        // (see `remove_command`'s own two-phase pattern).
        let mut ephemeral_exited: Vec<(String, String)> = Vec::new();
        {
            let mut guard = self.procs.lock().unwrap();
            for p in guard.values_mut() {
                // Holding a pid before refresh but not after means the child
                // ended on its own (crash or self-exit). A user-initiated stop
                // already cleared the pid earlier, so it won't be seen here -
                // its port was released on the stop path.
                let had_pid = p.pid.is_some();
                // Capture the internal port before refresh; a crash clears the
                // child but refresh does NOT touch internal_port, so this is a
                // belt-and-braces snapshot.
                let internal = p.internal_port();
                p.refresh();
                if had_pid && p.pid.is_none() {
                    if let Some(port) = p.acquired_port() {
                        released.push(port);
                    }
                    if let Some(port) = internal {
                        released.push(port);
                    }
                    if p.spec.kind == ProcKind::Ephemeral {
                        if let Some((project_id, command_id)) = p.spec.id.split_once(':') {
                            ephemeral_exited.push((project_id.to_string(), command_id.to_string()));
                        }
                    }
                }
            }
        }
        for port in released {
            self.ports.release(port);
        }
        // Ephemeral entries don't linger as `stopped` - delete on exit instead.
        for (project_id, command_id) in ephemeral_exited {
            if let Err(e) = self.remove_command(&project_id, &command_id) {
                log::warn!("reap_tick: could not remove ephemeral {project_id}:{command_id}: {e}");
            }
        }
        self.persist_pids();
    }

    /// Background sampler: refresh RAM + detected-port for every running proc and
    /// cache it on each `ManagedProc`. Runs on the reaper thread (off the UI
    /// thread). The one heavy enumeration + netstat happens here, NOT in `list()`.
    ///
    /// Snapshots the running pids under the lock, does the heavy compute with the
    /// lock released, then writes the results back. A proc that started between
    /// the snapshot and the write-back simply keeps its prior cache for one more
    /// tick; a proc that stopped has its cache cleared.
    pub fn sample_tick(&self) {
        let running: Vec<(String, u32, Option<u16>)> = {
            let guard = self.procs.lock().unwrap();
            guard
                .values()
                .filter_map(|p| p.pid.map(|pid| (p.spec.id.clone(), pid, p.acquired_port())))
                .collect()
        };
        let samples = {
            let mut sys = self.sampler_sys.lock().unwrap();
            crate::supervisor::sampler::sample(&mut sys, &running)
        };
        let mut guard = self.procs.lock().unwrap();
        for p in guard.values_mut() {
            if p.pid.is_none() {
                p.set_sample(None, None, None);
            } else if let Some(s) = samples.get(&p.spec.id) {
                p.set_sample(Some(s.mem), Some(s.cpu_pct), s.port);
            }
        }
    }
}
