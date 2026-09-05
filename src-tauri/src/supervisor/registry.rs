use super::config;
use super::proc::ManagedProc;
use super::proxy_hub::ProxyHub;
use crate::ports::PortRegistry;
use crate::types::{LogLine, ProcInfo, ProcSpec, Project};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use sysinfo::System;

mod stop;
pub(super) use stop::begin_stop_locked;
mod lifecycle;
mod sampling;

/// Owns every supervised process. `projects` is the persisted config (source of
/// truth); `procs` is the live runtime map keyed by composite `project/command` id.
///
/// This file holds the struct definition, construction, and the two cheap
/// read-only queries (`list`/`logs`). The four other Supervisor concerns are
/// each a further `impl Supervisor` block in a sibling module: process
/// lifecycle up (`lifecycle`: readopt/autostart/start/restart/reload) and down
/// (`stop`: stop/stop_all/shutdown_all), periodic reconciliation (`sampling`:
/// reap_tick/sample_tick), and config CRUD for projects/commands (`crud`,
/// alongside this module rather than under it). Fields are `pub(super)` so
/// those blocks can reach them.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{unit_id, Command, ProcKind, ProcSpec, ProcStatus};
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

    /// Unlike `command`'s instant-exit child, `stop_all` only acts on procs
    /// still holding a pid when it runs - so this test needs one that is
    /// reliably still alive to be killed, not racing its own natural exit.
    fn long_running_command(id: &str, kind: ProcKind) -> Command {
        Command {
            id: id.to_string(),
            name: id.to_string(),
            cmd: "cmd /C ping -n 30 127.0.0.1 >NUL".to_string(),
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
    fn stop_all_deletes_an_ephemeral_entry_but_keeps_generic_as_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let ports = Arc::new(PortRegistry::new(dir.path().to_path_buf()));
        let sup = Supervisor::new(dir.path().to_path_buf(), Arc::clone(&ports));

        let project = Project {
            id: "p".to_string(),
            name: "p".to_string(),
            root: ".".to_string(),
            commands: vec![
                long_running_command("eph", ProcKind::Ephemeral),
                long_running_command("gen", ProcKind::Generic),
            ],
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

        // Both children are still alive (long-running, not self-exited) when
        // stop_all runs, so it is stop_all itself doing the kill here - unlike
        // reap_tick's test, which only notices an exit that already happened.
        sup.stop_all();

        let projects = sup.list_projects();
        let proj = projects
            .iter()
            .find(|p| p.id == "p")
            .expect("project survives (gen command remains)");
        assert!(
            !proj.commands.iter().any(|c| c.id == "eph"),
            "ephemeral command's config entry must be deleted by stop_all, same as stop()"
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
