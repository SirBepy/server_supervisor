//! Config CRUD for the Supervisor: projects and commands. A second
//! `impl Supervisor` block, split from `registry` (which keeps the struct +
//! runtime control) so process lifecycle and config mutation each read as one
//! focused file. Reaches the supervisor's `pub(super)` fields directly.
//!
//! This file holds `ensure_and_run` (the composite one-call `/run` API entry
//! point, which registers a project AND a command then starts it) plus its
//! `prune_failed_siblings` helper and the name-derivation helpers both lean
//! on. Project CRUD (list/add/rename/remove a project) and command CRUD
//! (add/edit/remove a command) are each a further `impl Supervisor` block in
//! a sibling module - `project` and `command` - since `ensure_and_run`
//! composes both.

use super::config;
use super::registry::Supervisor;
use crate::types::{unit_id, ProcInfo, ProcKind};

mod command;
mod project;

impl Supervisor {
    /// Register a project (by folder) + a command (by cmd string) if not already
    /// present - both are idempotent - then start it and return its ProcInfo.
    /// The composite used by the `POST /run` API for one-call server launch.
    pub fn ensure_and_run(
        &self,
        root: &str,
        cmd: &str,
        name: Option<String>,
        kind: Option<ProcKind>,
        use_dynamic_port: bool,
        fixed_port: Option<u16>,
        env: String,
    ) -> Result<ProcInfo, String> {
        let project_name = smart_project_name(root);
        let detected = super::transient::detect(std::path::Path::new(root));
        let project = self.add_project_inner(
            project_name,
            root.to_string(),
            detected.transient,
            detected.label,
        )?;
        let command_name = name.unwrap_or_else(|| derive_name(cmd));
        let command = self.add_command(
            &project.id,
            command_name,
            cmd.to_string(),
            kind,
            false,
            use_dynamic_port,
            fixed_port,
            env,
            None,
        )?;
        // The incoming command is now registered, so pruning its failed siblings
        // can never empty the project. Clears the dead-on-arrival variant pile.
        self.prune_failed_siblings(&project.id, &command.cmd);
        let id = unit_id(&project.id, &command.id);
        self.start(&id)?;
        self.list()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("started but not found in list: {id}"))
    }

    /// After a successful `/run`, drop this project's *other* commands that are
    /// dead-on-arrival crashes - failed launch variants the AI left behind (the
    /// classic `flutter run` x3 pile). `keep_cmd` is the just-registered command
    /// (already normalized); never prune it, so a same-cmd retry keeps its logs.
    /// A real server that ran a long time and then crashed is NOT dead-on-arrival
    /// (see `ManagedProc::is_dead_on_arrival`), so it survives.
    ///
    /// Safe against emptying the project because `add_command` is idempotent:
    /// `keep_cmd` is always present in `project.commands`, so the
    /// `c.cmd == keep_cmd` skip below guarantees at least that command survives
    /// the prune (the auto-delete-empty-project path in `remove_command` can
    /// never fire mid-run).
    fn prune_failed_siblings(&self, project_id: &str, keep_cmd: &str) {
        let victims: Vec<String> = {
            // Lock order: projects THEN procs. No other Supervisor path nests both
            // simultaneously (add_command/remove_command release `projects` before
            // locking `procs`), so this is the sole nesting site. Keep this order
            // to avoid a deadlock regression.
            let projects = self.projects.lock().unwrap();
            let mut map = self.procs.lock().unwrap();
            let Some(project) = projects.iter().find(|p| p.id == project_id) else {
                return;
            };
            let mut v = Vec::new();
            for c in &project.commands {
                if c.cmd == keep_cmd {
                    continue;
                }
                if let Some(proc) = map.get_mut(&unit_id(project_id, &c.id)) {
                    proc.refresh();
                    if proc.is_dead_on_arrival() {
                        v.push(c.id.clone());
                    }
                }
            }
            v
        };
        for cid in victims {
            if let Err(e) = self.remove_command(project_id, &cid) {
                log::warn!("prune_failed_siblings: could not remove {project_id}:{cid}: {e}");
            }
        }
    }
}

