//! Loads/saves the project registry (`<data-dir>/projects.json`). The user
//! edits projects + commands through the UI; this is the persisted source of truth.

use crate::types::{Command, Project, ProcSpec};
use std::path::Path;

const CONFIG_FILE: &str = "projects.json";
const LEGACY_FILE: &str = "procs.json";

/// Slugify a name into a stable id fragment.
pub fn slug(name: &str) -> String {
    let s: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let collapsed = s
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if collapsed.is_empty() {
        "item".to_string()
    } else {
        collapsed
    }
}

/// Load the project list. Falls back to migrating a legacy flat `procs.json`,
/// otherwise writes an empty `projects.json`.
pub fn load(data_dir: &Path) -> Vec<Project> {
    let path = data_dir.join(CONFIG_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        match serde_json::from_str::<Vec<Project>>(&text) {
            Ok(projects) => return projects,
            Err(e) => {
                log::error!("supervisor: failed to parse {CONFIG_FILE}: {e}");
                return Vec::new();
            }
        }
    }

    // Migrate a legacy flat procs.json (list of ProcSpec) if present.
    if let Ok(text) = std::fs::read_to_string(data_dir.join(LEGACY_FILE)) {
        if let Ok(specs) = serde_json::from_str::<Vec<ProcSpec>>(&text) {
            if !specs.is_empty() {
                let migrated = migrate(specs);
                save(data_dir, &migrated);
                log::info!("supervisor: migrated {LEGACY_FILE} -> {CONFIG_FILE}");
                return migrated;
            }
        }
    }

    let _ = std::fs::write(&path, "[]\n");
    Vec::new()
}

pub fn save(data_dir: &Path, projects: &[Project]) {
    match serde_json::to_string_pretty(projects) {
        Ok(text) => {
            let _ = std::fs::write(data_dir.join(CONFIG_FILE), text);
        }
        Err(e) => log::error!("supervisor: failed to serialize {CONFIG_FILE}: {e}"),
    }
}

/// Group flat specs by their `project` label into nested projects.
fn migrate(specs: Vec<ProcSpec>) -> Vec<Project> {
    let mut projects: Vec<Project> = Vec::new();
    for spec in specs {
        let pid = slug(&spec.project);
        let cmd = Command {
            id: slug(&spec.name),
            name: spec.name,
            cmd: spec.cmd,
            kind: spec.kind,
            autostart: spec.autostart,
            use_dynamic_port: spec.use_dynamic_port,
            env: spec.env,
        };
        if let Some(p) = projects.iter_mut().find(|p| p.id == pid) {
            p.commands.push(cmd);
        } else {
            projects.push(Project {
                id: pid,
                name: spec.project,
                root: spec.cwd,
                commands: vec![cmd],
            });
        }
    }
    projects
}
