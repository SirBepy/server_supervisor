//! Advisory, non-blocking command validity check. Given a project root and a
//! command string, best-effort RESOLVES whether the command's executable
//! exists. It NEVER runs the user's command (only `where.exe` / fs probes) and
//! when in doubt returns `ok: true` to avoid false positives.

use serde::{Deserialize, Serialize};
use std::path::Path;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct CommandCheck {
    pub ok: bool,
    pub reason: String, // empty when ok; a short human hint when !ok
}

impl CommandCheck {
    fn ok() -> Self {
        CommandCheck {
            ok: true,
            reason: String::new(),
        }
    }
    fn bad(reason: impl Into<String>) -> Self {
        CommandCheck {
            ok: false,
            reason: reason.into(),
        }
    }
}

/// Builtins that are valid commands but never appear on PATH.
const BUILTINS: [&str; 19] = [
    "cd", "echo", "set", "dir", "type", "copy", "move", "del", "mkdir", "rmdir", "cls", "exit",
    "call", "start", "where", "ver", "title", "pushd", "popd",
];

/// Common Windows executable / script extensions to try when a token has none.
const EXE_EXTS: [&str; 4] = [".exe", ".cmd", ".bat", ".ps1"];
/// Extensions for node_modules/.bin shims.
const BIN_EXTS: [&str; 4] = ["", ".cmd", ".ps1", ".exe"];

/// Strip a single pair of surrounding quotes from a token.
fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && ((b[0] == b'"' && b[b.len() - 1] == b'"') || (b[0] == b'\'' && b[b.len() - 1] == b'\'')) {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Read `<root>/package.json` and return the `scripts` object's keys, if any.
/// Returns `None` when the file can't be read or parsed (so the caller can
/// avoid guessing); `Some(empty)` when there's no scripts object.
fn package_scripts(root: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    match json.get("scripts").and_then(|s| s.as_object()) {
        Some(scripts) => Some(scripts.keys().cloned().collect()),
        None => Some(Vec::new()),
    }
}

/// Detect an `npm run X` / `pnpm run X` / `yarn run X` invocation, returning the
/// script name `X`. Bare `yarn X` is intentionally NOT matched (ambiguous).
fn npm_script_name(cmd: &str) -> Option<&str> {
    let mut it = cmd.split_whitespace();
    let first = it.next()?;
    let lc = first.to_ascii_lowercase();
    let is_runner = lc == "npm" || lc == "pnpm" || lc == "yarn";
    if !is_runner {
        return None;
    }
    let second = it.next()?;
    if !second.eq_ignore_ascii_case("run") {
        return None;
    }
    it.next()
}

pub fn validate_command(root: &str, cmd: &str) -> CommandCheck {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return CommandCheck::ok();
    }
    let root = Path::new(root);

    // 1. npm/pnpm/yarn script ground-truth check.
    if let Some(script) = npm_script_name(cmd) {
        match package_scripts(root) {
            Some(scripts) if scripts.iter().any(|s| s == script) => return CommandCheck::ok(),
            Some(_) => return CommandCheck::bad(format!("no script \"{script}\" in package.json")),
            // Can't read/parse package.json -> don't guess.
            None => return CommandCheck::ok(),
        }
    }

    // 2. First token -> the executable.
    let Some(raw_exe) = cmd.split_whitespace().next() else {
        return CommandCheck::ok();
    };
    let exe = unquote(raw_exe);
    if exe.is_empty() {
        return CommandCheck::ok();
    }
    let exe_lc = exe.to_ascii_lowercase();

    // 3. Builtins are valid but not on PATH.
    if BUILTINS.iter().any(|b| *b == exe_lc) {
        return CommandCheck::ok();
    }

    resolve_exe(root, exe)
}

/// Does `path` exist either as-is or with one of the given extensions appended?
fn exists_with_exts(path: &Path, exts: &[&str]) -> bool {
    if path.is_file() {
        return true;
    }
    // Only try extension variants when the path has no extension already.
    if path.extension().is_none() {
        for ext in exts {
            if ext.is_empty() {
                continue;
            }
            let mut p = path.as_os_str().to_owned();
            p.push(ext);
            if Path::new(&p).is_file() {
                return true;
            }
        }
    }
    false
}

#[cfg(windows)]
fn resolve_exe(root: &Path, exe: &str) -> CommandCheck {
    // Path-like token: resolve relative to root or as an absolute path.
    let looks_like_path = exe.contains('/') || exe.contains('\\') || exe.starts_with('.');
    if looks_like_path {
        let candidates = [root.join(exe), Path::new(exe).to_path_buf()];
        for c in candidates {
            if exists_with_exts(&c, &EXE_EXTS) {
                return CommandCheck::ok();
            }
        }
        return CommandCheck::bad(format!("file \"{exe}\" not found"));
    }

    // node_modules/.bin shim.
    let bin = root.join("node_modules").join(".bin").join(exe);
    if exists_with_exts(&bin, &BIN_EXTS) {
        return CommandCheck::ok();
    }

    // PATH resolution via `where.exe` (a safe instant system utility; does NOT
    // run the user's command). CREATE_NO_WINDOW avoids a console flash.
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = std::process::Command::new("where")
        .arg(exe)
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match status {
        Ok(o) if o.status.success() => CommandCheck::ok(),
        Ok(_) => CommandCheck::bad(format!("\"{exe}\" not found on PATH")),
        // `where` itself failed to launch -> be conservative, don't flag.
        Err(_) => CommandCheck::ok(),
    }
}

#[cfg(not(windows))]
fn resolve_exe(_root: &Path, _exe: &str) -> CommandCheck {
    // Windows-only app; on other platforms never flag.
    CommandCheck::ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn missing_npm_script_is_flagged() {
        let dir = tmp();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"dev":"vite"}}"#,
        )
        .unwrap();
        let root = dir.path().to_str().unwrap();
        assert!(!validate_command(root, "npm run nope").ok);
        assert!(validate_command(root, "npm run dev").ok);
    }

    #[test]
    fn gibberish_exe_is_flagged() {
        let dir = tmp();
        let root = dir.path().to_str().unwrap();
        assert!(!validate_command(root, "asdasdasd").ok);
    }

    #[test]
    fn real_exe_is_ok() {
        let dir = tmp();
        let root = dir.path().to_str().unwrap();
        // cmd resolves on PATH.
        assert!(validate_command(root, "cmd /C echo hi").ok);
        // echo is a builtin in the allowlist.
        assert!(validate_command(root, "echo hi").ok);
    }

    #[test]
    fn empty_is_ok() {
        let dir = tmp();
        let root = dir.path().to_str().unwrap();
        assert!(validate_command(root, "   ").ok);
    }

    #[test]
    fn no_package_json_doesnt_false_flag_npm() {
        let dir = tmp();
        let root = dir.path().to_str().unwrap();
        // Can't read scripts -> don't guess. npm itself resolves on PATH.
        assert!(validate_command(root, "npm run whatever").ok);
    }
}
