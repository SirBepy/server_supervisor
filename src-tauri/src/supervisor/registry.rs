use super::config;
use super::proc::ManagedProc;
use super::reaper::{self, PidEntry};
use crate::ports::PortRegistry;
use crate::types::{LogLine, ProcInfo, ProcSpec, Project};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Owns every supervised process. `projects` is the persisted config (source of
/// truth); `procs` is the live runtime map keyed by composite `project/command` id.
///
/// This file holds the runtime-control half (start/stop/restart/list/logs and
/// lifecycle). The config-CRUD half (projects + commands) is a second
/// `impl Supervisor` block in the sibling `crud` module; the fields are
/// `pub(super)` so that block can reach them.
pub struct Supervisor {
    pub(super) projects: Mutex<Vec<Project>>,
    pub(super) procs: Mutex<HashMap<String, ManagedProc>>,
    pub(super) data_dir: PathBuf,
    ports: Arc<PortRegistry>,
}

impl Supervisor {
    pub fn new(data_dir: PathBuf, ports: Arc<PortRegistry>) -> Self {
        let _ = std::fs::create_dir_all(&data_dir);
        let projects = config::load(&data_dir);
        let mut map = HashMap::new();
        for project in &projects {
            ensure_procs(&mut map, project);
        }
        Self {
            projects: Mutex::new(projects),
            procs: Mutex::new(map),
            data_dir,
            ports,
        }
    }

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
    }

    /// Stop every running process but keep the app alive (tray "Close Processes").
    /// Distinct from `shutdown_all`, which is the kill-then-exit path.
    pub fn stop_all(&self) {
        let ids: Vec<String> = {
            let guard = self.procs.lock().unwrap();
            guard
                .iter()
                .filter(|(_, p)| p.pid.is_some())
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in ids {
            let _ = self.stop(&id);
        }
    }

    /// Backend reconcile pass, meant to run on a timer rather than only when
    /// `list()` is called from the UI/API. When the window is hidden to the tray
    /// and nothing polls, nothing else refreshes the procs - so a crashed child
    /// would keep a stale Running status, its dynamic port would leak in the
    /// registry, and pids.json would name a dead PID the next launch might wrongly
    /// re-adopt. This refreshes every proc, releases the port of any that just
    /// transitioned out of running on its own, and rewrites pids.json.
    pub fn reap_tick(&self) {
        let released: Vec<u16> = {
            let mut guard = self.procs.lock().unwrap();
            let mut released = Vec::new();
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
                }
            }
            released
        };
        for port in released {
            self.ports.release(port);
        }
        self.persist_pids();
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

    // ----- runtime control (by composite id) -----

    /// Cheap, UI-poll-safe snapshot. Only does per-proc `try_wait` liveness +
    /// field clones; RAM and detected-port are read from the cache that the
    /// background `sample_tick` fills. This used to enumerate the whole process
    /// table twice and shell out to netstat on the main thread every poll, which
    /// was the source of the window-drag / click lag.
    pub fn list(&self) -> Vec<ProcInfo> {
        let mut guard = self.procs.lock().unwrap();
        let mut out: Vec<ProcInfo> = guard
            .values_mut()
            .map(|p| {
                p.refresh();
                p.info()
            })
            .collect();
        out.sort_by(|a, b| (&a.project, &a.name).cmp(&(&b.project, &b.name)));
        out
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
        let samples = super::sampler::sample(&running);
        let mut guard = self.procs.lock().unwrap();
        for p in guard.values_mut() {
            if p.pid.is_none() {
                p.set_sample(None, None);
            } else if let Some(s) = samples.get(&p.spec.id) {
                p.set_sample(Some(s.mem), s.port);
            }
        }
    }

    /// Stable per-machine profile dir for the dedicated flutter-web dev browser
    /// (Chromium launched with `--disable-web-security`). Reused across sessions
    /// so clicked ports keep opening as tabs in the same window.
    pub fn dev_browser_profile_dir(&self) -> PathBuf {
        self.data_dir.join("dev-browser-profile")
    }

    pub fn logs(&self, id: &str) -> Result<Vec<LogLine>, String> {
        let guard = self.procs.lock().unwrap();
        guard
            .get(id)
            .map(|p| p.logs_snapshot())
            .ok_or_else(|| format!("unknown process id: {id}"))
    }

    pub fn start(&self, id: &str) -> Result<(), String> {
        // No-op guard: a start on an already Running/Starting proc must acquire
        // NOTHING, mirroring ManagedProc::start's own early return. Without this,
        // we would acquire ports below that ManagedProc::start then ignores (its
        // guard returns before using them), and the cleanup path only releases on
        // error - permanently leaking 1 port (generic) or 2 (proxied flutter) per
        // redundant start. Check status under the same procs lock used for lookup,
        // and return early BEFORE any acquire so the acquired set is unchanged.
        let (want_dynamic, wants_proxy) = {
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
            (p.spec.use_dynamic_port, p.wants_proxy())
        };
        // Acquire a probed, free port if this command opts in (before locking
        // procs, since acquire does OS work; reserve-before-spawn prevents races).
        // `port` is the port the child binds (`{PORT}`/PORT env). `public` is the
        // port the dashboard advertises. For a normal proc they are the same. For
        // a proxied flutter web-server, the child binds a fresh INTERNAL port and
        // the live-reload proxy fronts it on the public dynamic port - so we keep
        // the original dynamic port as the proxy's public port and acquire a
        // second ephemeral port for flutter itself.
        let port = if want_dynamic {
            Some(self.acquire_free_port(id)?)
        } else {
            None
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
            p.start(child_port, public_port).map_err(|e| e.to_string())
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
                        proc.start(Some(retry), public_port).map_err(|e| e.to_string())
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

    pub fn stop(&self, id: &str) -> Result<(), String> {
        let released;
        let released_internal;
        {
            let mut guard = self.procs.lock().unwrap();
            let p = guard
                .get_mut(id)
                .ok_or_else(|| format!("unknown process id: {id}"))?;
            // Capture both ports BEFORE stop() clears them. `acquired_port` is the
            // public port (proxy port when proxied); `internal_port` is the extra
            // ephemeral port the child bound behind the proxy, Some only then.
            released = p.acquired_port();
            released_internal = p.internal_port();
            p.stop();
        }
        if let Some(port) = released {
            self.ports.release(port);
        }
        // Release the internal port too. It is always distinct from the public
        // port (separate acquire), so no double-release risk.
        if let Some(port) = released_internal {
            self.ports.release(port);
        }
        self.persist_pids();
        Ok(())
    }

    pub fn restart(&self, id: &str) -> Result<(), String> {
        self.stop(id)?;
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

    pub fn shutdown_all(&self) {
        {
            let mut guard = self.procs.lock().unwrap();
            for p in guard.values_mut() {
                if p.pid.is_some() {
                    p.stop();
                }
            }
        }
        reaper::write_pids(&self.data_dir, &[]);
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

    fn persist_pids(&self) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ProcKind, ProcSpec};

    fn fast_exit_spec(id: &str) -> ProcSpec {
        ProcSpec {
            id: id.to_string(),
            project: "p".to_string(),
            name: "c".to_string(),
            cmd: "cmd /C exit 0".to_string(), // trivial child, exits instantly
            cwd: ".".to_string(),
            kind: ProcKind::Generic,
            autostart: false,
            use_dynamic_port: false,
            env: String::new(),
        }
    }

    #[test]
    fn redundant_start_on_running_proc_acquires_no_port() {
        // Fix 1: a start() on an already-Running, dynamic-port proc must acquire
        // ZERO ports. Previously start() acquired (and for a proxy, double-
        // acquired) before ManagedProc::start's own Running guard returned without
        // using them, and cleanup only released on error - leaking the port(s).
        let dir = tempfile::tempdir().unwrap();
        let ports = Arc::new(PortRegistry::new(dir.path().to_path_buf()));
        let sup = Supervisor::new(dir.path().to_path_buf(), Arc::clone(&ports));

        // Insert a proc whose status is Running but that owns no Child and is not
        // adopted: refresh() is a no-op on such a proc (the Child branch is skipped
        // and the adopted branch is skipped), so it stays Running through the
        // guard's refresh - standing in for a genuinely-running proc without
        // spawning a long-lived child. use_dynamic_port=true so the OLD code would
        // have acquired (and leaked) a port on each redundant start.
        {
            let mut map = sup.procs.lock().unwrap();
            let mut spec = fast_exit_spec("p:c");
            spec.use_dynamic_port = true;
            let mut p = ManagedProc::new(spec);
            p.status = crate::types::ProcStatus::Running;
            map.insert("p:c".to_string(), p);
        }

        // Probe the next port acquire() would hand out, then release it: this is
        // the canary. If start() leaks a port, this canary will no longer be free.
        let canary = ports.acquire().unwrap();
        ports.release(canary);

        // Redundant starts on the already-Running proc.
        sup.start("p:c").unwrap();
        sup.start("p:c").unwrap();

        // The same canary port must still be the next one handed out: zero net
        // acquires happened across both redundant starts.
        let after = ports.acquire().unwrap();
        assert_eq!(
            after, canary,
            "redundant start on a Running proc must acquire no port"
        );
    }

    #[test]
    fn reap_tick_notices_self_exited_child_without_a_list_call() {
        let dir = tempfile::tempdir().unwrap();
        let ports = Arc::new(PortRegistry::new(dir.path().to_path_buf()));
        let sup = Supervisor::new(dir.path().to_path_buf(), ports);

        // Start a process that exits on its own, bypassing list()/the UI poll.
        {
            let mut map = sup.procs.lock().unwrap();
            let mut p = ManagedProc::new(fast_exit_spec("p:c"));
            p.start(None, None).unwrap();
            assert!(p.pid.is_some(), "freshly started proc has a pid");
            map.insert("p:c".to_string(), p);
        }
        std::thread::sleep(std::time::Duration::from_millis(400)); // let it exit

        // reap_tick is the ONLY refresh here - nothing calls list().
        sup.reap_tick();

        {
            let map = sup.procs.lock().unwrap();
            assert!(
                map.get("p:c").unwrap().pid.is_none(),
                "reap_tick must notice the exit and clear the pid"
            );
        }
        // pids.json must not keep the dead proc's PID around to be re-adopted.
        let pids = std::fs::read_to_string(dir.path().join("pids.json")).unwrap_or_default();
        assert!(!pids.contains("p:c"), "stale pid must be pruned from pids.json");
    }
}

/// Insert a ManagedProc for each of the project's commands that isn't already
/// tracked. Existing (possibly running) entries are left untouched.
fn ensure_procs(map: &mut HashMap<String, ManagedProc>, project: &Project) {
    for c in &project.commands {
        let spec = ProcSpec::from_unit(project, c);
        map.entry(spec.id.clone())
            .or_insert_with(|| ManagedProc::new(spec));
    }
}
