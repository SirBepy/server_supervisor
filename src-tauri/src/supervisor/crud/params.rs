//! Command params: matching a concrete posted line back to its templated
//! command, validating authored params and `/run` selections, and switching
//! a command's variant at runtime.

use crate::supervisor::config;
use crate::supervisor::param_sub::{normalize_cmd, render_with, resolve_value};
use crate::supervisor::proc::ManagedProc;
use crate::supervisor::registry::Supervisor;
use crate::types::{unit_id, Command, CommandParam, ProcSpec, Project};
use super::command::AddCommandOutcome;
use serde::Serialize;
use std::collections::HashMap;

/// Renders tried per templated command when matching a posted line. Real
/// commands have 1-2 params of 2-3 values (2026-07-30 audit of 55 commands),
/// so this only bounds a pathological hand-authored set.
const MAX_RENDER_COMBOS: usize = 64;

/// What is live vs. what a posted line asked for, keyed by param name.
#[derive(Debug, Clone, Serialize)]
pub struct ParamMismatch {
    pub running: HashMap<String, String>,
    pub requested: HashMap<String, String>,
}

/// The command `add_command` would resolve `posted_cmd` to, without creating
/// one, so `/run` can reject a bad `params` key before anything is forked.
pub(super) fn find_existing<'a>(project: &'a Project, posted_cmd: &str) -> Option<&'a Command> {
    let normalized = normalize_cmd(posted_cmd);
    project
        .commands
        .iter()
        .find(|c| c.cmd == normalized)
        .or_else(|| find_by_params_match(project, posted_cmd).map(|(c, _)| c))
}

/// First templated command, and combination of its value ids, whose render
/// equals `posted_cmd` after normalizing. A pure string match with no
/// similarity heuristic, so genuinely separate commands (`dev` vs `preview`)
/// never merge.
pub(super) fn find_by_params_match<'a>(
    project: &'a Project,
    posted_cmd: &str,
) -> Option<(&'a Command, Vec<(String, String)>)> {
    let target = normalize_cmd(posted_cmd);
    for c in project.commands.iter().filter(|c| !c.params.is_empty()) {
        for combo in combos(&c.params) {
            if normalize_cmd(&render_with(&c.cmd, &c.params, &combo)) == target {
                return Some((c, combo));
            }
        }
    }
    None
}

/// Up to `MAX_RENDER_COMBOS` combinations, decoded from an index so the full
/// cartesian product is never materialized. A param with no values yields
/// none: there is nothing to pick for it.
fn combos(params: &[CommandParam]) -> Vec<Vec<(String, String)>> {
    if params.is_empty() || params.iter().any(|p| p.values.is_empty()) {
        return Vec::new();
    }
    let sizes: Vec<usize> = params.iter().map(|p| p.values.len()).collect();
    let total = sizes.iter().fold(1usize, |acc, &s| acc.saturating_mul(s));
    let n = total.min(MAX_RENDER_COMBOS);
    let mut out = Vec::with_capacity(n);
    for idx in 0..n {
        // Mixed-radix decode, last param varying fastest (nested-loop order).
        let mut rest = idx;
        let mut digits = vec![0usize; params.len()];
        for i in (0..params.len()).rev() {
            digits[i] = rest % sizes[i];
            rest /= sizes[i];
        }
        let combo = params
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.clone(), p.values[digits[i]].value.clone()))
            .collect();
        out.push(combo);
    }
    out
}

/// The combination `substitute_params` would spawn, via the same fallback.
pub(super) fn current_combo(command: &Command) -> Vec<(String, String)> {
    command
        .params
        .iter()
        .filter_map(|p| resolve_value(p, None).map(|v| (p.name.clone(), v.value.clone())))
        .collect()
}

/// Rejects a selection naming an unknown param or value id, listing the valid
/// ones, so a typo never silently runs the default variant.
pub(super) fn validate_selection(params: &[CommandParam], requested: &HashMap<String, String>) -> Result<(), String> {
    for (name, value_id) in requested {
        let Some(p) = params.iter().find(|p| p.name.eq_ignore_ascii_case(name)) else {
            let valid = names_list(params);
            return Err(format!("unknown param \"{name}\"; valid params: {valid}"));
        };
        if !p.values.iter().any(|v| &v.value == value_id) {
            let valid = value_ids_list(p);
            return Err(format!(
                "unknown value \"{value_id}\" for param \"{name}\"; valid values: {valid}"
            ));
        }
    }
    Ok(())
}

fn names_list(params: &[CommandParam]) -> String {
    if params.is_empty() {
        "(none)".to_string()
    } else {
        params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
    }
}

fn value_ids_list(p: &CommandParam) -> String {
    if p.values.is_empty() {
        "(none)".to_string()
    } else {
        p.values.iter().map(|v| v.value.as_str()).collect::<Vec<_>>().join(", ")
    }
}

/// Names become `{NAME}` tokens, hence the charset and the reserved `port`.
pub(super) fn validate_params(params: &[CommandParam]) -> Result<(), String> {
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for p in params {
        let name = p.name.trim();
        if name.is_empty() {
            return Err("a param name cannot be empty".to_string());
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err(format!(
                "param name \"{}\" must contain only letters, digits, \"_\", or \"-\"",
                p.name
            ));
        }
        if name.eq_ignore_ascii_case("port") {
            return Err("param name \"port\" is reserved (would shadow {PORT})".to_string());
        }
        if !seen_names.insert(name.to_ascii_uppercase()) {
            return Err(format!("duplicate param name \"{}\"", p.name));
        }
        let mut seen_values: std::collections::HashSet<String> = std::collections::HashSet::new();
        for v in &p.values {
            if v.value.is_empty() {
                return Err(format!("param \"{}\" has an empty value id", p.name));
            }
            if !seen_values.insert(v.value.clone()) {
                return Err(format!("param \"{}\" has a duplicate value id \"{}\"", p.name, v.value));
            }
        }
    }
    Ok(())
}

