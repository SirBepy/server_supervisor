//! Startup adoption, autostart, and the `start`/`restart`/`reload` control
//! paths - the "bring a process up" half of the Supervisor's lifecycle. The
//! "bring it down" half (`stop`/`stop_all`/`shutdown_all`) lives in the
//! sibling `stop` module; the periodic reconciliation ticks that notice a
//! process going down on its own live in `sampling`.
//!
//! ~320 lines, just past the ~300 split guideline, almost entirely `start()`
//! itself: one lock-guarded control-flow sequence (no-op guard, port resolve,
//! spawn, proxy-fallback cleanup, EADDRINUSE retry) whose steps depend on each
//! other's local state. Extracting any step into its own function/file would
//! mean threading that state across the split instead of removing it.

use super::Supervisor;
use crate::supervisor::config;
use crate::supervisor::reaper::{self, PidEntry};
use crate::types::unit_id;

impl Supervisor {
    /// On startup, re-adopt processes that survived a prior app instance instead
    /// of killing them. For each persisted PID still alive as our `cmd.exe`
    /// wrapper whose command still exists in config, mark its ManagedProc Running
    /// (adopted) and restore its port. Dead or unknown PIDs are skipped (left
    /// Stopped). Then rewrite pids.json to reflect what we actually adopted.
    pub fn readopt_orphans(&self) {
        let entries = reaper::read_pids(&self.data_dir);
        {
            let mut map = self.procs.lock().unwrap();
            for e in &entries {
                if !reaper::pid_is_our_wrapper(e.pid) {
                    continue; // dead, or PID reused by something else
                }
                if let Some(proc) = map.get_mut(&e.id) {
                    proc.adopt(e.pid, e.started_at, e.port);
                    if let Some(port) = e.port {
                        self.ports.mark_acquired(port);
                    }
                    log::info!("supervisor: re-adopted {} pid={}", e.id, e.pid);
                }
            }
        }
        // Rewrite pids.json from the live (now-adopted) set.
        self.persist_pids();
        self.prune_dead_transients();
    }

    /// A transient project (worktree/scratch run, see `Project::transient`)
    /// with no live process is a dead entry from a prior instance - drop it so
    /// it doesn't linger forever in `transient_projects.json`. One with a live
    /// process (just re-adopted above) survives, so it stays stoppable.
    fn prune_dead_transients(&self) {
        let mut projects = self.projects.lock().unwrap();
        let procs = self.procs.lock().unwrap();
        let mut dropped: Vec<String> = Vec::new();
        projects.retain(|p| {
            let keep = !p.transient
                || p.commands
                    .iter()
                    .any(|c| procs.get(&unit_id(&p.id, &c.id)).map_or(false, |proc| proc.pid.is_some()));
            if !keep {
                dropped.push(p.id.clone());
            }
            keep
        });
        drop(procs);
        // Same reclaim remove_project does: a dropped project's block marker and
        // per-command slots would otherwise stay reserved forever, and a throwaway
        // worktree mints a fresh project id every run.
        for id in &dropped {
            self.ports.release_project(id);
        }
        config::save(&self.data_dir, &projects);
    }

    pub fn start_autostart(&self) {
        let ids: Vec<String> = {
            let guard = self.procs.lock().unwrap();
            guard
                .values()
                .filter(|p| p.spec.autostart)
                .map(|p| p.spec.id.clone())
                .collect()
        };
        for id in ids {
            if let Err(e) = self.start(&id) {
                log::error!("supervisor: autostart failed for {id}: {e}");
            }
        }
    }

