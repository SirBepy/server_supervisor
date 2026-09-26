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

/// True if `line` is a function signature `generate_handler!` could reach:
/// any `pub`/`pub(...)` visibility, followed by zero or more of
/// `extern "C"`/`extern`/`async`/`unsafe`/`const`, followed by `fn`.
/// `generate_handler!` only needs the item path reachable in-crate, so
/// `pub(crate)`/`pub(super)` are genuinely registrable, not a narrower case
/// than plain `pub`.
fn is_command_signature(line: &str) -> bool {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("pub") {
        return false;
    }
    let mut rest = &trimmed[3..];
    if rest.starts_with('(') {
        match rest.find(')') {
            Some(close) => rest = &rest[close + 1..],
            None => return false,
        }
    }
    loop {
        rest = rest.trim_start();
        if rest.starts_with("fn ") || rest.starts_with("fn(") {
            return true;
        }
        let qualifiers = ["extern \"C\"", "extern", "async", "unsafe", "const"];
        match qualifiers.iter().find(|q| rest.starts_with(**q)) {
            Some(q) => rest = &rest[q.len()..],
            None => return false,
        }
    }
}

/// Finds where the `#[tauri::command...]` attribute starting at `lines[start]`
/// closes, tracking `[`/`]` depth like `parse_registered_commands` tracks
/// bracket depth below, so a multi-line arg list (`#[tauri::command(` on one
/// line, `)]` on a later one) isn't mistaken for closing at the first line.
/// Returns `(lines.len(), 0)` if the attribute never closes, since that's not
/// a valid position for the caller to slice.
fn attribute_end(lines: &[&str], start: usize) -> (usize, usize) {
    let mut depth = 0i32;
    for (li, line) in lines[start..].iter().enumerate() {
        for (ci, ch) in line.char_indices() {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return (start + li, ci + 1);
                    }
                }
                _ => {}
            }
        }
    }
    (lines.len(), 0)
}

/// Walks forward from the closing `]` of the attribute at `lines[attr_idx]`
/// to the function it applies to, skipping blanks, further attributes,
/// `//`/`///` comments and `/* */` block comments, and also checking the
/// attribute's own line for the attribute-and-signature-on-one-line shape.
/// Returns `None` only when a real, non-skippable, non-signature line is hit
/// (or the attribute never closes) - the caller turns that into a panic
/// rather than a dropped command.
fn resolve_command_name(lines: &[&str], attr_idx: usize) -> Option<String> {
    let (end_line, end_col) = attribute_end(lines, attr_idx);
    if end_line >= lines.len() {
        return None;
    }

    let remainder = lines[end_line][end_col..].trim();
    if !remainder.is_empty() {
        return if is_command_signature(remainder) {
            extract_fn_name(remainder)
        } else {
            None
        };
    }

    let mut idx = end_line + 1;
    while idx < lines.len() {
        let candidate = lines[idx].trim();
        if candidate.is_empty()
            || candidate.starts_with("#[")
            || candidate.starts_with("///")
            || candidate.starts_with("//")
        {
            idx += 1;
            continue;
        }
        if candidate.starts_with("/*") {
            if !candidate.contains("*/") {
                idx += 1;
                while idx < lines.len() && !lines[idx].contains("*/") {
                    idx += 1;
                }
            }
            idx += 1;
            continue;
        }
        return if is_command_signature(candidate) {
            extract_fn_name(candidate)
        } else {
            None
        };
    }
    None
}

