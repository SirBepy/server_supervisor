use super::config;
use super::proc::ManagedProc;
use super::reaper::{self, PidEntry};
use crate::types::{unit_id, Command, LogLine, ProcInfo, ProcKind, ProcSpec, Project};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Owns every supervised process. `projects` is the persisted config (source of
/// truth); `procs` is the live runtime map keyed by composite `project/command` id.
pub struct Supervisor {
    projects: Mutex<Vec<Project>>,
    procs: Mutex<HashMap<String, ManagedProc>>,
    data_dir: PathBuf,
}

impl Supervisor {
    pub fn new(data_dir: PathBuf) -> Self {
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
        }
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
        {
            let mut guard = self.procs.lock().unwrap();
            let p = guard
                .get_mut(id)
                .ok_or_else(|| format!("unknown process id: {id}"))?;
            p.start().map_err(|e| e.to_string())?;
        }
        self.persist_pids();
        Ok(())
    }

    pub fn stop(&self, id: &str) -> Result<(), String> {
        {
            let mut guard = self.procs.lock().unwrap();
            let p = guard
                .get_mut(id)
                .ok_or_else(|| format!("unknown process id: {id}"))?;
            p.stop();
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
