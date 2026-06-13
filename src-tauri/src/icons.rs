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

/// Detect a project's primary tech from marker files in its root. Used as a
/// fallback when the command program doesn't reveal it (e.g. a custom launcher
/// script like `start-odysseus`). Returns a key matching the frontend tech logos
/// (rust / flutter / node / python / go / deno / dotnet). Most-specific markers
/// win; `package.json` (the most common) is checked last so a Tauri/Flutter repo
/// that also carries a frontend package.json still reads as rust/flutter.
pub fn detect_tech(root: &Path) -> Option<&'static str> {
    let has = |rel: &str| root.join(rel).exists();
    if has("pubspec.yaml") {
        return Some("flutter");
    }
    if has("Cargo.toml") {
        return Some("rust");
    }
    if has("pyproject.toml") || has("requirements.txt") || has("setup.py") || has("Pipfile") {
        return Some("python");
    }
    if has("go.mod") {
        return Some("go");
    }
    if has("deno.json") || has("deno.jsonc") {
        return Some("deno");
    }
    let has_dotnet = std::fs::read_dir(root).ok().is_some_and(|entries| {
        entries.flatten().any(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("csproj") || x.eq_ignore_ascii_case("sln"))
        })
    });
    if has_dotnet {
        return Some("dotnet");
    }
    if has("package.json") {
        return Some("node");
    }
    None
}

/// Marker-file tech detection for a project root, or None. Frontend tier-2
/// fallback when command parsing can't infer the tech.
#[tauri::command]
pub fn get_project_tech(root: String) -> Option<String> {
    detect_tech(Path::new(&root)).map(|s| s.to_string())
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

    #[test]
    fn detect_tech_prefers_specific_marker_over_package_json() {
        let dir = std::env::temp_dir().join(format!("ss_tech_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // A Python project that also ships a frontend package.json (like odysseus)
        // must read as python, not node.
        fs::write(dir.join("pyproject.toml"), b"x").unwrap();
        fs::write(dir.join("package.json"), b"{}").unwrap();
        assert_eq!(detect_tech(&dir), Some("python"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_tech_node_and_none() {
        let dir = std::env::temp_dir().join(format!("ss_tech_node_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(detect_tech(&dir), None);
        fs::write(dir.join("package.json"), b"{}").unwrap();
        assert_eq!(detect_tech(&dir), Some("node"));
        let _ = fs::remove_dir_all(&dir);
    }
}