    pub fn start(&self, id: &str) -> Result<(), String> {
        // No-op guard: a start on an already Running/Starting proc must acquire
        // NOTHING, mirroring ManagedProc::start's own early return. Without this,
        // we would acquire ports below that ManagedProc::start then ignores (its
        // guard returns before using them), and the cleanup path only releases on
        // error - permanently leaking 1 port (generic) or 2 (proxied flutter) per
        // redundant start. Check status under the same procs lock used for lookup,
        // and return early BEFORE any acquire so the acquired set is unchanged.
        let (want_dynamic, wants_proxy, fixed_port) = {
            let mut guard = self.procs.lock().unwrap();
            let p = guard
                .get_mut(id)
                .ok_or_else(|| format!("unknown process id: {id}"))?;
            // Refresh first so a since-crashed proc is seen as not-Running (mirrors
            // ManagedProc::start, which refreshes before its own Running guard).
            p.refresh();
            if matches!(
                p.status,
                crate::types::ProcStatus::Running | crate::types::ProcStatus::Starting
            ) {
                return Ok(()); // already up: zero net port acquires
            }
            (p.spec.use_dynamic_port, p.wants_proxy(), p.spec.fixed_port)
        };
        // Resolve the port this command should bind (before locking procs,
        // since this does OS work; reserve-before-spawn prevents races).
        // `port` is the port the child binds (`{PORT}`/PORT env). `public` is the
        // port the dashboard advertises. For a normal proc they are the same. For
        // a proxied flutter web-server, the child binds a fresh INTERNAL port and
        // the live-reload proxy fronts it on the public dynamic port - so we keep
        // the original dynamic port as the proxy's public port and acquire a
        // second ephemeral port for flutter itself.
        let (port, fallback) = if want_dynamic {
            self.resolve_project_port(id, fixed_port)?
        } else {
            (None, false)
        };
        let proxy_public = if wants_proxy && want_dynamic {
            // Re-route: the originally-acquired `port` becomes the PUBLIC port; a
            // fresh internal port is what the child binds.
            let public = port;
            let internal = match self.acquire_free_port(id) {
                Ok(p) => p,
                Err(e) => {
                    if let Some(p) = public {
                        self.ports.release(p);
                    }
                    return Err(e);
                }
            };
            (public, Some(internal)) // (public_for_proxy, internal_for_child)
        } else {
            (None, port) // no proxy: child binds `port`, nothing fronts it
        };
        let (public_port, child_port) = proxy_public;
        let res = {
            let mut guard = self.procs.lock().unwrap();
            let p = guard
                .get_mut(id)
                .ok_or_else(|| format!("unknown process id: {id}"))?;
            let r = p.start(child_port, public_port).map_err(|e| e.to_string());
            if r.is_ok() {
                p.set_fallback_port(fallback);
            }
            r
        };
        if res.is_err() {
            if let Some(p) = child_port {
                self.ports.release(p);
            }
            if let Some(p) = public_port {
                self.ports.release(p);
            }
        }
        res?;

        // Proxy-fallback cleanup: when this proc was meant to be proxied (both
        // public_port and the internal child_port were acquired) but the proxy
        // could not bind, flutter fell back to the public port and the internal
        // port went unused. Detect that via the proc no longer reporting an
        // internal port, and release the orphaned internal acquisition.
        if let (Some(_public), Some(internal)) = (public_port, child_port) {
            let proxied = {
                let guard = self.procs.lock().unwrap();
                guard.get(id).and_then(|p| p.internal_port()).is_some()
            };
            if !proxied {
                self.ports.release(internal);
            }
        }

        // EADDRINUSE retry-once (dynamic-port only). The registry bind-probes
        // before handing out a port, but a TOCTOU race can let another process
        // grab it between probe and spawn. If the child dies within ~1500ms with
        // EADDRINUSE in its logs, release that port and respawn once on a fresh
        // acquire. This blocks `start` ~1500ms for dynamic-port commands only;
        // start is user/AI-initiated (not a hot path), so that's acceptable.
        if let Some(p_port) = child_port {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let crashed_addrinuse = {
                let mut guard = self.procs.lock().unwrap();
                if let Some(proc) = guard.get_mut(id) {
                    proc.refresh();
                    proc.status == crate::types::ProcStatus::Crashed
                        && proc
                            .logs_snapshot()
                            .iter()
                            .any(|l| l.text.contains("EADDRINUSE"))
                } else {
                    false
                }
            };
            if crashed_addrinuse {
                // Only the child (internal) port conflicted; the public proxy port
                // is kept and reused. Release the dead child port, acquire a fresh
                // one, and respawn with the same public port.
                self.ports.release(p_port);
                log::warn!("supervisor: {id} hit EADDRINUSE on {p_port}, retrying once");
                let retry = self.acquire_free_port(id)?;
                let retry_res = {
                    let mut guard = self.procs.lock().unwrap();
                    if let Some(proc) = guard.get_mut(id) {
                        let r = proc.start(Some(retry), public_port).map_err(|e| e.to_string());
                        if r.is_ok() && public_port.is_none() {
                            // Non-proxied: child_port IS the advertised port, and it
                            // just changed to a fresh ephemeral acquisition - flag
                            // this run as not on its usual port. (Proxied: only the
                            // hidden internal port retried, the public port and its
                            // fallback status are unaffected.)
                            proc.set_fallback_port(true);
                        }
                        r
                    } else {
                        Ok(0)
                    }
                };
                if retry_res.is_err() {
                    self.ports.release(retry);
                    if let Some(pp) = public_port {
                        self.ports.release(pp);
                    }
                }
                retry_res?;
            }
        }

        self.persist_pids();
        Ok(())
    }

