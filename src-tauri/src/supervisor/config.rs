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
                // The file exists but is unparseable. Returning empty here would let
                // the next mutation's save() overwrite it with `[]`, making the loss
                // permanent. Back the original up to projects.json.corrupt first
                // (never clobbering an existing backup) so it stays recoverable.
                let corrupt = data_dir.join(format!("{CONFIG_FILE}.corrupt"));
                if corrupt.exists() {
                    log::warn!(
                        "supervisor: {CONFIG_FILE} is corrupt but {CONFIG_FILE}.corrupt already exists; leaving original in place"
                    );
                } else if let Err(re) = std::fs::rename(&path, &corrupt) {
                    log::error!("supervisor: could not back up corrupt {CONFIG_FILE}: {re}");
                } else {
                    log::warn!("supervisor: backed up corrupt {CONFIG_FILE} -> {CONFIG_FILE}.corrupt");
                }
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

    let _ = crate::fsutil::write_atomic(&path, b"[]\n");
    Vec::new()
}

pub fn save(data_dir: &Path, projects: &[Project]) {
    match serde_json::to_string_pretty(projects) {
        Ok(text) => {
            if let Err(e) = crate::fsutil::write_atomic(&data_dir.join(CONFIG_FILE), text.as_bytes())
            {
                log::error!("supervisor: failed to write {CONFIG_FILE}: {e}");
            }
        }
        Err(e) => log::error!("supervisor: failed to serialize {CONFIG_FILE}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn corrupt_config_is_backed_up_not_destroyed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        let bad = b"{ this is not valid json ]";
        fs::write(&path, bad).unwrap();

        // load() must NOT propagate the corruption as data, and must NOT leave the
        // original where the next save() would overwrite it with `[]`.
        let loaded = load(dir.path());
        assert!(loaded.is_empty(), "corrupt config loads as empty");

        let corrupt = dir.path().join(format!("{CONFIG_FILE}.corrupt"));
        assert!(corrupt.exists(), "original preserved as .corrupt");
        assert_eq!(fs::read(&corrupt).unwrap(), bad, "backup keeps the original bytes");
        assert!(!path.exists(), "corrupt original is renamed away, not left to be clobbered");
    }

    #[test]
    fn corrupt_backup_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        let corrupt = dir.path().join(format!("{CONFIG_FILE}.corrupt"));
        fs::write(&corrupt, b"first-corruption").unwrap();
        fs::write(&path, b"second-corruption {]").unwrap();

        let loaded = load(dir.path());
        assert!(loaded.is_empty());
        // The pre-existing backup is the source of truth and must survive.
        assert_eq!(fs::read(&corrupt).unwrap(), b"first-corruption");
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
