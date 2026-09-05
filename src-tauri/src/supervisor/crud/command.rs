//! Command CRUD: add/edit/remove a command within a project. Project CRUD
//! (list/add/rename/remove a project) lives in the sibling `project` module.

use crate::supervisor::config;
use crate::supervisor::proc::ManagedProc;
use crate::supervisor::registry::Supervisor;
use crate::types::{unit_id, Command, ProcKind, ProcSpec, Role};

impl Supervisor {
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
        fixed_port: Option<u16>,
        env: String,
        role: Option<Role>,
    ) -> Result<Command, String> {
        let name = name.trim().to_string();
        let cmd = normalize_cmd(&cmd);
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
        let cid = super::unique_id(&name, &|cand| project.commands.iter().any(|c| c.id == cand));
        // Claim this command's stable port slot eagerly, in creation order, so
        // "base+0, base+1, ..." reflects declared order rather than start
        // order, and a bad manual override is rejected right here instead of
        // silently at spawn time.
        if use_dynamic_port {
            self.ports.project_port(project_id, &unit_id(project_id, &cid), fixed_port)?;
        }
        let command = Command {
            id: cid,
            name,
            cmd,
            kind,
            autostart,
            use_dynamic_port,
            fixed_port,
            env,
            role,
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
        fixed_port: Option<u16>,
        env: String,
        role: Option<Role>,
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
            // Re-resolve (or drop) this command's port reservation now that we
            // know it genuinely exists, before mutating/saving anything, so a
            // bad manual override is rejected cleanly rather than half-applied.
            let owner = unit_id(project_id, command_id);
            if use_dynamic_port {
                self.ports.project_port(project_id, &owner, fixed_port)?;
            } else {
                self.ports.release_owner(&owner);
            }
            command.name = name;
            command.cmd = cmd;
            command.kind = kind;
            command.autostart = autostart;
            command.use_dynamic_port = use_dynamic_port;
            command.fixed_port = fixed_port;
            command.env = env;
            command.role = role;
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
                        || proc.spec.fixed_port != new_spec.fixed_port
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
        let project_emptied = project.commands.is_empty();
        if project_emptied {
            projects.retain(|p| p.id != project_id);
        }
        config::save(&self.data_dir, &projects);
        drop(projects);

        let mut map = self.procs.lock().unwrap();
        if let Some(mut proc) = map.remove(&unit_id(project_id, command_id)) {
            // Already guaranteed stopped (checked above), so this is a no-op
            // cleanup - fine to finish() inline without releasing the lock.
            proc.begin_stop().finish();
        }
        drop(map);
        // Reclaim this command's port slot - or, if the project emptied out
        // and auto-removed itself, the whole block - rather than leaving it
        // reserved forever.
        if project_emptied {
            self.ports.release_project(project_id);
        } else {
            self.ports.release_owner(&unit_id(project_id, command_id));
        }
        Ok(())
    }
}

/// Collapse a command string to its canonical dedup form: trim ends and reduce
/// every run of internal whitespace to a single space. Keeps case (flags are
/// case-sensitive). `flutter  run` and ` flutter run ` both become `flutter run`,
/// so trivial whitespace variants reuse one command entry instead of forking.
fn normalize_cmd(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::normalize_cmd;

    #[test]
    fn normalize_cmd_collapses_and_trims_whitespace() {
        assert_eq!(normalize_cmd("flutter  run"), "flutter run");
        assert_eq!(normalize_cmd("  flutter run  "), "flutter run");
        assert_eq!(normalize_cmd("npm\trun   dev"), "npm run dev");
        assert_eq!(normalize_cmd("flutter run"), "flutter run");
    }
}