    pub fn restart(&self, id: &str) -> Result<(), String> {
        self.stop_inner(id, false)?;
        self.start(id)
    }

    /// Try a fast hot restart via the Flutter daemon. If the daemon is not ready
    /// (no appId / no stdin seen yet), transparently fall back to a full process
    /// restart so the caller always gets a working reload. The proc lock MUST be
    /// released before calling restart() to avoid a self-deadlock.
    pub fn reload(&self, id: &str, full: bool) -> Result<(), String> {
        let res = {
            let mut guard = self.procs.lock().unwrap();
            let p = guard
                .get_mut(id)
                .ok_or_else(|| format!("unknown process id: {id}"))?;
            p.reload(full)
            // guard dropped here, before any restart below.
        };
        match res {
            Ok(()) => {
                log::info!("supervisor: {id} fast reload via daemon");
                Ok(())
            }
            Err(e)
                if e.contains("daemon not ready")
                    || e.contains("no appId")
                    || e.contains("no stdin") =>
            {
                log::info!("supervisor: {id} daemon not ready, falling back to full restart");
                self.restart(id)
            }
            Err(e) => Err(e),
        }
    }

    /// Resolve the port a dynamic-port command should bind: its stable
    /// project-block (or manually overridden) port, falling back to the
    /// existing ephemeral `acquire()` path if that port is occupied right now
    /// - e.g. a second instance of the same project, or something unrelated
    /// squatting it - so the command still starts rather than failing. The
    /// bool is true exactly when the fallback path was taken.
    fn resolve_project_port(&self, id: &str, fixed_port: Option<u16>) -> Result<(Option<u16>, bool), String> {
        let project_id = id.split(':').next().unwrap_or(id);
        let usual = self.ports.project_port(project_id, id, fixed_port)?;
        if self.ports.is_os_port_free(usual) {
            Ok((Some(usual), false))
        } else {
            log::warn!(
                "supervisor: {id}'s usual port {usual} is occupied; falling back to a dynamic port"
            );
            Ok((Some(self.acquire_free_port(id)?), true))
        }
    }

    /// Acquire a free port from the registry, logging if the OS reports it as
    /// held by another process despite the registry's bind-probe (rare race).
    fn acquire_free_port(&self, id: &str) -> Result<u16, String> {
        let port = self.ports.acquire()?;
        if let Some(holder) = reaper::port_holder(port) {
            log::warn!(
                "supervisor: port {port} appears held by {holder} despite probe; using it for {id} anyway"
            );
        }
        Ok(port)
    }

    /// `pub(super)`, not private: `stop`/`stop_all`/`shutdown_all` (sibling
    /// `stop` module) and `reap_tick` (sibling `sampling` module) all call
    /// this after their own kill/refresh pass.
    pub(super) fn persist_pids(&self) {
        let entries: Vec<PidEntry> = {
            let guard = self.procs.lock().unwrap();
            guard
                .values()
                .filter_map(|p| {
                    p.pid.map(|pid| PidEntry {
                        id: p.spec.id.clone(),
                        pid,
                        started_at: p.started_at.unwrap_or(0),
                        port: p.acquired_port(),
                    })
                })
                .collect()
        };
        reaper::write_pids(&self.data_dir, &entries);
    }
}
