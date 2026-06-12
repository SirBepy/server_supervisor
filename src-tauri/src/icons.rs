use base64::Engine;
use serde::Serialize;
use std::path::{Path, PathBuf};
use ts_rs::TS;

// Candidate icon locations under a project root, in priority order. First file
// that exists wins. Covers generic roots, web/public, Flutter, and Tauri layouts.
const CANDIDATES: &[&str] = &[
    "icon.svg",
    "icon.png",
    "icon.ico",
    "logo.svg",
    "logo.png",
    "app-icon.png",
    "favicon.svg",
    "favicon.ico",
    "favicon.png",
    "public/favicon.svg",
    "public/favicon.ico",
    "public/favicon.png",
    "public/logo.png",
    "static/favicon.svg",
    "static/favicon.ico",
    "static/favicon.png",
    "web/icons/Icon-192.png",
    "web/favicon.png",
    "src-tauri/icons/128x128.png",
    "src-tauri/icons/icon.png",
];

const MAX_ICON_BYTES: u64 = 512 * 1024; // skip absurdly large files

/// First existing icon file under `root`, or None.
pub fn find_icon_file(root: &Path) -> Option<PathBuf> {
    CANDIDATES
        .iter()
        .map(|rel| root.join(rel))
        .find(|p| p.is_file())
}

/// MIME type for a supported image extension, or None if unsupported.
pub fn mime_for(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "svg" => Some("image/svg+xml"),
        "png" => Some("image/png"),
        "ico" => Some("image/x-icon"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

// Exported to TS via tests/export_types.rs (this repo composes all TS types
// there rather than using per-type #[ts(export)] - see that file's header).
#[derive(Serialize, TS)]
pub struct ProjectIcon {
    pub mime: String,
    /// base64-encoded file bytes (no data: prefix; the frontend builds the URI).
    pub data: String,
}

/// Scan a project root for a real icon and return it as base64. None if no
/// supported icon is found (frontend then falls back to the tech logo).
#[tauri::command]
pub fn get_project_icon(root: String) -> Option<ProjectIcon> {
    let path = find_icon_file(Path::new(&root))?;
    let mime = mime_for(&path)?;
    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() > MAX_ICON_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    Some(ProjectIcon {
        mime: mime.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_first_candidate_by_priority() {
        let dir = std::env::temp_dir().join(format!("ss_icons_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("web/icons")).unwrap();
        // Two candidates present; favicon.png is lower priority than icon.png.
        fs::write(dir.join("web/icons/Icon-192.png"), b"x").unwrap();
        fs::write(dir.join("icon.png"), b"x").unwrap();
        let found = find_icon_file(&dir).unwrap();
        assert!(found.ends_with("icon.png"), "got {found:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn none_when_no_icon() {
        let dir = std::env::temp_dir().join(format!("ss_icons_none_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(find_icon_file(&dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mime_known_and_unknown() {
        assert_eq!(mime_for(Path::new("a/icon.svg")), Some("image/svg+xml"));
        assert_eq!(mime_for(Path::new("a/icon.PNG")), Some("image/png"));
        assert_eq!(mime_for(Path::new("a/icon.txt")), None);
    }
}
