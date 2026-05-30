//! Auto-detect runnable commands in a project folder, in descending reliability:
//! package.json scripts, .vscode/launch.json (Dart/Flutter), Cargo.toml binaries,
//! .vscode/tasks.json, then README heuristics. Structured sources are emitted
//! before the fuzzy README scan so dedup (keyed on cmd) prefers them.

use crate::types::{DetectedCommand, ProcKind};
use std::path::Path;

pub fn detect(root: &Path) -> Vec<DetectedCommand> {
    let mut out = Vec::new();
    out.extend(from_package_json(root));
    out.extend(from_launch_json(root));
    out.extend(from_cargo(root));
    out.extend(from_tasks_json(root));
    out.extend(from_readme(root));
    dedup(out)
}

fn kind_of(cmd: &str) -> ProcKind {
    ProcKind::infer(cmd)
}

fn dedup(mut items: Vec<DetectedCommand>) -> Vec<DetectedCommand> {
    let mut seen = std::collections::HashSet::new();
    items.retain(|d| seen.insert(d.cmd.clone()));
    items
}

fn from_package_json(root: &Path) -> Vec<DetectedCommand> {
    let Ok(text) = std::fs::read_to_string(root.join("package.json")) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(scripts) = json.get("scripts").and_then(|s| s.as_object()) else {
        return Vec::new();
    };
    scripts
        .iter()
        .map(|(name, body)| {
            let body_str = body.as_str().unwrap_or("");
            DetectedCommand {
                source: "package.json".to_string(),
                name: name.clone(),
                cmd: format!("npm run {name}"),
                kind: kind_of(body_str),
            }
        })
        .collect()
}

/// Strip `//` line comments and trailing commas so a JSONC launch.json parses.
fn strip_jsonc(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for line in input.lines() {
        // Drop a `//` comment that isn't inside a string (best-effort: ignore
        // `//` that appears after a quote on the same line is rare in launch.json).
        let cleaned = match line.find("//") {
            Some(idx) if !line[..idx].contains('"') => &line[..idx],
            _ => line,
        };
        out.push_str(cleaned);
        out.push('\n');
    }
    // Remove trailing commas before } or ].
    let mut result = String::with_capacity(out.len());
    let bytes = out.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b',' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                i += 1;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

fn from_launch_json(root: &Path) -> Vec<DetectedCommand> {
    let Ok(raw) = std::fs::read_to_string(root.join(".vscode").join("launch.json")) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(&raw)) else {
        return Vec::new();
    };
    let Some(configs) = json.get("configurations").and_then(|c| c.as_array()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for cfg in configs {
        let cfg_type = cfg.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let name = cfg
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("launch")
            .to_string();
        // Dart/Flutter launch configs map cleanly to `flutter run`.
        if cfg_type == "dart" {
            let mut cmd = String::from("fvm flutter run");
            if let Some(args) = cfg.get("args").and_then(|a| a.as_array()) {
                for a in args {
                    if let Some(s) = a.as_str() {
                        cmd.push(' ');
                        cmd.push_str(s);
                    }
                }
            }
            out.push(DetectedCommand {
                source: "launch.json".to_string(),
                name,
                cmd,
                kind: ProcKind::Flutter,
            });
        }
    }
    out
}

/// `cargo run` for a binary crate, plus `cargo run --bin <name>` for each
/// `src/bin/*.rs`. Keyed off the canonical filesystem layout rather than parsing
/// Cargo.toml, which stays robust without pulling in a TOML dependency. Library-
/// only crates (no `src/main.rs`, no `src/bin/`) yield nothing.
fn from_cargo(root: &Path) -> Vec<DetectedCommand> {
    if !root.join("Cargo.toml").exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if root.join("src").join("main.rs").exists() {
        out.push(DetectedCommand {
            source: "Cargo.toml".to_string(),
            name: "cargo run".to_string(),
            cmd: "cargo run".to_string(),
            kind: ProcKind::Generic,
        });
    }
    if let Ok(entries) = std::fs::read_dir(root.join("src").join("bin")) {
        // Sort for deterministic order (read_dir order is OS-dependent).
        let mut bins: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.extension().and_then(|x| x.to_str()) == Some("rs") {
                    path.file_stem().and_then(|s| s.to_str()).map(str::to_string)
                } else {
                    None
                }
            })
            .collect();
        bins.sort();
        for name in bins {
            out.push(DetectedCommand {
                source: "Cargo.toml".to_string(),
                name: format!("cargo run --bin {name}"),
                cmd: format!("cargo run --bin {name}"),
                kind: ProcKind::Generic,
            });
        }
    }
    out
}

