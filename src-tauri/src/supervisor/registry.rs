use super::config;
use super::proc::ManagedProc;
use super::reaper::{self, PidEntry};
use crate::ports::PortRegistry;
use crate::types::{unit_id, Command, LogLine, ProcInfo, ProcKind, ProcSpec, Project};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Owns every supervised process. `projects` is the persisted config (source of
/// truth); `procs` is the live runtime map keyed by composite `project/command` id.
pub struct Supervisor {
    projects: Mutex<Vec<Project>>,
    procs: Mutex<HashMap<String, ManagedProc>>,
    data_dir: PathBuf,
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

    pub fn ports(&self) -> &Arc<PortRegistry> {
        &self.ports
    }

    pub fn reconcile_orphans(&self) {
        reaper::reconcile(&self.data_dir);
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

    // ----- config CRUD (mutates projects + runtime map, persists) -----

    pub fn list_projects(&self) -> Vec<Project> {
        self.projects.lock().unwrap().clone()
    }

    pub fn add_project(&self, name: String, root: String) -> Result<Project, String> {
        let name = name.trim().to_string();
        let root = root.trim().to_string();
        if name.is_empty() || root.is_empty() {
            return Err("project name and root are required".to_string());
        }
        let mut projects = self.projects.lock().unwrap();
        let id = unique_id(&name, &|cand| projects.iter().any(|p| p.id == cand));
        let project = Project {
            id,
            name,
            root,
            commands: Vec::new(),
        };
        projects.push(project.clone());
        config::save(&self.data_dir, &projects);
        Ok(project)
    }

    pub fn remove_project(&self, project_id: &str) -> Result<(), String> {
        let mut projects = self.projects.lock().unwrap();
        let idx = projects
            .iter()
            .position(|p| p.id == project_id)
            .ok_or_else(|| format!("unknown project: {project_id}"))?;
        let removed = projects.remove(idx);
        config::save(&self.data_dir, &projects);
        drop(projects);

        let mut map = self.procs.lock().unwrap();
        for c in &removed.commands {
            if let Some(mut proc) = map.remove(&unit_id(&removed.id, &c.id)) {
                proc.stop();
            }
        }
        Ok(())
    }

    pub fn add_command(
        &self,
        project_id: &str,
        name: String,
        cmd: String,
        kind: ProcKind,
        autostart: bool,
    ) -> Result<Command, String> {
        let name = name.trim().to_string();
        let cmd = cmd.trim().to_string();
        if name.is_empty() || cmd.is_empty() {
            return Err("command name and cmd are required".to_string());
        }
        let mut projects = self.projects.lock().unwrap();
        let project = projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .ok_or_else(|| format!("unknown project: {project_id}"))?;
        let cid = unique_id(&name, &|cand| project.commands.iter().any(|c| c.id == cand));
        let command = Command {
            id: cid,
            name,
            cmd,
            kind,
            autostart,
            use_dynamic_port: false,
        };
        project.commands.push(command.clone());
        let project_snapshot = project.clone();
        config::save(&self.data_dir, &projects);
        drop(projects);

        let mut map = self.procs.lock().unwrap();
        let spec = ProcSpec::from_unit(&project_snapshot, &command);
        map.entry(spec.id.clone())
            .or_insert_with(|| ManagedProc::new(spec));
        Ok(command)
    }

    pub fn remove_command(&self, project_id: &str, command_id: &str) -> Result<(), String> {
        let mut projects = self.projects.lock().unwrap();
        let project = projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .ok_or_else(|| format!("unknown project: {project_id}"))?;
        let before = project.commands.len();
        project.commands.retain(|c| c.id != command_id);
        if project.commands.len() == before {
            return Err(format!("unknown command: {command_id}"));
        }
        config::save(&self.data_dir, &projects);
        drop(projects);

        let mut map = self.procs.lock().unwrap();
        if let Some(mut proc) = map.remove(&unit_id(project_id, command_id)) {
            proc.stop();
        }
        Ok(())
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
                    })
                })
                .collect()
        };
        reaper::write_pids(&self.data_dir, &entries);
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

fn unique_id(base: &str, taken: &dyn Fn(&str) -> bool) -> String {
    let b = config::slug(base);
    if !taken(&b) {
        return b;
    }
    let mut n = 2;
    loop {
        let cand = format!("{b}-{n}");
        if !taken(&cand) {
            return cand;
        }
        n += 1;
    }
}
