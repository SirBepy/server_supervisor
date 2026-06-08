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
                p.refresh();
                if had_pid && p.pid.is_none() {
                    if let Some(port) = p.acquired_port() {
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
        // One shared System refresh pass fills per-subtree RAM for running procs.
        super::mem::fill_memory(&mut out);
        out
    }

    pub fn logs(&self, id: &str) -> Result<Vec<LogLine>, String> {
        let guard = self.procs.lock().unwrap();
        guard
            .get(id)
            .map(|p| p.logs_snapshot())
            .ok_or_else(|| format!("unknown process id: {id}"))
    }

    pub fn start(&self, id: &str) -> Result<(), String> {
        // Acquire a probed, free port if this command opts in (before locking
        // procs, since acquire does OS work; reserve-before-spawn prevents races).
        let want_dynamic = {
            let guard = self.procs.lock().unwrap();
            guard
                .get(id)
                .map(|p| p.spec.use_dynamic_port)
                .unwrap_or(false)
        };
        let port = if want_dynamic {
            Some(self.acquire_free_port(id)?)
        } else {
            None
        };
        let res = {
            let mut guard = self.procs.lock().unwrap();
            let p = guard
                .get_mut(id)
                .ok_or_else(|| format!("unknown process id: {id}"))?;
            p.start(port).map_err(|e| e.to_string())
        };
        if res.is_err() {
            if let Some(p) = port {
                self.ports.release(p);
            }
        }
        res?;

        // EADDRINUSE retry-once (dynamic-port only). The registry bind-probes
        // before handing out a port, but a TOCTOU race can let another process
        // grab it between probe and spawn. If the child dies within ~1500ms with
        // EADDRINUSE in its logs, release that port and respawn once on a fresh
        // acquire. This blocks `start` ~1500ms for dynamic-port commands only;
        // start is user/AI-initiated (not a hot path), so that's acceptable.
        if let Some(p_port) = port {
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
                self.ports.release(p_port);
                log::warn!("supervisor: {id} hit EADDRINUSE on {p_port}, retrying once");
                let retry = self.acquire_free_port(id)?;
                let retry_res = {
                    let mut guard = self.procs.lock().unwrap();
                    if let Some(proc) = guard.get_mut(id) {
                        proc.start(Some(retry)).map_err(|e| e.to_string())
                    } else {
                        Ok(0)
                    }
                };
                if retry_res.is_err() {
                    self.ports.release(retry);
                }
                retry_res?;
            }
        }

        self.persist_pids();
        Ok(())
    }

    pub fn stop(&self, id: &str) -> Result<(), String> {
        let released;
        {
            let mut guard = self.procs.lock().unwrap();
            let p = guard
                .get_mut(id)
                .ok_or_else(|| format!("unknown process id: {id}"))?;
            released = p.acquired_port();
            p.stop();
        }
        if let Some(port) = released {
            self.ports.release(port);
        }
        self.persist_pids();
        Ok(())
    }

    pub fn restart(&self, id: &str) -> Result<(), String> {
        self.stop(id)?;
        self.start(id)
    }

    pub fn reload(&self, id: &str, full: bool) -> Result<(), String> {
        let mut guard = self.procs.lock().unwrap();
        let p = guard
            .get_mut(id)
            .ok_or_else(|| format!("unknown process id: {id}"))?;
        p.reload(full)
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
    fn reap_tick_notices_self_exited_child_without_a_list_call() {
        let dir = tempfile::tempdir().unwrap();
        let ports = Arc::new(PortRegistry::new(dir.path().to_path_buf()));
        let sup = Supervisor::new(dir.path().to_path_buf(), ports);

        // Start a process that exits on its own, bypassing list()/the UI poll.
        {
            let mut map = sup.procs.lock().unwrap();
            let mut p = ManagedProc::new(fast_exit_spec("p:c"));
            p.start(None).unwrap();
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
