use super::config;
use super::proc::ManagedProc;
use super::proxy_hub::ProxyHub;
use super::reaper::{self, PidEntry};
use crate::ports::PortRegistry;
use crate::types::{unit_id, LogLine, ProcInfo, ProcKind, ProcSpec, Project};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use sysinfo::System;

mod stop;
pub(super) use stop::begin_stop_locked;

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
    pub(super) ports: Arc<PortRegistry>,
    /// One reverse-proxy hub listener per project that has upstream presets
    /// configured (see `proxy_hub`), keyed by project id. Populated lazily
    /// (first preset added) and at startup via `init_hubs`.
    pub(super) hubs: Mutex<HashMap<String, ProxyHub>>,
    /// Persists across `sample_tick` calls (unlike the one-shot `sysstats`
    /// sampler) so per-process CPU usage has a prior reading to diff against -
    /// see `sampler.rs` module docs.
    sampler_sys: Mutex<System>,
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
            hubs: Mutex::new(HashMap::new()),
            sampler_sys: Mutex::new(System::new()),
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
        let samples = {
            let mut sys = self.sampler_sys.lock().unwrap();
            super::sampler::sample(&mut sys, &running)
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
    use crate::types::{Command, ProcKind, ProcSpec, ProcStatus};
    use std::fs;

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
            fixed_port: None,
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

    fn command(id: &str, kind: ProcKind) -> Command {
        Command {
            id: id.to_string(),
            name: id.to_string(),
            cmd: "cmd /C exit 0".to_string(), // trivial child, exits instantly
            kind,
            autostart: false,
            use_dynamic_port: false,
            fixed_port: None,
            env: String::new(),
            role: None,
        }
    }

    #[test]
    fn reap_tick_deletes_an_ephemeral_entry_on_exit_but_keeps_generic_as_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let ports = Arc::new(PortRegistry::new(dir.path().to_path_buf()));
        let sup = Supervisor::new(dir.path().to_path_buf(), Arc::clone(&ports));

        let project = Project {
            id: "p".to_string(),
            name: "p".to_string(),
            root: ".".to_string(),
            commands: vec![command("eph", ProcKind::Ephemeral), command("gen", ProcKind::Generic)],
            presets: Vec::new(),
            active_preset: None,
            transient: false,
            transient_label: None,
        };
        {
            let mut projects = sup.projects.lock().unwrap();
            projects.push(project.clone());
        }
        {
            let mut map = sup.procs.lock().unwrap();
            for c in &project.commands {
                let spec = ProcSpec::from_unit(&project, c);
                let mut p = ManagedProc::new(spec);
                p.start(None, None).unwrap();
                assert!(p.pid.is_some(), "freshly started proc has a pid");
                map.insert(unit_id("p", &c.id), p);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(400)); // let both exit

        // reap_tick is the ONLY refresh here - nothing calls list() first.
        sup.reap_tick();

        let projects = sup.list_projects();
        let proj = projects.iter().find(|p| p.id == "p").expect("project survives (gen command remains)");
        assert!(
            !proj.commands.iter().any(|c| c.id == "eph"),
            "ephemeral command's config entry must be deleted on exit"
        );
        assert!(
            proj.commands.iter().any(|c| c.id == "gen"),
            "generic command's config entry must be retained"
        );

        let info = sup.list();
        assert!(
            !info.iter().any(|i| i.id == "p:eph"),
            "ephemeral proc entry must be gone from list()"
        );
        let gen_info = info.iter().find(|i| i.id == "p:gen").expect("generic proc entry retained");
        assert_eq!(gen_info.status, ProcStatus::Stopped, "generic entry must be retained as stopped");
    }

    #[test]
    fn restart_keeps_an_ephemeral_entry_so_start_can_find_it() {
        let dir = tempfile::tempdir().unwrap();
        let ports = Arc::new(PortRegistry::new(dir.path().to_path_buf()));
        let sup = Supervisor::new(dir.path().to_path_buf(), Arc::clone(&ports));

        let project = Project {
            id: "p".to_string(),
            name: "p".to_string(),
            root: ".".to_string(),
            commands: vec![command("eph", ProcKind::Ephemeral)],
            presets: Vec::new(),
            active_preset: None,
            transient: false,
            transient_label: None,
        };
        {
            let mut projects = sup.projects.lock().unwrap();
            projects.push(project.clone());
        }
        {
            let mut map = sup.procs.lock().unwrap();
            let spec = ProcSpec::from_unit(&project, &project.commands[0]);
            let mut p = ManagedProc::new(spec);
            p.start(None, None).unwrap();
            map.insert(unit_id("p", "eph"), p);
        }

        sup.restart("p:eph").expect("restart must not delete the entry it is about to start");

        let projects = sup.list_projects();
        let proj = projects.iter().find(|p| p.id == "p").expect("project survives");
        assert!(
            proj.commands.iter().any(|c| c.id == "eph"),
            "restart must leave the ephemeral config entry in place"
        );

        sup.stop("p:eph").unwrap();
        let projects = sup.list_projects();
        assert!(
            !projects.iter().any(|p| p.commands.iter().any(|c| c.id == "eph")),
            "a plain stop still deletes the ephemeral entry"
        );
    }

    #[test]
    fn start_falls_back_to_a_dynamic_port_when_the_usual_one_is_occupied() {
        let dir = tempfile::tempdir().unwrap();
        let ports = Arc::new(PortRegistry::new(dir.path().to_path_buf()));
        let sup = Supervisor::new(dir.path().to_path_buf(), Arc::clone(&ports));

        let mut spec = fast_exit_spec("p:c");
        spec.use_dynamic_port = true;
        {
            let mut map = sup.procs.lock().unwrap();
            map.insert("p:c".to_string(), ManagedProc::new(spec));
        }
        // Learn the project's usual port the same way start() will - fetched
        // up front rather than assumed, so this test survives if the
        // allocator's search order ever changes.
        let usual = ports.project_port("p", "p:c", None).unwrap();

        // Occupy it so `is_os_port_free` reports it taken, forcing the
        // fallback path. An IPv4 bind alone is enough: `port_free` requires
        // BOTH loopbacks to succeed.
        use std::net::{Ipv4Addr, TcpListener};
        let _hold = TcpListener::bind((Ipv4Addr::LOCALHOST, usual)).unwrap();

        sup.start("p:c").unwrap();

        let info = sup.list().into_iter().find(|p| p.id == "p:c").unwrap();
        assert_ne!(info.port, Some(usual), "must not have bound the occupied usual port");
        assert!(info.fallback_port, "dashboard must be told this is not the usual port");
    }

    fn transient_project(id: &str, cmd_id: &str) -> Project {
        Project {
            id: id.to_string(),
            name: id.to_string(),
            root: ".".to_string(),
            commands: vec![Command {
                id: cmd_id.to_string(),
                name: cmd_id.to_string(),
                cmd: "cmd /C exit 0".to_string(),
                kind: ProcKind::Generic,
                autostart: false,
                use_dynamic_port: false,
                fixed_port: None,
                env: String::new(),
                role: None,
            }],
            presets: Vec::new(),
            active_preset: None,
            transient: true,
            transient_label: Some(id.to_string()),
        }
    }

    #[test]
    fn readopt_orphans_prunes_dead_transients_but_keeps_live_ones() {
        let dir = tempfile::tempdir().unwrap();
        let ports = Arc::new(PortRegistry::new(dir.path().to_path_buf()));
        let sup = Supervisor::new(dir.path().to_path_buf(), Arc::clone(&ports));

        let dead = transient_project("dead", "c");
        let alive = transient_project("alive", "c");
        {
            let mut projects = sup.projects.lock().unwrap();
            projects.push(dead);
            projects.push(alive.clone());
        }
        {
            let mut map = sup.procs.lock().unwrap();
            let spec = ProcSpec::from_unit(&alive, &alive.commands[0]);
            let mut p = ManagedProc::new(spec);
            p.pid = Some(999); // stand-in for a still-alive re-adopted process
            map.insert(unit_id("alive", "c"), p);
        }

        sup.readopt_orphans();

        let projects = sup.list_projects();
        assert!(!projects.iter().any(|p| p.id == "dead"), "dead transient with no live process is pruned");
        assert!(projects.iter().any(|p| p.id == "alive"), "transient with a live process survives");

        let transient_file = fs::read_to_string(dir.path().join("transient_projects.json")).unwrap();
        assert!(transient_file.contains("\"alive\""));
        assert!(!transient_file.contains("\"dead\""));
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