/// Default display name for a project folder. A folder literally named "app" or
/// "src" is a useless label, so prefix it with the parent folder name:
/// `.../myproject/app` -> "myproject-app". Any other folder uses its own name.
fn smart_project_name(root: &str) -> String {
    let path = std::path::Path::new(root);
    let folder = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(root)
        .to_string();
    if folder.eq_ignore_ascii_case("app") || folder.eq_ignore_ascii_case("src") {
        if let Some(parent) = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
        {
            return format!("{parent}-{folder}");
        }
    }
    folder
}

pub(super) fn unique_id(base: &str, taken: &dyn Fn(&str) -> bool) -> String {
    let b = config::slug(base);
    if !taken(&b) {
        return b;
    }
    let mut n = 2;
    loop {
        let cand = format!("{b}-{n}");
        if !taken(&cand) {
            return cand;
        }
        n += 1;
    }
}

/// Short display name for a command, used whenever a name is omitted (the UI's
/// add flow and the `POST /run` API). Never returns the whole command line - a
/// long launch (a Flutter run with a wall of `--dart-define`s) collapses to a
/// stable short label instead of becoming an unreadable title.
fn derive_name(cmd: &str) -> String {
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    // Every Flutter launch (`flutter run`, `fvm flutter run --machine ...`)
    // contains "flutter"; collapse the dart-define soup to one short label.
    if toks.iter().any(|t| *t == "flutter") {
        return "flutter run".to_string();
    }
    let prog_short = |p: &str| {
        std::path::Path::new(p)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(p)
            .to_string()
    };
    match toks.as_slice() {
        [runner, "run", x, ..]
            if matches!(prog_short(runner).as_str(), "npm" | "pnpm" | "yarn" | "bun") =>
        {
            x.to_string()
        }
        [runner, x, ..]
            if matches!(prog_short(runner).as_str(), "yarn" | "pnpm" | "bun" | "npx") =>
        {
            x.to_string()
        }
        [cargo, x, ..] if prog_short(cargo) == "cargo" => format!("cargo {x}"),
        // Fallback: basename without extension, never the full path or command line.
        [prog, ..] => prog_short(prog),
        [] => cmd.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{derive_name, smart_project_name};

    #[test]
    fn smart_project_name_prefixes_app_and_src_folders() {
        assert_eq!(smart_project_name(r"C:\Projects\myproject\app"), "myproject-app");
        assert_eq!(smart_project_name(r"C:\Projects\myproject\src"), "myproject-src");
        // Case-insensitive on the folder, preserves the folder's own casing.
        assert_eq!(smart_project_name(r"C:\Projects\myproject\App"), "myproject-App");
        // A normal folder keeps its own name.
        assert_eq!(smart_project_name(r"C:\Projects\myproject"), "myproject");
        // Unix-style separators work too.
        assert_eq!(smart_project_name("/home/joe/myproject/src"), "myproject-src");
    }

    #[test]
    fn derive_name_handles_runners_and_fallback() {
        assert_eq!(derive_name("npm run dev"), "dev");
        assert_eq!(derive_name("pnpm run build"), "build");
        assert_eq!(derive_name("yarn run start"), "start");
        assert_eq!(derive_name("yarn dev"), "dev");
        assert_eq!(derive_name("npx vite"), "vite");
        assert_eq!(derive_name("cargo run"), "cargo run");
        assert_eq!(derive_name("cargo tauri dev"), "cargo tauri");
        // Long launches collapse to a short label instead of the whole line.
        assert_eq!(derive_name("node server.js"), "node");
        assert_eq!(derive_name("  flutter run  "), "flutter run");
        assert_eq!(
            derive_name("fvm flutter run -d web-server --web-port 5000 --dart-define=ENV=local"),
            "flutter run"
        );
        // Full Windows paths strip to basename without extension.
        assert_eq!(derive_name(r"C:\nvm4w\nodejs\npm.cmd run dev"), "dev");
        assert_eq!(
            derive_name(r"C:\Users\tecno\AppData\Local\nvm\v22.13.0\npm.cmd run dev"),
            "dev"
        );
        assert_eq!(derive_name(r"C:/tmp/zng-api-devup.cmd"), "zng-api-devup");
        assert_eq!(derive_name(r"C:\nvm4w\nodejs\node.exe server.js"), "node");
    }
}
