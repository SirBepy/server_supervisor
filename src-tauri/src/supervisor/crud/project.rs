//! Project CRUD: list/add/rename/remove a project. Command CRUD (add/edit/
//! remove a command) lives in the sibling `command` module; both are reached
//! by the parent `crud` module's `ensure_and_run`, which composes them.

use crate::supervisor::config;
use crate::supervisor::registry::{begin_stop_locked, Supervisor};
use crate::types::{unit_id, Project};

impl Supervisor {
    pub fn list_projects(&self) -> Vec<Project> {
        self.projects.lock().unwrap().clone()
    }

    pub fn add_project(&self, name: String, root: String) -> Result<Project, String> {
        self.add_project_inner(name, root, false, None)
    }

    /// Shared by `add_project` (always non-transient: the UI's manual add-project
    /// flow) and `ensure_and_run` (which detects transience first). Transience is
    /// set once here, at registration, and never re-derived afterward.
    pub(super) fn add_project_inner(
        &self,
        name: String,
        root: String,
        transient: bool,
        transient_label: Option<String>,
    ) -> Result<Project, String> {
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
        let id = super::unique_id(&name, &|cand| projects.iter().any(|p| p.id == cand));
        let project = Project {
            id,
            name,
            root,
            commands: Vec::new(),
            presets: Vec::new(),
            active_preset: None,
            transient,
            transient_label,
        };
        projects.push(project.clone());
        config::save(&self.data_dir, &projects);
        Ok(project)
    }

    /// Rename a project's display name. The `id` is the stable handle (keys the
    /// runtime procs map, logs, and API paths), so it never changes here - only
    /// the mutable `name`. Returns the updated project.
    pub fn rename_project(&self, project_id: &str, new_name: String) -> Result<Project, String> {
        let new_name = new_name.trim().to_string();
        if new_name.is_empty() {
            return Err("project name is required".to_string());
        }
        let mut projects = self.projects.lock().unwrap();
        let project = projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .ok_or_else(|| format!("unknown project: {project_id}"))?;
        project.name = new_name;
        let updated = project.clone();
        config::save(&self.data_dir, &projects);
        Ok(updated)
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

        // Two-phase like `Supervisor::stop`: run the kills after dropping the
        // lock. `release_project` below reclaims ports regardless.
        let handles: Vec<crate::supervisor::proc::StopHandle> = {
            let mut map = self.procs.lock().unwrap();
            removed
                .commands
                .iter()
                .filter_map(|c| map.remove(&unit_id(&removed.id, &c.id)))
                .map(|mut proc| begin_stop_locked(&mut proc).2)
                .collect()
        };
        for handle in handles {
            handle.finish();
        }
        // Stop the project's reverse-proxy hub listener (if any) before
        // reclaiming its port - see `hub_lifecycle::Supervisor::stop_hub`.
        self.stop_hub(&removed.id);
        // Reclaim the whole port block (and any per-command overrides) so a
        // future project can reuse it instead of it staying reserved forever.
        self.ports.release_project(&removed.id);
        Ok(())
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
