use serde::{Deserialize, Serialize};
use std::path::Path;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub project_ids: Vec<String>,
}

const GROUPS_FILE: &str = "groups.json";

fn groups_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(GROUPS_FILE)
}

pub fn load(data_dir: &Path) -> Vec<Group> {
    std::fs::read_to_string(groups_path(data_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(data_dir: &Path, groups: &[Group]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(groups).map_err(|e| e.to_string())?;
    crate::fsutil::write_atomic(&groups_path(data_dir), text.as_bytes())
        .map_err(|e| e.to_string())
}

pub fn create(data_dir: &Path, name: String) -> Result<Group, String> {
    let mut groups = load(data_dir);
    if groups
        .iter()
        .any(|g| g.name.to_lowercase() == name.to_lowercase())
    {
        return Err(format!("group '{}' already exists", name));
    }
    let group = Group {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        project_ids: vec![],
    };
    groups.push(group.clone());
    save(data_dir, &groups)?;
    Ok(group)
}

pub fn update(data_dir: &Path, id: &str, name: String) -> Result<Group, String> {
    let mut groups = load(data_dir);
    if groups
        .iter()
        .any(|g| g.id != id && g.name.to_lowercase() == name.to_lowercase())
    {
        return Err(format!("group '{}' already exists", name));
    }
    let g = groups
        .iter_mut()
        .find(|g| g.id == id)
        .ok_or_else(|| format!("group '{}' not found", id))?;
    g.name = name;
    let updated = g.clone();
    save(data_dir, &groups)?;
    Ok(updated)
}

pub fn delete(data_dir: &Path, id: &str) -> Result<(), String> {
    let mut groups = load(data_dir);
    let before = groups.len();
    groups.retain(|g| g.id != id);
    if groups.len() == before {
        return Err(format!("group '{}' not found", id));
    }
    save(data_dir, &groups)
}

pub fn set_project_group(
    data_dir: &Path,
    project_id: &str,
    group_id: Option<&str>,
) -> Result<(), String> {
    let mut groups = load(data_dir);
    for g in &mut groups {
        g.project_ids.retain(|pid| pid != project_id);
    }
    if let Some(gid) = group_id {
        let g = groups
            .iter_mut()
            .find(|g| g.id == gid)
            .ok_or_else(|| format!("group '{}' not found", gid))?;
        g.project_ids.push(project_id.to_string());
    }
    save(data_dir, &groups)
}
