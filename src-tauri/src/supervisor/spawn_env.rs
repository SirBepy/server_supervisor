/// Parse newline-separated `KEY=VALUE` env overrides. Blank lines and `#`
/// comments are ignored. Each value is variable-expanded (see `expand_vars`).
pub(super) fn parse_env(raw: &str) -> Vec<(String, String)> {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), expand_vars(v.trim())))
        .collect()
}

/// Expand `${NAME}` and `%NAME%` references against the supervisor's own
/// environment. Unknown names expand to empty; a lone/unterminated sigil is kept
/// literally. This is what makes `PATH=C:\node;%PATH%` prepend rather than
/// clobber: env values set via `Command::env` are NOT expanded by the child, so
/// we resolve them here at spawn time.
fn expand_vars(input: &str) -> String {
    let mut out = String::new();
    let mut rest = input;
    while let Some(idx) = rest.find(['$', '%']) {
        out.push_str(&rest[..idx]);
        let tail = &rest[idx..];
        if let Some(after) = tail.strip_prefix("${") {
            if let Some(end) = after.find('}') {
                out.push_str(&std::env::var(&after[..end]).unwrap_or_default());
                rest = &after[end + 1..];
                continue;
            }
        } else if tail.starts_with('%') {
            if let Some(end) = tail[1..].find('%') {
                let name = &tail[1..1 + end];
                if !name.is_empty() {
                    out.push_str(&std::env::var(name).unwrap_or_default());
                    rest = &tail[1 + end + 1..];
                    continue;
                }
            }
        }
        // Not a recognized token: emit the sigil char literally and advance.
        let ch = tail.chars().next().unwrap();
        out.push(ch);
        rest = &tail[ch.len_utf8()..];
    }
    out.push_str(rest);
    out
}

