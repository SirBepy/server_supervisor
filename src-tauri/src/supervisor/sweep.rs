//! Launch-time sweep of throwaway checkouts out of the saved project list.
//! `transient::detect` keeps NEW worktree/scratch `/run`s out of
//! `projects.json`, but a row saved before that existed, or from a checkout it
//! could not see, stays forever: the list grew five `frontend` rows and five
//! `frontend2` rows from ordinary worktree and `C:\tmp` workflow (todo 0028).
//!
//! A worktree row folds its commands into the project at the matching path in
//! the main checkout; a scratch row (`.for_bepy`, `C:\tmp`, OS temp) has no
//! parent and is dropped. A row with a live process is never touched, and
//! `projects.json` is copied to `projects.json.bak-sweep` before any change.

use super::config;
use super::proc::ManagedProc;
use super::registry::Supervisor;
use super::transient;
use crate::types::{unit_id, ProcSpec, Project};
use std::path::Path;

#[derive(Debug, PartialEq)]
pub(super) enum Verdict {
    Keep,
    FoldInto(String),
    Drop,
}

/// Pure classification of one row against the whole list. Path rules only, no
/// git spawn: unlike `transient::detect`, a bare `.claude` component is not
/// enough here, since a project rooted at `~/.claude` itself is a real one.
pub(super) fn classify(p: &Project, all: &[Project]) -> Verdict {
    if p.transient {
        return Verdict::Keep; // `prune_dead_transients` owns these
    }
    if let Some(main) = worktree_main(&p.root) {
        let main = norm(&main);
        return match all.iter().find(|q| q.id != p.id && norm(&q.root) == main) {
            Some(q) if q.transient || is_scratch(&q.root) || worktree_main(&q.root).is_some() => {
                Verdict::Drop
            }
            Some(q) => Verdict::FoldInto(q.id.clone()),
            None => Verdict::Drop,
        };
    }
    if is_scratch(&p.root) {
        Verdict::Drop
    } else {
        Verdict::Keep
    }
}

/// `<repo>/.claude/worktrees/<name>[/<rel>]` -> `<repo>[/<rel>]`, which works
/// even after the worktree folder is deleted; else a live linked worktree's
/// `.git` file.
fn worktree_main(root: &str) -> Option<String> {
    let r = root.replace('\\', "/");
    let lower = r.to_lowercase();
    if let Some(i) = lower.find("/.claude/worktrees/") {
        let after = &r[i + "/.claude/worktrees/".len()..];
        return Some(match after.find('/') {
            Some(j) => format!("{}{}", &r[..i], &after[j..]),
            None => r[..i].to_string(),
        });
    }
    transient::linked_worktree_main(Path::new(root)).map(|p| p.to_string_lossy().into_owned())
}

fn is_scratch(root: &str) -> bool {
    let p = Path::new(root);
    transient::has_component(p, ".for_bepy")
        || transient::under_os_temp(p)
        || transient::first_component_is_tmp(p)
}

