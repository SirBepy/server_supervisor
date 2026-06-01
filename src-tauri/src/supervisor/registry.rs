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
        // Idempotent on the folder: same canonical path -> reuse the existing
        // project unchanged (keep its name; ignore the re-entered one). No dup.
        if let Some(existing) = projects.iter().find(|p| same_path(&p.root, &root)) {
            return Ok(existing.clone());
        }
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

    /// Add a command. `kind` is normally inferred from the command string
    /// (`None`); an explicit `Some(kind)` overrides inference (used by the `/run`
    /// API when a caller knows better).
    pub fn add_command(
        &self,
        project_id: &str,
        name: String,
        cmd: String,
        kind: Option<ProcKind>,
        autostart: bool,
        use_dynamic_port: bool,
        env: String,
    ) -> Result<Command, String> {
        let name = name.trim().to_string();
        let cmd = cmd.trim().to_string();
        if name.is_empty() || cmd.is_empty() {
            return Err("command name and cmd are required".to_string());
        }
        let kind = kind.unwrap_or_else(|| ProcKind::infer(&cmd));
        let mut projects = self.projects.lock().unwrap();
        let project = projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .ok_or_else(|| format!("unknown project: {project_id}"))?;
        // Idempotent on the exact cmd string within this project: if a command
        // with the same cmd already exists, return it (the runtime procs map
        // already holds its entry, so don't re-insert).
        if let Some(existing) = project.commands.iter().find(|c| c.cmd == cmd) {
            return Ok(existing.clone());
        }
        let cid = unique_id(&name, &|cand| project.commands.iter().any(|c| c.id == cand));
        let command = Command {
            id: cid,
            name,
            cmd,
            kind,
            autostart,
            use_dynamic_port,
            env,
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

    /// Register a project (by folder) + a command (by cmd string) if not already
    /// present - both are idempotent - then start it and return its ProcInfo.
    /// The composite used by the `POST /run` API for one-call server launch.
    pub fn ensure_and_run(
        &self,
        root: &str,
        cmd: &str,
        name: Option<String>,
        kind: Option<ProcKind>,
        use_dynamic_port: bool,
        env: String,
    ) -> Result<ProcInfo, String> {
        let project_name = std::path::Path::new(root)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(root)
            .to_string();
        let project = self.add_project(project_name, root.to_string())?;
        let command_name = name.unwrap_or_else(|| derive_name(cmd));
        let command = self.add_command(
            &project.id,
            command_name,
            cmd.to_string(),
            kind,
            false,
            use_dynamic_port,
            env,
        )?;
        let id = unit_id(&project.id, &command.id);
        self.start(&id)?;
        self.list()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("started but not found in list: {id}"))
    }

    /// Edit an existing command in place. The command `id` is a stable handle
    /// (it keys the runtime procs map, the captured logs, and the API path), so
    /// it never changes here - only the mutable fields do. The runtime
    /// `ManagedProc` is mutated in place (preserving its log buffer and any live
    /// child handle). If the process is running and the edit changes a field the
    /// spawn depends on (cmd, cwd, kind, dynamic-port), it is restarted so the
    /// live process reflects the edit.
    pub fn update_command(
        &self,
        project_id: &str,
        command_id: &str,
        name: String,
        cmd: String,
        autostart: bool,
        use_dynamic_port: bool,
        env: String,
    ) -> Result<Command, String> {
        let name = name.trim().to_string();
        let cmd = cmd.trim().to_string();
        if name.is_empty() || cmd.is_empty() {
            return Err("command name and cmd are required".to_string());
        }
        // Kind is always inferred from the command string (no manual picker).
        let kind = ProcKind::infer(&cmd);
        // A running command is locked: editing it would silently relaunch the
        // live process. Require the caller to stop it first. Refresh so a child
        // that already exited on its own does not count as running.
        {
            let mut map = self.procs.lock().unwrap();
            if let Some(proc) = map.get_mut(&unit_id(project_id, command_id)) {
                proc.refresh();
                if proc.pid.is_some() {
                    return Err("stop the command before editing it".to_string());
                }
            }
        }
        let (updated, project_snapshot) = {
            let mut projects = self.projects.lock().unwrap();
            let project = projects
                .iter_mut()
                .find(|p| p.id == project_id)
                .ok_or_else(|| format!("unknown project: {project_id}"))?;
            let command = project
                .commands
                .iter_mut()
                .find(|c| c.id == command_id)
                .ok_or_else(|| format!("unknown command: {command_id}"))?;
            command.name = name;
            command.cmd = cmd;
            command.kind = kind;
            command.autostart = autostart;
            command.use_dynamic_port = use_dynamic_port;
            command.env = env;
            let updated = command.clone();
            let snapshot = project.clone();
            config::save(&self.data_dir, &projects);
            (updated, snapshot)
        };

        let new_spec = ProcSpec::from_unit(&project_snapshot, &updated);
        let id = new_spec.id.clone();
        let restart_needed = {
            let mut map = self.procs.lock().unwrap();
            match map.get_mut(&id) {
                Some(proc) => {
                    let affects_running = proc.spec.cmd != new_spec.cmd
                        || proc.spec.cwd != new_spec.cwd
                        || proc.spec.kind != new_spec.kind
                        || proc.spec.use_dynamic_port != new_spec.use_dynamic_port
                        || proc.spec.env != new_spec.env;
                    let running = proc.pid.is_some();
                    proc.spec = new_spec;
                    running && affects_running
                }
                None => {
                    // Defensive: every command should already have a runtime entry,
                    // but if not, create one so the edit is at least startable.
                    map.insert(id.clone(), ManagedProc::new(new_spec));
                    false
                }
            }
        };
        if restart_needed {
            self.restart(&id)?;
        }
        Ok(updated)
    }

    pub fn remove_command(&self, project_id: &str, command_id: &str) -> Result<(), String> {
        // Same lock as edit: a running command must be stopped before it can be
        // removed, so deletion never races a live child.
        {
            let mut map = self.procs.lock().unwrap();
            if let Some(proc) = map.get_mut(&unit_id(project_id, command_id)) {
                proc.refresh();
                if proc.pid.is_some() {
                    return Err("stop the command before removing it".to_string());
                }
            }
        }
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
        // Auto-remove the project once its last command is gone (there is no
        // manual project delete; an empty project cleans itself up).
        if project.commands.is_empty() {
            projects.retain(|p| p.id != project_id);
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

/// True if two folder paths refer to the same location. Canonicalize both and
/// compare the resulting `PathBuf`s (handles drive-letter case, `/` vs `\`,
/// trailing separators, and `.`/`..` on Windows). If canonicalize fails for
/// either path (e.g. it no longer exists), fall back to a normalized string
/// compare: lowercase + strip trailing `\` and `/`.
fn same_path(a: &str, b: &str) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => norm_path(a) == norm_path(b),
    }
}

fn norm_path(p: &str) -> String {
    p.trim_end_matches(['\\', '/']).to_lowercase()
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

/// Short display name for a command, used whenever a name is omitted (the UI's
/// add flow and the `POST /run` API). Never returns the whole command line - a
/// long launch (a Flutter run with a wall of `--dart-define`s) collapses to a
/// stable short label instead of becoming an unreadable title.
fn derive_name(cmd: &str) -> String {
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    // Every Flutter launch (`flutter run`, `fvm flutter run --machine ...`)
    // contains "flutter"; collapse the dart-define soup to one short label.
    if toks.iter().any(|t| *t == "flutter") {
        return "flutter run".to_string();
    }
    match toks.as_slice() {
        [runner, "run", x, ..] if matches!(*runner, "npm" | "pnpm" | "yarn" | "bun") => {
            x.to_string()
        }
        [runner, x, ..] if matches!(*runner, "yarn" | "pnpm" | "bun" | "npx") => x.to_string(),
        ["cargo", x, ..] => format!("cargo {x}"),
        // Fallback: the program name only, never the full command line.
        [prog, ..] => prog.to_string(),
        [] => cmd.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::derive_name;

    #[test]
    fn derive_name_handles_runners_and_fallback() {
        assert_eq!(derive_name("npm run dev"), "dev");
        assert_eq!(derive_name("pnpm run build"), "build");
        assert_eq!(derive_name("yarn run start"), "start");
        assert_eq!(derive_name("yarn dev"), "dev");
        assert_eq!(derive_name("npx vite"), "vite");
        assert_eq!(derive_name("cargo run"), "cargo run");
        assert_eq!(derive_name("cargo tauri dev"), "cargo tauri");
        // Long launches collapse to a short label instead of the whole line.
        assert_eq!(derive_name("node server.js"), "node");
        assert_eq!(derive_name("  flutter run  "), "flutter run");
        assert_eq!(
            derive_name("fvm flutter run -d web-server --web-port 5000 --dart-define=ENV=local"),
            "flutter run"
        );
    }
}
