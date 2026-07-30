//! Hub lifecycle and preset CRUD for the Supervisor: starting/stopping a
//! project's reverse-proxy hub listener and managing its upstream presets.
//! A second `impl Supervisor` block, split from `proxy_hub` (which keeps the
//! forwarding/CORS mechanics and the `ProxyHub`/`HubShared` types).

use super::config;
use super::crud;
use super::proxy_hub::{spawn, RequestLogEntry};
use super::registry::Supervisor;
use crate::types::{Project, UpstreamPreset};

/// The project's active preset, resolved from `active_preset` and falling
/// back to the first preset when unset or stale (points at a since-removed
/// preset). `None` only when the project has no presets at all.
fn resolve_active_preset(project: &Project) -> Option<&UpstreamPreset> {
    project
        .active_preset
        .as_deref()
        .and_then(|id| project.presets.iter().find(|p| p.id == id))
        .or_else(|| project.presets.first())
}

// ----- Supervisor integration: lifecycle + preset CRUD + request log -----

impl Supervisor {
    /// Start a hub listener for every project that already has presets
    /// configured. Called once at startup (mirrors `readopt_orphans` /
    /// `start_autostart` - not folded into `Supervisor::new` itself, so
    /// tests that construct a bare `Supervisor` don't unexpectedly bind
    /// listeners).
    pub fn init_hubs(&self) {
        let projects = self.projects.lock().unwrap().clone();
        for project in &projects {
            if project.presets.is_empty() {
                continue;
            }
            if let Err(e) = self.start_hub_for(project) {
                log::error!("proxy_hub: failed to start hub for {}: {e}", project.id);
            }
        }
    }

    /// Spawn the hub listener for `project` on its fixed hub port, using its
    /// resolved active preset. No-op (Ok) if a hub is already running for
    /// this project, or if it has no presets yet.
    fn start_hub_for(&self, project: &Project) -> Result<(), String> {
        {
            let hubs = self.hubs.lock().unwrap();
            if hubs.contains_key(&project.id) {
                return Ok(());
            }
        }
        let Some(active) = resolve_active_preset(project) else {
            return Ok(());
        };
        let port = self.ports.project_hub_port(&project.id)?;
        let hub = spawn(port, active).map_err(|e| e.to_string())?;
        log::info!("proxy_hub: {} listening on http://127.0.0.1:{port}", project.id);
        self.hubs.lock().unwrap().insert(project.id.clone(), hub);
        Ok(())
    }

    /// The fixed port a project's hub listens on (or would listen on once it
    /// has at least one preset) - the stable address a dev app bakes in.
    pub fn hub_port(&self, project_id: &str) -> Result<u16, String> {
        self.ports.project_hub_port(project_id)
    }

    /// Add a named upstream preset to a project. The first preset added for a
    /// project starts its hub listener immediately (activated by default);
    /// subsequent presets are just added to the list until explicitly
    /// activated via `set_active_preset`.
    pub fn add_preset(
        &self,
        project_id: &str,
        name: String,
        base_url: String,
        danger: bool,
    ) -> Result<UpstreamPreset, String> {
        let name = name.trim().to_string();
        let base_url = base_url.trim().to_string();
        if name.is_empty() || base_url.is_empty() {
            return Err("preset name and base_url are required".to_string());
        }
        if reqwest::Url::parse(&base_url).is_err() {
            return Err(format!("'{base_url}' is not a valid URL"));
        }
        let (preset, project_snapshot, was_first) = {
            let mut projects = self.projects.lock().unwrap();
            let project = projects
                .iter_mut()
                .find(|p| p.id == project_id)
                .ok_or_else(|| format!("unknown project: {project_id}"))?;
            let id = crud::unique_id(&name, &|cand| project.presets.iter().any(|p| p.id == cand));
            let was_first = project.presets.is_empty();
            let preset = UpstreamPreset { id: id.clone(), name, base_url, danger };
            project.presets.push(preset.clone());
            if was_first {
                project.active_preset = Some(id);
            }
            let snapshot = project.clone();
            config::save(&self.data_dir, &projects);
            (preset, snapshot, was_first)
        };
        if was_first {
            if let Err(e) = self.start_hub_for(&project_snapshot) {
                log::error!("proxy_hub: failed to start hub for {project_id}: {e}");
            }
        }
        Ok(preset)
    }

