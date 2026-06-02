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
}