/// Scans a source file's lines for `#[tauri::command]`, `#[tauri::command(async)]`,
/// or any other `#[tauri::command(...)]` form (including one spanning several
/// lines), then resolves each to the function it applies to via
/// `resolve_command_name`.
///
/// DECISION (todo 0048): keep this hand-rolled scan rather than pulling in
/// `syn`. `syn` would be correct by construction, but the fail-loud panic
/// below already closes the failure mode `syn` was proposed to fix - a gap
/// this scanner still has now announces itself as a red test instead of a
/// silent pass, so a simpler parser stays strictly better once it can't go
/// quiet. 0047's no-new-dependency constraint stands.
///
/// DECISION (todo 0048): a `#[tauri::command]` inside a `#[cfg(test)]` block
/// is excluded, tracked by brace depth (`brace_depth`/`cfg_test_entry_depth`
/// below), the same technique `parse_registered_commands` uses for bracket
/// depth. Such an attribute is not compiled into the release binary and is
/// not registrable, so a fail-loud scanner reporting it would be a false
/// positive - and one false positive is all it takes for a loud test to get
/// muted, which recreates the exact silent-miss problem this test exists to
/// prevent.
fn find_annotated_commands(content: &str, file: &Path) -> Vec<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut found = Vec::new();

    let mut brace_depth = 0i32;
    let mut cfg_test_entry_depth: Option<i32> = None;
    let mut pending_cfg_test = false;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if trimmed.starts_with("#[cfg(test)]") {
            pending_cfg_test = true;
        }

        let opens = line.matches('{').count() as i32;
        let closes = line.matches('}').count() as i32;

        if pending_cfg_test && opens > 0 {
            cfg_test_entry_depth = Some(brace_depth + 1);
            pending_cfg_test = false;
        }

        brace_depth += opens - closes;

        if let Some(entry_depth) = cfg_test_entry_depth {
            if brace_depth < entry_depth {
                cfg_test_entry_depth = None;
            }
        }

        if cfg_test_entry_depth.is_some() || !trimmed.starts_with("#[tauri::command") {
            continue;
        }

        match resolve_command_name(&lines, i) {
            Some(name) => found.push(name),
            None => panic!(
                "{}:{}: found `#[tauri::command]` but could not resolve the \
                 function it applies to; teach the scanner this shape rather \
                 than silently dropping the command (todo 0048)",
                file.display(),
                i + 1
            ),
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
        annotated.extend(find_annotated_commands(&content, file));
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

// Permanent regression coverage for todo 0048's four blind-spot shapes, plus
// the cfg(test) exclusion and the fail-loud panic. Exercised against inline
// fixtures rather than the real tree so they hold even after the real tree's
// shapes change.

#[test]
fn resolves_multiline_attribute() {
    let src = "#[tauri::command(\n    rename_all = \"camelCase\"\n)]\npub fn get_status() -> String {\n    String::new()\n}\n";
    let commands = find_annotated_commands(src, Path::new("fixture.rs"));
    assert_eq!(commands, vec!["get_status".to_string()]);
}

#[test]
fn resolves_broadened_visibility_and_qualifiers() {
    let signatures = [
        "pub(crate) fn a() {}",
        "pub(super) fn b() {}",
        "pub unsafe fn c() {}",
        "pub const fn d() {}",
        "pub(crate) async fn e() {}",
    ];
    for sig in signatures {
        let src = format!("#[tauri::command]\n{sig}\n");
        let commands = find_annotated_commands(&src, Path::new("fixture.rs"));
        assert_eq!(commands.len(), 1, "failed to resolve: {sig}");
    }
}

#[test]
fn resolves_attribute_and_signature_on_one_line() {
    let src = "#[tauri::command] pub fn one_liner() -> String { String::new() }\n";
    let commands = find_annotated_commands(src, Path::new("fixture.rs"));
    assert_eq!(commands, vec!["one_liner".to_string()]);
}

#[test]
fn resolves_past_block_comment() {
    let src = "#[tauri::command]\n/* explains the command */\npub fn documented() -> String {\n    String::new()\n}\n";
    let commands = find_annotated_commands(src, Path::new("fixture.rs"));
    assert_eq!(commands, vec!["documented".to_string()]);
}

#[test]
fn excludes_commands_inside_cfg_test_blocks() {
    let src = "#[cfg(test)]\nmod tests {\n    #[tauri::command]\n    pub fn fake_command() {}\n}\n";
    let commands = find_annotated_commands(src, Path::new("fixture.rs"));
    assert!(commands.is_empty(), "expected no commands, got {commands:?}");
}

#[test]
fn still_finds_real_command_after_a_cfg_test_block_closes() {
    let src = "#[cfg(test)]\nmod tests {\n    #[tauri::command]\n    pub fn fake_command() {}\n}\n\n#[tauri::command]\npub fn real_command() {}\n";
    let commands = find_annotated_commands(src, Path::new("fixture.rs"));
    assert_eq!(commands, vec!["real_command".to_string()]);
}

#[test]
#[should_panic(expected = "fixture.rs:1")]
fn panics_on_unresolvable_attribute() {
    let src = "#[tauri::command]\nstruct NotAFunction;\n";
    find_annotated_commands(src, Path::new("fixture.rs"));
}
