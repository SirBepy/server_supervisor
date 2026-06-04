//! Config CRUD for the Supervisor: projects and commands. A second
//! `impl Supervisor` block, split from `registry` (which keeps the struct +
//! runtime control) so process lifecycle and config mutation each read as one
//! focused file. Reaches the supervisor's `pub(super)` fields directly.

use super::config;
use super::proc::ManagedProc;
use super::registry::Supervisor;
use crate::types::{unit_id, Command, ProcInfo, ProcKind, ProcSpec, Project};

impl Supervisor {
    // ----- config CRUD (mutates projects + runtime map, persists) -----

    pub fn list_projects(&self) -> Vec<Project> {
        self.projects.lock().unwrap().clone()
    }

    pub fn add_project(&self, name: String, root: String) -> Result<Project, String> {
        let name = name.trim().to_string();
        let root = root.trim().to_string();
        if name.is_empty() || root.is_empty() {
            return Err("project name and root are required".to_string());
        }
        let mut projects = self.projects.lock().unwrap();
        // Idempotent on the folder: same canonical path -> reuse the existing
        // project unchanged (keep its name; ignore the re-entered one). No dup.
        if let Some(existing) = projects.iter().find(|p| same_path(&p.root, &root)) {
            return Ok(existing.clone());
        }
        let id = unique_id(&name, &|cand| projects.iter().any(|p| p.id == cand));
        let project = Project {
            id,
            name,
            root,
            commands: Vec::new(),
        };
        projects.push(project.clone());
        config::save(&self.data_dir, &projects);
        Ok(project)
    }

    pub fn remove_project(&self, project_id: &str) -> Result<(), String> {
        let mut projects = self.projects.lock().unwrap();
        let idx = projects
            .iter()
            .position(|p| p.id == project_id)
            .ok_or_else(|| format!("unknown project: {project_id}"))?;
        let removed = projects.remove(idx);
        config::save(&self.data_dir, &projects);
        drop(projects);

        let mut map = self.procs.lock().unwrap();
        for c in &removed.commands {
            if let Some(mut proc) = map.remove(&unit_id(&removed.id, &c.id)) {
                proc.stop();
            }
        }
        Ok(())
    }