/// Resolve reparse points (symlinks/junctions) in each PATH entry to their real
/// target directory. Windows refuses to traverse certain reparse points during
/// process creation, failing with "the path cannot be traversed because it
/// contains an untrusted mount point" - which breaks toolchains installed behind
/// a symlink, notably nvm-windows (`C:\nvm4w\nodejs` -> `...\nvm\v<ver>`).
/// Pointing the spawned child's PATH at the real directories leaves no reparse
/// point in the exec path, so `npm`/`npx`/`node` resolve and run. Entries that
/// don't exist (or can't be canonicalized) are kept verbatim.
#[cfg(windows)]
pub(super) fn resolve_path_dirs(path: &str) -> String {
    let resolved: Vec<std::path::PathBuf> = std::env::split_paths(path)
        .map(|p| match std::fs::canonicalize(&p) {
            Ok(real) => strip_verbatim(real),
            Err(_) => p,
        })
        .collect();
    std::env::join_paths(resolved)
        .map(|os| os.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

/// `std::fs::canonicalize` returns Windows extended-length (`\\?\C:\...`) paths,
/// which clutter PATH and some tools mishandle. Strip the verbatim prefix back to
/// an ordinary path (`\\?\UNC\server\share` -> `\\server\share`).
#[cfg(windows)]
pub(super) fn strip_verbatim(p: std::path::PathBuf) -> std::path::PathBuf {
    let s = p.to_string_lossy();
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        std::path::PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        std::path::PathBuf::from(rest)
    } else {
        p
    }
}

/// Read the persisted machine + user `Path` from the registry and merge them
/// (machine first, then user) the way Windows composes a logon session's PATH.
/// This lets spawned children resolve per-user toolchains (node via nvm,
/// cargo/rustup under `~/.cargo/bin`) even when the supervisor process itself was
/// started with a reduced env, e.g. autostarted at logon before the user profile
/// env materialized. Children otherwise inherit only the supervisor's own PATH,
/// so a logon-autostarted supervisor silently breaks every per-user toolchain.
/// `%VAR%` references (REG_EXPAND_SZ values) are expanded against the current
/// env. Returns `None` if neither key could be read, so the caller can fall back
/// to the inherited PATH.
#[cfg(windows)]
pub(super) fn registry_merged_path() -> Option<String> {
    let machine =
        query_reg_path(r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment");
    let user = query_reg_path(r"HKCU\Environment");
    if machine.is_none() && user.is_none() {
        return None;
    }
    let parts: Vec<String> = [machine, user]
        .into_iter()
        .flatten()
        .map(|s| expand_vars(&s))
        .filter(|s| !s.is_empty())
        .collect();
    Some(parts.join(";"))
}

/// Query a single registry key's `Path` value via `reg.exe`. Uses the absolute
/// `%SystemRoot%\System32\reg.exe` so it resolves even under a reduced PATH (the
/// exact failure mode this whole module guards against), and `CREATE_NO_WINDOW`
/// so the query never flashes a console. Returns `None` if the key/value is
/// absent (a machine with no user `Path` is normal) or the command fails.
#[cfg(windows)]
fn query_reg_path(key: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let reg_exe = std::env::var("SystemRoot")
        .map(|r| format!(r"{r}\System32\reg.exe"))
        .unwrap_or_else(|_| "reg.exe".to_string());
    let out = std::process::Command::new(reg_exe)
        .args(["query", key, "/v", "Path"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_reg_path(&String::from_utf8_lossy(&out.stdout))
}

/// Extract the `Path` data out of `reg query <key> /v Path` output, which looks
/// like:
/// ```text
///
/// HKEY_CURRENT_USER\Environment
///     Path    REG_EXPAND_SZ    C:\Users\me\.cargo\bin;%USERPROFILE%\bin
/// ```
/// The value-type token (`REG_SZ` / `REG_EXPAND_SZ`) is not localized by Windows,
/// so we split on it and take the trailing data. PATH data never spans newlines,
/// so a single line carries the whole value.
#[cfg(windows)]
fn parse_reg_path(output: &str) -> Option<String> {
    for line in output.lines() {
        for tok in ["REG_EXPAND_SZ", "REG_SZ"] {
            if let Some(idx) = line.find(tok) {
                let data = line[idx + tok.len()..].trim();
                if !data.is_empty() {
                    return Some(data.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_env;

    #[test]
    fn parse_env_skips_blanks_and_comments_and_splits_pairs() {
        let raw = "\n# a comment\nFOO=bar\n  BAZ = qux \nNOEQ\n";
        assert_eq!(
            parse_env(raw),
            vec![
                ("FOO".to_string(), "bar".to_string()),
                ("BAZ".to_string(), "qux".to_string()),
            ],
        );
    }

    #[test]
    fn parse_env_expands_known_vars_and_keeps_literals() {
        std::env::set_var("SS_TEST_VAR", "XYZ");
        let pairs = parse_env("A=${SS_TEST_VAR};%SS_TEST_VAR%\nB=100% raw $ sign");
        assert_eq!(pairs[0], ("A".to_string(), "XYZ;XYZ".to_string()));
        // Unterminated `%` and a lone `$` survive verbatim.
        assert_eq!(pairs[1], ("B".to_string(), "100% raw $ sign".to_string()));
        std::env::remove_var("SS_TEST_VAR");
    }

    #[cfg(windows)]
    #[test]
    fn strip_verbatim_removes_extended_prefix() {
        use std::path::PathBuf;
        assert_eq!(
            super::strip_verbatim(PathBuf::from(r"\\?\C:\foo\bar")),
            PathBuf::from(r"C:\foo\bar")
        );
        assert_eq!(
            super::strip_verbatim(PathBuf::from(r"\\?\UNC\srv\share")),
            PathBuf::from(r"\\srv\share")
        );
        assert_eq!(
            super::strip_verbatim(PathBuf::from(r"C:\plain")),
            PathBuf::from(r"C:\plain")
        );
    }

    #[cfg(windows)]
    #[test]
    fn resolve_path_dirs_keeps_missing_and_strips_verbatim_on_real() {
        let real = std::env::temp_dir();
        let missing = r"C:\definitely\not\here\ss_xyz123";
        let out = super::resolve_path_dirs(&format!("{};{}", real.display(), missing));
        assert!(out.contains(missing), "missing entry should be kept: {out}");
        assert!(!out.contains(r"\\?\"), "no extended-length prefix should leak: {out}");
    }

    #[cfg(windows)]
    #[test]
    fn resolve_path_dirs_follows_symlink_to_real_target() {
        use std::os::windows::fs::symlink_dir;
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        if symlink_dir(&real, &link).is_err() {
            return;
        }
        let out = super::resolve_path_dirs(&link.display().to_string());
        let want = super::strip_verbatim(std::fs::canonicalize(&real).unwrap());
        assert_eq!(out, want.display().to_string(), "symlink must resolve to real target");
    }

    #[cfg(windows)]
    #[test]
    fn parse_reg_path_extracts_data_after_type_token() {
        let out = "\r\nHKEY_CURRENT_USER\\Environment\r\n    Path    REG_EXPAND_SZ    C:\\Users\\me\\.cargo\\bin;%USERPROFILE%\\bin\r\n";
        assert_eq!(
            super::parse_reg_path(out).as_deref(),
            Some(r"C:\Users\me\.cargo\bin;%USERPROFILE%\bin")
        );
    }

    #[cfg(windows)]
    #[test]
    fn parse_reg_path_handles_plain_reg_sz_and_missing() {
        let sz = "    Path    REG_SZ    C:\\sys32;C:\\Windows";
        assert_eq!(super::parse_reg_path(sz).as_deref(), Some(r"C:\sys32;C:\Windows"));
        // No value line -> None (e.g. the key has no Path).
        assert_eq!(super::parse_reg_path("HKEY_CURRENT_USER\\Environment\r\n"), None);
    }

    #[cfg(windows)]
    #[test]
    fn registry_merged_path_includes_real_machine_path() {
        // The machine `Path` always exists, so a merge must return Some and
        // contain System32 (always on the machine PATH). Proves the reg query +
        // parse round-trips against the live registry on this box.
        let merged = super::registry_merged_path().expect("machine Path must read");
        assert!(
            merged.to_lowercase().contains("system32"),
            "merged registry PATH should contain System32: {merged}"
        );
        // %VAR% references must be expanded, never left literal.
        assert!(!merged.contains('%'), "expanded PATH should have no %VAR%: {merged}");
    }
}
