//! Loads the declared-process registry from `<data-dir>/procs.json`.
//! The user hand-edits this file to declare the dev servers across projects.

use crate::types::ProcSpec;
use std::path::Path;

const CONFIG_FILE: &str = "procs.json";

/// Load declared process specs. If the file is missing, write an empty default
/// so the user has something to edit, and return an empty list.
pub fn load(data_dir: &Path) -> Vec<ProcSpec> {
    let path = data_dir.join(CONFIG_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Vec<ProcSpec>>(&text) {
            Ok(specs) => specs,
            Err(e) => {
                log::error!("supervisor: failed to parse {CONFIG_FILE}: {e}");
                Vec::new()
            }
        },
        Err(_) => {
            let _ = std::fs::write(&path, "[]\n");
            Vec::new()
        }
    }
}