/// VS Code `tasks.json` (JSONC). Each task with a concrete `command` becomes a
/// command (its `args` appended); compound tasks (only `dependsOn`, no command)
/// are skipped.
fn from_tasks_json(root: &Path) -> Vec<DetectedCommand> {
    let Ok(raw) = std::fs::read_to_string(root.join(".vscode").join("tasks.json")) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(&raw)) else {
        return Vec::new();
    };
    let Some(tasks) = json.get("tasks").and_then(|t| t.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for task in tasks {
        let Some(command) = task.get("command").and_then(|c| c.as_str()) else {
            continue;
        };
        let mut cmd = command.to_string();
        if let Some(args) = task.get("args").and_then(|a| a.as_array()) {
            for a in args {
                if let Some(s) = a.as_str() {
                    cmd.push(' ');
                    cmd.push_str(s);
                }
            }
        }
        let name = task
            .get("label")
            .and_then(|l| l.as_str())
            .unwrap_or(cmd.as_str())
            .to_string();
        out.push(DetectedCommand {
            source: "tasks.json".to_string(),
            name,
            kind: kind_of(&cmd),
            cmd,
        });
    }
    out
}

fn from_readme(root: &Path) -> Vec<DetectedCommand> {
    let candidates = ["README.md", "readme.md", "Readme.md", "README"];
    let mut text = String::new();
    for c in candidates {
        if let Ok(t) = std::fs::read_to_string(root.join(c)) {
            text = t;
            break;
        }
    }
    if text.is_empty() {
        return Vec::new();
    }

    const NEEDLES: [&str; 7] = [
        "npm run ",
        "npm start",
        "pnpm ",
        "yarn ",
        "flutter run",
        "fvm flutter run",
        "cargo run",
    ];
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim().trim_start_matches('`').trim_matches('`').trim();
        for needle in NEEDLES {
            if let Some(idx) = trimmed.find(needle) {
                let cmd: String = trimmed[idx..]
                    .split('`')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if cmd.len() >= needle.len() && cmd.len() < 120 {
                    out.push(DetectedCommand {
                        source: "readme".to_string(),
                        name: cmd.clone(),
                        cmd: cmd.clone(),
                        kind: kind_of(&cmd),
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_comments_and_trailing_commas() {
        let jsonc = "{\n  // a comment\n  \"a\": 1,\n  \"b\": [1, 2,],\n}";
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(jsonc)).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn detects_package_scripts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"dev:up":"node x","build":"flutter build"}}"#,
        )
        .unwrap();
        let found = from_package_json(dir.path());
        assert!(found.iter().any(|d| d.cmd == "npm run dev:up"));
        let build = found.iter().find(|d| d.name == "build").unwrap();
        assert_eq!(build.kind, ProcKind::Flutter);
    }

    #[test]
    fn detects_cargo_run_and_bins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::create_dir_all(dir.path().join("src").join("bin")).unwrap();
        std::fs::write(dir.path().join("src").join("main.rs"), "fn main(){}").unwrap();
        std::fs::write(dir.path().join("src").join("bin").join("worker.rs"), "fn main(){}").unwrap();
        std::fs::write(dir.path().join("src").join("bin").join("api.rs"), "fn main(){}").unwrap();

        let found = from_cargo(dir.path());
        let cmds: Vec<&str> = found.iter().map(|d| d.cmd.as_str()).collect();
        assert_eq!(cmds, vec!["cargo run", "cargo run --bin api", "cargo run --bin worker"]);
        assert!(found.iter().all(|d| d.kind == ProcKind::Generic && d.source == "Cargo.toml"));
    }

    #[test]
    fn cargo_library_crate_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"lib\"\n").unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("lib.rs"), "").unwrap();
        assert!(from_cargo(dir.path()).is_empty(), "a library crate has nothing to run");
    }

    #[test]
    fn detects_tasks_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".vscode")).unwrap();
        std::fs::write(
            dir.path().join(".vscode").join("tasks.json"),
            "{\n  // tasks\n  \"version\":\"2.0.0\",\n  \"tasks\":[\n    {\"label\":\"serve\",\"type\":\"shell\",\"command\":\"cargo\",\"args\":[\"run\"],},\n    {\"label\":\"all\",\"dependsOn\":[\"serve\"]}\n  ],\n}",
        )
        .unwrap();
        let found = from_tasks_json(dir.path());
        assert_eq!(found.len(), 1, "compound task without a command is skipped");
        assert_eq!(found[0].name, "serve");
        assert_eq!(found[0].cmd, "cargo run");
        assert_eq!(found[0].source, "tasks.json");
    }

    #[test]
    fn detects_dart_launch_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".vscode")).unwrap();
        std::fs::write(
            dir.path().join(".vscode").join("launch.json"),
            "{\n  // launch\n  \"configurations\": [\n    {\"name\":\"dev\",\"type\":\"dart\",\"args\":[\"--flavor\",\"dev\"],}\n  ],\n}",
        )
        .unwrap();
        let found = from_launch_json(dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].cmd, "fvm flutter run --flavor dev");
        assert_eq!(found[0].kind, ProcKind::Flutter);
    }
}