    /// Add a command. `kind` is normally inferred from the command string
    /// (`None`); an explicit `Some(kind)` overrides inference (used by the `/run`
    /// API when a caller knows better).
    pub fn add_command(
        &self,
        project_id: &str,
        name: String,
        cmd: String,
        kind: Option<ProcKind>,
        autostart: bool,
        use_dynamic_port: bool,
        env: String,
    ) -> Result<Command, String> {
        let name = name.trim().to_string();
        let cmd = normalize_cmd(&cmd);
        if name.is_empty() || cmd.is_empty() {
            return Err("command name and cmd are required".to_string());
        }
        let kind = kind.unwrap_or_else(|| ProcKind::infer(&cmd));
        let mut projects = self.projects.lock().unwrap();
        let project = projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .ok_or_else(|| format!("unknown project: {project_id}"))?;
        // Idempotent on the exact cmd string within this project: if a command
        // with the same cmd already exists, return it (the runtime procs map
        // already holds its entry, so don't re-insert).
        if let Some(existing) = project.commands.iter().find(|c| c.cmd == cmd) {
            return Ok(existing.clone());
        }
        let cid = unique_id(&name, &|cand| project.commands.iter().any(|c| c.id == cand));
        let command = Command {
            id: cid,
            name,
            cmd,
            kind,
            autostart,
            use_dynamic_port,
            env,
        };
        project.commands.push(command.clone());
        let project_snapshot = project.clone();
        config::save(&self.data_dir, &projects);
        drop(projects);

        let mut map = self.procs.lock().unwrap();
        let spec = ProcSpec::from_unit(&project_snapshot, &command);
        map.entry(spec.id.clone())
            .or_insert_with(|| ManagedProc::new(spec));
        Ok(command)
    }

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
        env: String,
    ) -> Result<ProcInfo, String> {
        let project_name = std::path::Path::new(root)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(root)
            .to_string();
        let project = self.add_project(project_name, root.to_string())?;
        let command_name = name.unwrap_or_else(|| derive_name(cmd));
        let command = self.add_command(
            &project.id,
            command_name,
            cmd.to_string(),
            kind,
            false,
            use_dynamic_port,
            env,
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

    /// Edit an existing command in place. The command `id` is a stable handle
    /// (it keys the runtime procs map, the captured logs, and the API path), so
    /// it never changes here - only the mutable fields do. The runtime
    /// `ManagedProc` is mutated in place (preserving its log buffer and any live
    /// child handle). If the process is running and the edit changes a field the
    /// spawn depends on (cmd, cwd, kind, dynamic-port), it is restarted so the
    /// live process reflects the edit.
    pub fn update_command(
        &self,
        project_id: &str,
        command_id: &str,
        name: String,
        cmd: String,
        autostart: bool,
        use_dynamic_port: bool,
        env: String,
    ) -> Result<Command, String> {
        let name = name.trim().to_string();
        let cmd = cmd.trim().to_string();
        if name.is_empty() || cmd.is_empty() {
            return Err("command name and cmd are required".to_string());
        }
        // Kind is always inferred from the command string (no manual picker).
        let kind = ProcKind::infer(&cmd);
        // A running command is locked: editing it would silently relaunch the
        // live process. Require the caller to stop it first. Refresh so a child
        // that already exited on its own does not count as running.
        {
            let mut map = self.procs.lock().unwrap();
            if let Some(proc) = map.get_mut(&unit_id(project_id, command_id)) {
                proc.refresh();
                if proc.pid.is_some() {
                    return Err("stop the command before editing it".to_string());
                }
            }
        }
        let (updated, project_snapshot) = {
            let mut projects = self.projects.lock().unwrap();
            let project = projects
                .iter_mut()
                .find(|p| p.id == project_id)
                .ok_or_else(|| format!("unknown project: {project_id}"))?;
            let command = project
                .commands
                .iter_mut()
                .find(|c| c.id == command_id)
                .ok_or_else(|| format!("unknown command: {command_id}"))?;
            command.name = name;
            command.cmd = cmd;
            command.kind = kind;
            command.autostart = autostart;
            command.use_dynamic_port = use_dynamic_port;
            command.env = env;
            let updated = command.clone();
            let snapshot = project.clone();
            config::save(&self.data_dir, &projects);
            (updated, snapshot)
        };

        let new_spec = ProcSpec::from_unit(&project_snapshot, &updated);
        let id = new_spec.id.clone();
        let restart_needed = {
            let mut map = self.procs.lock().unwrap();
            match map.get_mut(&id) {
                Some(proc) => {
                    let affects_running = proc.spec.cmd != new_spec.cmd
                        || proc.spec.cwd != new_spec.cwd
                        || proc.spec.kind != new_spec.kind
                        || proc.spec.use_dynamic_port != new_spec.use_dynamic_port
                        || proc.spec.env != new_spec.env;
                    let running = proc.pid.is_some();
                    proc.spec = new_spec;
                    running && affects_running
                }
                None => {
                    // Defensive: every command should already have a runtime entry,
                    // but if not, create one so the edit is at least startable.
                    map.insert(id.clone(), ManagedProc::new(new_spec));
                    false
                }
            }
        };
        if restart_needed {
            self.restart(&id)?;
        }
        Ok(updated)
    }

    pub fn remove_command(&self, project_id: &str, command_id: &str) -> Result<(), String> {
        // Same lock as edit: a running command must be stopped before it can be
        // removed, so deletion never races a live child.
        {
            let mut map = self.procs.lock().unwrap();
            if let Some(proc) = map.get_mut(&unit_id(project_id, command_id)) {
                proc.refresh();
                if proc.pid.is_some() {
                    return Err("stop the command before removing it".to_string());
                }
            }
        }
        let mut projects = self.projects.lock().unwrap();
        let project = projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .ok_or_else(|| format!("unknown project: {project_id}"))?;
        let before = project.commands.len();
        project.commands.retain(|c| c.id != command_id);
        if project.commands.len() == before {
            return Err(format!("unknown command: {command_id}"));
        }
        // Auto-remove the project once its last command is gone (there is no
        // manual project delete; an empty project cleans itself up).
        if project.commands.is_empty() {
            projects.retain(|p| p.id != project_id);
        }
        config::save(&self.data_dir, &projects);
        drop(projects);

        let mut map = self.procs.lock().unwrap();
        if let Some(mut proc) = map.remove(&unit_id(project_id, command_id)) {
            proc.stop();
        }
        Ok(())
    }
}

/// True if two folder paths refer to the same location. Canonicalize both and
/// compare the resulting `PathBuf`s (handles drive-letter case, `/` vs `\`,
/// trailing separators, and `.`/`..` on Windows). If canonicalize fails for
/// either path (e.g. it no longer exists), fall back to a normalized string
/// compare: lowercase + strip trailing `\` and `/`.
fn same_path(a: &str, b: &str) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => norm_path(a) == norm_path(b),
    }
}

fn norm_path(p: &str) -> String {
    p.trim_end_matches(['\\', '/']).to_lowercase()
}

/// Collapse a command string to its canonical dedup form: trim ends and reduce
/// every run of internal whitespace to a single space. Keeps case (flags are
/// case-sensitive). `flutter  run` and ` flutter run ` both become `flutter run`,
/// so trivial whitespace variants reuse one command entry instead of forking.
fn normalize_cmd(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn unique_id(base: &str, taken: &dyn Fn(&str) -> bool) -> String {
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
    use super::{derive_name, normalize_cmd};

    #[test]
    fn normalize_cmd_collapses_and_trims_whitespace() {
        assert_eq!(normalize_cmd("flutter  run"), "flutter run");
        assert_eq!(normalize_cmd("  flutter run  "), "flutter run");
        assert_eq!(normalize_cmd("npm\trun   dev"), "npm run dev");
        assert_eq!(normalize_cmd("flutter run"), "flutter run");
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
