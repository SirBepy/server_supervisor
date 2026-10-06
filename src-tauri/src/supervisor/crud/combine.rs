//! "Combine into one command" migration: merges 2+ existing commands the dev
//! picked by hand into a single parameterized `Command`, one `ParamValue` per
//! source. There is no clustering/similarity code here - the caller always
//! names the exact source ids.

use super::params;
use crate::ports::PortEntry;
use crate::supervisor::config;
use crate::supervisor::param_sub::normalize_cmd;
use crate::supervisor::proc::ManagedProc;
use crate::supervisor::registry::Supervisor;
use crate::types::{unit_id, Command, CommandParam, ProcSpec};

impl Supervisor {
    /// Builds one new templated `Command` from `source_ids` (2+, all in
    /// `project_id`) and removes the sources, mirroring the port-reservation
    /// and runtime-map bookkeeping `add_command`/`remove_command` each do on
    /// their own side of this operation. Every non-`cmd` field is copied from
    /// the FIRST source (`source_ids[0]`); nothing is written if any
    /// validation step below fails.
    pub fn combine_commands(
        &self,
        project_id: &str,
        source_ids: Vec<String>,
        name: String,
        template: String,
        mut param: CommandParam,
    ) -> Result<Command, String> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err("command name is required".to_string());
        }
        let template = normalize_cmd(&template);
        if template.is_empty() {
            return Err("template is required".to_string());
        }
        params::validate_params(std::slice::from_ref(&param))?;
        let token = format!("{{{}}}", param.name.to_uppercase());
        if !template.contains(&token) {
            return Err(format!("template must contain the {token} placeholder"));
        }
        if source_ids.len() < 2 {
            return Err("pick at least 2 commands to combine".to_string());
        }
        let mut seen = std::collections::HashSet::new();
        for id in &source_ids {
            if !seen.insert(id.as_str()) {
                return Err(format!("duplicate source command \"{id}\""));
            }
        }
        if param.values.len() != source_ids.len() {
            return Err(format!(
                "param must have exactly one value per source command ({} values for {} commands)",
                param.values.len(),
                source_ids.len()
            ));
        }

        let mut projects = self.projects.lock().unwrap();
        let project = projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .ok_or_else(|| format!("unknown project: {project_id}"))?;

        // Resolve every source (in the caller's order) before touching
        // anything else, so an unknown id - including one from another
        // project, which simply never matches here - fails cleanly.
        let mut sources: Vec<Command> = Vec::with_capacity(source_ids.len());
        for id in &source_ids {
            let c = project
                .commands
                .iter()
                .find(|c| &c.id == id)
                .ok_or_else(|| format!("unknown command: {id}"))?;
            sources.push(c.clone());
        }

        // Same liveness check `remove_command` uses (refresh, then pid):
        // a source mid-restart must block the combine exactly like it
        // blocks a plain removal.
        {
            let mut map = self.procs.lock().unwrap();
            let mut running = Vec::new();
            for s in &sources {
                if let Some(proc) = map.get_mut(&unit_id(project_id, &s.id)) {
                    proc.refresh();
                    if proc.pid.is_some() {
                        running.push(s.name.clone());
                    }
                }
            }
            if !running.is_empty() {
                return Err(format!(
                    "stop these commands before combining: {}",
                    running.join(", ")
                ));
            }
        }

        let first = sources[0].clone();
        param.last_value = Some(param.values[0].value.clone());
        // Pinned to the first source rather than re-inferred from `template`:
        // the merged template's shared prefix/suffix could drop the one
        // token (e.g. the literal word "flutter") `ProcKind::infer`'s
        // substring check depends on, silently downgrading a flutter variant
        // pair to Generic.
        let kind = first.kind.clone();

        let owner_of = |id: &str| unit_id(project_id, id);
        let new_id = super::unique_id(&name, &|cand| project.commands.iter().any(|c| c.id == cand));
        let new_owner = owner_of(&new_id);

        // Release every source's port slot before claiming the new owner's,
        // since a fixed-port source would otherwise collide with itself
        // under a different owner string. Each released entry is recorded
        // first so a failed claim can restore the exact prior state.
        let prior_ports: Vec<PortEntry> = sources
            .iter()
            .filter_map(|s| self.ports.list().into_iter().find(|e| e.owner == owner_of(&s.id)))
            .collect();
        for s in &sources {
            self.ports.release_owner(&owner_of(&s.id));
        }
        if first.use_dynamic_port {
            if let Err(e) = self.ports.project_port(project_id, &new_owner, first.fixed_port) {
                for p in &prior_ports {
                    self.ports.reserve(&p.owner, p.port, &p.note);
                }
                return Err(e);
            }
        }

        let new_command = Command {
            id: new_id,
            name,
            cmd: template,
            kind,
            autostart: first.autostart,
            use_dynamic_port: first.use_dynamic_port,
            fixed_port: first.fixed_port,
            env: first.env.clone(),
            role: first.role.clone(),
            dock_window: first.dock_window,
            play_sound: first.play_sound,
            dock_headless: first.dock_headless,
            params: vec![param],
        };

        // Insert at the first source's original position, adjusted for any
        // OTHER selected source that sat earlier in the list (removing those
        // shifts everything after them left by one).
        let first_index = project.commands.iter().position(|c| c.id == first.id).unwrap();
        let removed_before_first = project.commands[..first_index]
            .iter()
            .filter(|c| source_ids.contains(&c.id))
            .count();
        project.commands.retain(|c| !source_ids.contains(&c.id));
        let insert_at = (first_index - removed_before_first).min(project.commands.len());
        project.commands.insert(insert_at, new_command.clone());

        let project_snapshot = project.clone();
        config::save(&self.data_dir, &projects);
        drop(projects);

        let mut map = self.procs.lock().unwrap();
        for s in &sources {
            if let Some(mut proc) = map.remove(&owner_of(&s.id)) {
                // Already guaranteed stopped above - fine to finish() inline.
                proc.begin_stop().finish();
            }
        }
        let spec = ProcSpec::from_unit(&project_snapshot, &new_command);
        map.entry(spec.id.clone()).or_insert_with(|| ManagedProc::new(spec));

        Ok(new_command)
    }
}
