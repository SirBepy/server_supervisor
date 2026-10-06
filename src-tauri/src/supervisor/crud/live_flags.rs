//! Per-command flags that apply to a running process without a restart:
//! the audio watcher and headless tick re-read the live spec on their own.

use crate::supervisor::config;
use crate::supervisor::registry::Supervisor;
use crate::types::{unit_id, Command, ProcSpec};

impl Supervisor {
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
}