fn norm(p: &str) -> String {
    p.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

fn norm_cmd(c: &str) -> String {
    c.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Supervisor {
    /// Runs once per launch, after re-adoption, so a live process is known.
    pub(super) fn sweep_throwaway_projects(&self) {
        let mut projects = self.projects.lock().unwrap();
        let mut procs = self.procs.lock().unwrap();
        let is_live = |p: &Project| {
            p.commands
                .iter()
                .any(|c| procs.get(&unit_id(&p.id, &c.id)).map_or(false, |m| m.pid.is_some()))
        };
        let plan: Vec<(String, Verdict)> = projects
            .iter()
            .map(|p| (p.id.clone(), classify(p, &projects)))
            .filter(|(_, v)| *v != Verdict::Keep)
            .filter(|(id, _)| !projects.iter().any(|p| &p.id == id && is_live(p)))
            .collect();
        if plan.is_empty() {
            return;
        }

        let data = self.data_dir.join("projects.json");
        let _ = std::fs::copy(&data, self.data_dir.join("projects.json.bak-sweep"));

        for (id, verdict) in &plan {
            let Some(idx) = projects.iter().position(|p| &p.id == id) else { continue };
            let gone = projects.remove(idx);
            for c in &gone.commands {
                procs.remove(&unit_id(&gone.id, &c.id));
            }
            self.ports.release_project(&gone.id);
            let Verdict::FoldInto(parent_id) = verdict else {
                log::info!("sweep: dropped throwaway project {} ({})", gone.id, gone.root);
                continue;
            };
            let Some(parent) = projects.iter_mut().find(|p| &p.id == parent_id) else { continue };
            for mut c in gone.commands {
                if parent.commands.iter().any(|pc| norm_cmd(&pc.cmd) == norm_cmd(&c.cmd)) {
                    continue;
                }
                c.id = super::crud::unique_id(&c.id, &|cand| parent.commands.iter().any(|pc| pc.id == cand));
                // The checkout it ran in may be gone; never relaunch it unasked.
                c.autostart = false;
                let spec = ProcSpec::from_unit(parent, &c);
                procs.insert(spec.id.clone(), ManagedProc::new(spec));
                parent.commands.push(c);
            }
            log::info!("sweep: folded worktree project {} into {}", gone.id, parent_id);
        }
        config::save(&self.data_dir, &projects);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::PortRegistry;
    use std::sync::Arc;

    fn project(id: &str, root: &str, cmds: &[(&str, &str)]) -> serde_json::Value {
        let commands: Vec<_> = cmds
            .iter()
            .map(|(cid, cmd)| serde_json::json!({ "id": cid, "name": cid, "cmd": cmd, "autostart": true }))
            .collect();
        serde_json::json!({ "id": id, "name": id, "root": root, "commands": commands })
    }

    fn parse(v: &serde_json::Value) -> Project {
        serde_json::from_value(v.clone()).unwrap()
    }

    #[test]
    fn classify_folds_worktrees_drops_scratch_keeps_real() {
        let all: Vec<Project> = [
            project("frontend", r"C:\p\fibo\frontend", &[]),
            project("fibo", "C:/p/fibo", &[]),
            project("frontend-2", "C:/p/fibo/.claude/worktrees/wt1/frontend", &[]),
            project("wt-root", r"C:\p\fibo\.claude\worktrees\wt2", &[]),
            project("orphan-wt", "C:/p/gone/.claude/worktrees/x/app", &[]),
            project("tmp", r"C:\tmp\pr-3\frontend2", &[]),
            project("mockups", "C:/p/app/.for_bepy/mockups", &[]),
            project("dotclaude", r"C:\Users\x\.claude", &[]),
        ]
        .iter()
        .map(parse)
        .collect();
        let v = |id: &str| classify(all.iter().find(|p| p.id == id).unwrap(), &all);
        assert_eq!(v("frontend"), Verdict::Keep);
        assert_eq!(v("frontend-2"), Verdict::FoldInto("frontend".into()));
        assert_eq!(v("wt-root"), Verdict::FoldInto("fibo".into()));
        assert_eq!(v("orphan-wt"), Verdict::Drop);
        assert_eq!(v("tmp"), Verdict::Drop);
        assert_eq!(v("mockups"), Verdict::Drop);
        assert_eq!(v("dotclaude"), Verdict::Keep, "a project rooted at ~/.claude is real");
    }

    #[test]
    fn sweep_folds_unique_commands_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let list = serde_json::json!([
            project("frontend", "C:/p/fibo/frontend", &[("dev", "npm run dev")]),
            project("frontend-2", "C:/p/fibo/.claude/worktrees/wt/frontend", &[("dev", "npm  run dev"), ("dev-2", "npm run e2e")]),
            project("scratch", "C:/tmp/x", &[("py", "python -m http.server")]),
        ]);
        std::fs::write(dir.path().join("projects.json"), list.to_string()).unwrap();
        let ports = Arc::new(PortRegistry::new(dir.path().to_path_buf()));
        let sup = Supervisor::new(dir.path().to_path_buf(), ports);

        sup.sweep_throwaway_projects();

        let projects = sup.list_projects();
        assert_eq!(projects.len(), 1);
        let cmds = &projects[0].commands;
        assert_eq!(cmds.iter().map(|c| c.cmd.as_str()).collect::<Vec<_>>(), ["npm run dev", "npm run e2e"]);
        assert!(!cmds[1].autostart, "a folded command never autostarts");
        assert!(sup.procs.lock().unwrap().contains_key("frontend:dev-2"));
        assert!(!sup.procs.lock().unwrap().keys().any(|k| k.starts_with("scratch:")));
        assert!(dir.path().join("projects.json.bak-sweep").exists());
        let saved = std::fs::read_to_string(dir.path().join("projects.json")).unwrap();
        assert!(!saved.contains("frontend-2") && !saved.contains("C:/tmp/x"));
    }
}
