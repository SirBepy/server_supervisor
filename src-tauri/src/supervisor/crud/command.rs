//! Command CRUD: add/edit/remove a command within a project. Project CRUD
//! (list/add/rename/remove a project) lives in the sibling `project` module.

use super::params::{self, ParamMismatch};
use crate::supervisor::config;
use crate::supervisor::param_sub::normalize_cmd;
use crate::supervisor::proc::ManagedProc;
use crate::supervisor::registry::Supervisor;
use crate::types::{unit_id, Command, CommandParam, ProcKind, ProcSpec, Role};

/// `param_mismatch` is set when the posted line matched a RUNNING command on a
/// different variant: `command` is then returned untouched, since an implicit
/// match must never restart or relabel a live process.
pub struct AddCommandOutcome {
    pub command: Command,
    pub param_mismatch: Option<ParamMismatch>,
}

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
        dock_window: bool,
        command_params: Vec<CommandParam>,
    ) -> Result<AddCommandOutcome, String> {
        let name = name.trim().to_string();
        let cmd = normalize_cmd(&cmd);
        if name.is_empty() || cmd.is_empty() {
            return Err("command name and cmd are required".to_string());
        }
        if !command_params.is_empty() {
            params::validate_params(&command_params)?;
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
            return Ok(AddCommandOutcome { command: existing.clone(), param_mismatch: None });
        }

        // A concrete line posted against a templated command (`sv.ps1`, or any
        // caller that renders the variant itself) resolves to that command
        // instead of forking a near-duplicate.
        if let Some((matched, combo)) = params::find_by_params_match(project, &cmd) {
            let matched = matched.clone();
            drop(projects);
            return self.adopt_params_match(project_id, matched, combo);
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
            dock_window,
            play_sound: false,
            dock_headless: false,
            params: command_params,
        };
        project.commands.push(command.clone());
        let project_snapshot = project.clone();
        config::save(&self.data_dir, &projects);
        drop(projects);

        let mut map = self.procs.lock().unwrap();
        let spec = ProcSpec::from_unit(&project_snapshot, &command);
        map.entry(spec.id.clone())
            .or_insert_with(|| ManagedProc::new(spec));
        Ok(AddCommandOutcome { command, param_mismatch: None })
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
        dock_window: bool,
        // `None` keeps the existing params, so a caller that predates them
        // cannot wipe them by omission; `Some(vec![])` clears.
        command_params: Option<Vec<CommandParam>>,
    ) -> Result<Command, String> {
        let name = name.trim().to_string();
        let cmd = cmd.trim().to_string();
        if name.is_empty() || cmd.is_empty() {
            return Err("command name and cmd are required".to_string());
        }
        if let Some(ref new_params) = command_params {
            if !new_params.is_empty() {
                params::validate_params(new_params)?;
            }
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
            command.dock_window = dock_window;
            if let Some(new_params) = command_params {
                command.params = new_params;
            }
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
                        || proc.spec.env != new_spec.env
                        || proc.spec.params != new_spec.params;
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

    /// Lets `project_id:command_id`'s audio through (`on`) or mutes it.
    /// Unlike `update_command` this works on a running command and never
    /// restarts it: the audio watcher reads `spec.play_sound` on its next
    /// tick and flips the live sessions itself.
    pub fn set_command_sound(&self, project_id: &str, command_id: &str, on: bool) -> Result<Command, String> {
        self.set_live_flag(project_id, command_id, |c| c.play_sound = on, |s| s.play_sound = on)
    }

    /// Moves `project_id:command_id`'s window into (`on`) or out of the
    /// headless host. Like `set_command_sound`, live and restart-free: the
    /// headless tick (`Supervisor::headless_tick`) does the actual docking.
    pub fn set_command_headless(&self, project_id: &str, command_id: &str, on: bool) -> Result<Command, String> {
        self.set_live_flag(project_id, command_id, |c| c.dock_headless = on, |s| s.dock_headless = on)
    }

    /// Persists a flag on the command and mirrors it onto the live proc's
    /// spec, so a running process picks it up without a restart.
    fn set_live_flag(
        &self,
        project_id: &str,
        command_id: &str,
        on_command: impl FnOnce(&mut Command),
        on_spec: impl FnOnce(&mut ProcSpec),
    ) -> Result<Command, String> {
        let updated = {
            let mut projects = self.projects.lock().unwrap();
            let command = projects
                .iter_mut()
                .find(|p| p.id == project_id)
                .ok_or_else(|| format!("unknown project: {project_id}"))?
                .commands
                .iter_mut()
                .find(|c| c.id == command_id)
                .ok_or_else(|| format!("unknown command: {command_id}"))?;
            on_command(command);
            let updated = command.clone();
            config::save(&self.data_dir, &projects);
            updated
        };
        if let Some(proc) = self.procs.lock().unwrap().get_mut(&unit_id(project_id, command_id)) {
            on_spec(&mut proc.spec);
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