impl Supervisor {
    /// A running command on a different variant is left alone and reported:
    /// an implicit match (no `params` object) must never restart a live
    /// process. A stopped one adopts the posted variant.
    pub(super) fn adopt_params_match(
        &self,
        project_id: &str,
        matched: Command,
        combo: Vec<(String, String)>,
    ) -> Result<AddCommandOutcome, String> {
        let running = match self.procs.lock().unwrap().get_mut(&unit_id(project_id, &matched.id)) {
            Some(proc) => {
                proc.refresh();
                proc.pid.is_some()
            }
            None => false,
        };
        if running {
            let current = current_combo(&matched);
            let param_mismatch = (current != combo).then(|| ParamMismatch {
                running: current.into_iter().collect(),
                requested: combo.into_iter().collect(),
            });
            return Ok(AddCommandOutcome { command: matched, param_mismatch });
        }
        let command = self.set_command_params(project_id, &matched.id, &combo.into_iter().collect())?;
        Ok(AddCommandOutcome { command, param_mismatch: None })
    }

    pub fn set_command_param(
        &self,
        project_id: &str,
        command_id: &str,
        name: &str,
        value_id: &str,
    ) -> Result<Command, String> {
        let mut requested = HashMap::new();
        requested.insert(name.to_string(), value_id.to_string());
        self.set_command_params(project_id, command_id, &requested)
    }

    /// Unlike the sound/headless toggles, a param changes the spawned command
    /// line, so a running command restarts into the new variant. The live spec
    /// is refreshed either way because `start` reads only the cached spec. A
    /// re-pick of the current value never bounces a live process.
    pub fn set_command_params(
        &self,
        project_id: &str,
        command_id: &str,
        requested: &HashMap<String, String>,
    ) -> Result<Command, String> {
        let (updated, project_snapshot, changed) = {
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
            validate_selection(&command.params, requested)?;
            let mut changed = false;
            for (pname, vid) in requested {
                if let Some(p) = command.params.iter_mut().find(|p| p.name.eq_ignore_ascii_case(pname)) {
                    if p.last_value.as_deref() != Some(vid.as_str()) {
                        changed = true;
                    }
                    p.last_value = Some(vid.clone());
                }
            }
            let updated = command.clone();
            let snapshot = project.clone();
            config::save(&self.data_dir, &projects);
            (updated, snapshot, changed)
        };

        let new_spec = ProcSpec::from_unit(&project_snapshot, &updated);
        let id = new_spec.id.clone();
        let running = {
            let mut map = self.procs.lock().unwrap();
            match map.get_mut(&id) {
                Some(proc) => {
                    proc.refresh();
                    proc.spec = new_spec;
                    proc.pid.is_some()
                }
                None => {
                    map.insert(id.clone(), ManagedProc::new(new_spec));
                    false
                }
            }
        };
        if changed && running {
            self.restart(&id)?;
        }
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ParamValue;

    fn param(name: &str, values: Vec<(&str, &str)>, last_value: Option<&str>) -> CommandParam {
        CommandParam {
            name: name.to_string(),
            label: name.to_string(),
            values: values
                .into_iter()
                .map(|(id, flag)| ParamValue {
                    value: id.to_string(),
                    label: id.to_string(),
                    flag: flag.to_string(),
                })
                .collect(),
            last_value: last_value.map(|s| s.to_string()),
        }
    }

    #[test]
    fn validate_selection_rejects_unknown_name_and_value() {
        let params = vec![param("device", vec![("chrome", "-d chrome")], None)];
        let mut req = HashMap::new();
        req.insert("flavor".to_string(), "dev".to_string());
        let err = validate_selection(&params, &req).unwrap_err();
        assert!(err.contains("flavor"), "{err}");
        assert!(err.contains("device"), "error must list the valid param name: {err}");

        let mut req2 = HashMap::new();
        req2.insert("device".to_string(), "firefox".to_string());
        let err2 = validate_selection(&params, &req2).unwrap_err();
        assert!(err2.contains("firefox"), "{err2}");
        assert!(err2.contains("chrome"), "error must list the valid value id: {err2}");

        let mut ok = HashMap::new();
        ok.insert("device".to_string(), "chrome".to_string());
        assert!(validate_selection(&params, &ok).is_ok());
    }

    #[test]
    fn validate_params_rejects_each_bad_authoring_shape() {
        let dup_name = vec![param("device", vec![("a", "")], None), param("DEVICE", vec![("b", "")], None)];
        assert!(validate_params(&dup_name).unwrap_err().contains("duplicate param name"));

        let empty_name = vec![CommandParam { name: "".to_string(), label: "".to_string(), values: vec![], last_value: None }];
        assert!(validate_params(&empty_name).unwrap_err().contains("empty"));

        let bad_chars = vec![param("de vice", vec![("a", "")], None)];
        assert!(validate_params(&bad_chars).is_err());

        let reserved = vec![param("port", vec![("a", "")], None)];
        assert!(validate_params(&reserved).unwrap_err().contains("reserved"));

        let dup_value = vec![param("device", vec![("chrome", "-d chrome"), ("chrome", "-d chrome2")], None)];
        assert!(validate_params(&dup_value).unwrap_err().contains("duplicate value id"));

        let empty_value = vec![param("device", vec![("", "-d chrome")], None)];
        assert!(validate_params(&empty_value).unwrap_err().contains("empty value id"));

        let ok = vec![param("device", vec![("chrome", "-d chrome")], None)];
        assert!(validate_params(&ok).is_ok());
    }
}