    /// Remove a preset. If it was the active one, the next-first remaining
    /// preset becomes active (live-swapped into the running hub, no rebind).
    /// Removing the LAST preset stops and drops the hub entirely.
    pub fn remove_preset(&self, project_id: &str, preset_id: &str) -> Result<(), String> {
        let (project_snapshot, emptied) = {
            let mut projects = self.projects.lock().unwrap();
            let project = projects
                .iter_mut()
                .find(|p| p.id == project_id)
                .ok_or_else(|| format!("unknown project: {project_id}"))?;
            let before = project.presets.len();
            project.presets.retain(|p| p.id != preset_id);
            if project.presets.len() == before {
                return Err(format!("unknown preset: {preset_id}"));
            }
            if project.active_preset.as_deref() == Some(preset_id) {
                project.active_preset = project.presets.first().map(|p| p.id.clone());
            }
            let emptied = project.presets.is_empty();
            let snapshot = project.clone();
            config::save(&self.data_dir, &projects);
            (snapshot, emptied)
        };
        if emptied {
            self.stop_hub(project_id);
        } else if let Some(active) = resolve_active_preset(&project_snapshot) {
            self.swap_active(project_id, active);
        }
        Ok(())
    }

    /// Live-swap a project's hub to a different already-configured preset.
    /// The running listener is NOT rebound - only the shared upstream state
    /// it reads per request changes, so the app's baked-in address holds.
    pub fn set_active_preset(&self, project_id: &str, preset_id: &str) -> Result<(), String> {
        let project_snapshot = {
            let mut projects = self.projects.lock().unwrap();
            let project = projects
                .iter_mut()
                .find(|p| p.id == project_id)
                .ok_or_else(|| format!("unknown project: {project_id}"))?;
            if !project.presets.iter().any(|p| p.id == preset_id) {
                return Err(format!("unknown preset: {preset_id}"));
            }
            project.active_preset = Some(preset_id.to_string());
            let snapshot = project.clone();
            config::save(&self.data_dir, &projects);
            snapshot
        };
        // The project may not have a hub running yet (e.g. this is the very
        // first activation on a project whose presets were seeded by hand-
        // editing projects.json rather than through `add_preset`).
        if let Err(e) = self.start_hub_for(&project_snapshot) {
            log::error!("proxy_hub: failed to start hub for {project_id}: {e}");
        }
        if let Some(active) = resolve_active_preset(&project_snapshot) {
            self.swap_active(project_id, active);
        }
        Ok(())
    }

    fn swap_active(&self, project_id: &str, preset: &UpstreamPreset) {
        let hubs = self.hubs.lock().unwrap();
        if let Some(hub) = hubs.get(project_id) {
            hub.set_active(preset);
        }
    }

    /// Snapshot of a project's request log, oldest first. Empty (not an
    /// error) for a project with no hub running yet.
    pub fn hub_log(&self, project_id: &str) -> Vec<RequestLogEntry> {
        self.hubs
            .lock()
            .unwrap()
            .get(project_id)
            .map(|h| h.log_snapshot())
            .unwrap_or_default()
    }

    /// Stop and drop a project's hub listener, if any - called when the
    /// project is removed, or when its last preset is deleted.
    pub(super) fn stop_hub(&self, project_id: &str) {
        self.hubs.lock().unwrap().remove(project_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_active_preset_falls_back_to_first_on_stale_or_unset_id() {
        let p1 = UpstreamPreset { id: "a".into(), name: "A".into(), base_url: "http://x".into(), danger: false };
        let p2 = UpstreamPreset { id: "b".into(), name: "B".into(), base_url: "http://y".into(), danger: false };
        let mut project = Project {
            id: "p".into(),
            name: "p".into(),
            root: ".".into(),
            commands: vec![],
            presets: vec![p1.clone(), p2.clone()],
            active_preset: None,
        };
        assert_eq!(resolve_active_preset(&project).unwrap().id, "a", "unset falls back to first");

        project.active_preset = Some("gone".to_string());
        assert_eq!(resolve_active_preset(&project).unwrap().id, "a", "stale id falls back to first");

        project.active_preset = Some("b".to_string());
        assert_eq!(resolve_active_preset(&project).unwrap().id, "b", "resolves the real active id");

        project.presets.clear();
        assert!(resolve_active_preset(&project).is_none(), "no presets: nothing to resolve");
    }
}
