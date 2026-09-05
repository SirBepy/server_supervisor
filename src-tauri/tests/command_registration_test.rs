// A `#[tauri::command]` fn compiles and links fine whether or not it is ever
// added to `tauri::generate_handler![]` in lib.rs - the list is hand-maintained
// and nothing in the type system enforces membership. The only failure is at
// runtime: `invoke()` rejects with "command not found", which a silent
// frontend catch can swallow with zero signal (this exact chain killed
// get_disk_usage for a whole release - see todo 0047). This test is the only
// thing that turns a missing registration into a red test instead of a
// shipped dead feature.

use std::fs;
use std::path::{Path, PathBuf};

/// Recursively collect every `.rs` file under `root`, skipping `target` and
/// `tests` directories if encountered (mirrors cargo's own exclusions; this
/// walk only ever runs against `src-tauri/src`, but stays defensive).
fn collect_rs_files(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "target" || name == "tests" {
                continue;
            }
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Pulls the identifier following the first `fn ` on a line, i.e. the command
/// name. Stops at the first non-identifier char, so both `pub fn name(` and
/// `pub async fn name(` resolve to `name`.
fn extract_fn_name(line: &str) -> Option<String> {
    let idx = line.find("fn ")?;
    let after = &line[idx + 3..];
    let name: String = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Scans a source file's lines for `#[tauri::command]`, `#[tauri::command(async)]`,
/// or any other `#[tauri::command(...)]` form, then walks forward past any
/// intervening attributes/doc-comments/blank lines to the `pub fn`/`pub async fn`
/// that attribute applies to, and records its name.
fn find_annotated_commands(content: &str) -> Vec<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.starts_with("#[tauri::command") {
            continue;
        }
        for candidate in &lines[i + 1..] {
            let c = candidate.trim();
            if c.is_empty() || c.starts_with("#[") || c.starts_with("///") || c.starts_with("//") {
                continue;
            }
            if c.starts_with("pub fn") || c.starts_with("pub async fn") {
                if let Some(name) = extract_fn_name(c) {
                    found.push(name);
                }
            }
            break;
        }
    }
    found
}

/// Parses the `tauri::generate_handler![ ... ]` list out of lib.rs, taking the
/// last path segment of each entry (entries are written like
/// `ipc::commands::get_disk_usage`). Bracket-depth counted rather than
/// string-split on `]`, so nested `[]` (none exist today, but nothing here
/// assumes it) can't truncate the scan early.
fn parse_registered_commands(lib_rs: &str) -> Vec<String> {
    let marker = "tauri::generate_handler![";
    let start = lib_rs
        .find(marker)
        .expect("lib.rs must contain a tauri::generate_handler![...] list")
        + marker.len();

    let bytes = lib_rs.as_bytes();
    let mut depth = 1i32;
    let mut end = None;
    for (offset, &b) in bytes[start..].iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.expect("unterminated generate_handler![ list in lib.rs");
    let body = &lib_rs[start..end];

    body.split(',')
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            entry
                .rsplit("::")
                .next()
                .expect("split always yields at least one segment")
                .to_string()
        })
        .collect()
}

#[test]
fn every_tauri_command_is_registered() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let src_root = Path::new(manifest_dir).join("src");

    let mut rs_files = Vec::new();
    collect_rs_files(&src_root, &mut rs_files);
    assert!(
        !rs_files.is_empty(),
        "walk found no .rs files under {}; the walk itself is broken",
        src_root.display()
    );

    let mut annotated = Vec::new();
    for file in &rs_files {
        let content = fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", file.display()));
        annotated.extend(find_annotated_commands(&content));
    }

    let lib_rs_path = src_root.join("lib.rs");
    let lib_rs = fs::read_to_string(&lib_rs_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", lib_rs_path.display()));
    let registered = parse_registered_commands(&lib_rs);

    let missing: Vec<&String> = annotated
        .iter()
        .filter(|name| !registered.contains(name))
        .collect();

    assert!(
        missing.is_empty(),
        "the following #[tauri::command] functions are never added to \
         tauri::generate_handler![] in lib.rs, so invoke() will reject them \
         at runtime with \"command not found\": {:?}",
        missing
    );
}
