//! Detects a throwaway project root (agent worktree, scratch dir, OS temp,
//! git linked worktree) so `crud::ensure_and_run` can skip permanent
//! `projects.json` registration for it. See `Project::transient` in `types.rs`.

use std::path::{Component, Path};

pub struct Detection {
    pub transient: bool,
    pub label: Option<String>,
}

/// Cheap path-component checks run first; git is only shelled out to when one
/// of them already matched, or the path mentions `worktrees`, so a normal
/// `/run` on a real project never pays for a git spawn.
pub fn detect(root: &Path) -> Detection {
    let cheap_hit = has_component(root, ".for_bepy")
        || has_component(root, ".claude")
        || under_os_temp(root)
        || first_component_is_tmp(root);
    let mentions_worktrees = has_component(root, "worktrees");
    if !cheap_hit && !mentions_worktrees {
        return Detection { transient: false, label: None };
    }

    let branch = git_branch(root);
    let is_linked_worktree = git_dir(root).map(|d| is_worktree_gitdir(&d)).unwrap_or(false);
    if !cheap_hit && !is_linked_worktree {
        return Detection { transient: false, label: None };
    }
    let label = branch.unwrap_or_else(|| dir_label(root));
    Detection { transient: true, label: Some(label) }
}

fn has_component(path: &Path, name: &str) -> bool {
    path.components()
        .any(|c| c.as_os_str().to_str().map(|s| s.eq_ignore_ascii_case(name)).unwrap_or(false))
}

fn under_os_temp(root: &Path) -> bool {
    root.starts_with(std::env::temp_dir())
}

/// `C:\tmp\x\...` -> transient; `std::env::temp_dir()` resolves to
/// `AppData\Local\Temp` and would miss this real-world case.
fn first_component_is_tmp(path: &Path) -> bool {
    for c in path.components() {
        if let Component::Normal(s) = c {
            return s.to_str().map(|s| s.eq_ignore_ascii_case("tmp") || s.eq_ignore_ascii_case("temp")).unwrap_or(false);
        }
    }
    false
}

fn dir_label(root: &Path) -> String {
    root.file_name().and_then(|s| s.to_str()).unwrap_or("worktree").to_string()
}

fn is_worktree_gitdir(git_dir: &str) -> bool {
    git_dir.replace('\\', "/").contains("/worktrees/")
}

fn git_branch(root: &Path) -> Option<String> {
    // Detached HEAD reports literally "HEAD" - not a useful label, fall through.
    run_git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|s| s != "HEAD")
}

fn git_dir(root: &Path) -> Option<String> {
    run_git(root, &["rev-parse", "--git-dir"])
}

#[cfg(windows)]
fn run_git(root: &Path, args: &[&str]) -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    parse_output(out)
}

#[cfg(not(windows))]
fn run_git(root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).current_dir(root).output().ok()?;
    parse_output(out)
}

fn parse_output(out: std::process::Output) -> Option<String> {
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_bepy_component_is_transient() {
        let d = detect(Path::new(r"C:\Users\x\Desktop\Projects\myrepo\.for_bepy\scratch"));
        assert!(d.transient);
        assert_eq!(d.label.as_deref(), Some("scratch"));
    }

    #[test]
    fn claude_component_is_transient() {
        let d = detect(Path::new(r"C:\Users\x\Desktop\Projects\myrepo\.claude\worktrees\wt1\frontend"));
        assert!(d.transient);
    }

    #[test]
    fn os_temp_dir_is_transient() {
        let dir = tempfile::tempdir().unwrap();
        let d = detect(dir.path());
        assert!(d.transient, "a real tempfile::tempdir() lives under std::env::temp_dir()");
    }

    #[test]
    fn tmp_root_first_component_is_transient() {
        // A path that doesn't exist on disk, so the git probe cleanly fails
        // and the fallback dir-name label is deterministic in any environment.
        let d = detect(Path::new(r"C:\tmp\zz-nonexistent-scratch-dir-0000\frontend2"));
        assert!(d.transient);
        assert_eq!(d.label.as_deref(), Some("frontend2"));
    }

    #[test]
    fn normal_project_root_is_not_transient() {
        let d = detect(Path::new(r"C:\Users\x\Desktop\Projects\fibo\frontend"));
        assert!(!d.transient);
        assert!(d.label.is_none());
    }
}
