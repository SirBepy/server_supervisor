use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What kind of process this is. Generic = spawn + tree-kill. Flutter = owns a
/// `flutter run --machine` daemon with `app.restart` reload (wired in Phase 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum ProcKind {
    Generic,
    Flutter,
}

impl Default for ProcKind {
    fn default() -> Self {
        ProcKind::Generic
    }
}

impl ProcKind {
    /// Infer the kind from a command string. Every Flutter launch (`flutter run`,
    /// `flutter run --machine`, `fvm flutter run`, ...) contains the substring
    /// "flutter"; nothing else we run does. This is the single source of truth for
    /// kind inference, so the UI never has to ask.
    pub fn infer(cmd: &str) -> ProcKind {
        if cmd.contains("flutter") {
            ProcKind::Flutter
        } else {
            ProcKind::Generic
        }
    }
}

/// Which side of the stack a command belongs to, for the dashboard's FE/BE
/// badge. Purely descriptive metadata - never read by the supervisor itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub enum Role {
    #[serde(rename = "FE")]
    Frontend,
    #[serde(rename = "BE")]
    Backend,
}

/// Lifecycle state of a supervised process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum ProcStatus {
    Stopped,
    Starting,
    Running,
    Crashed,
}

/// A declared process from the registry config (`procs.json`). The user hand-edits
/// these; the supervisor owns their lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ProcSpec {
    pub id: String,
    pub project: String,
    pub name: String,
    /// Full shell command, run via `cmd /C` (e.g. "npm run dev:up").
    pub cmd: String,
    /// Working directory the command runs in.
    pub cwd: String,
    #[serde(default)]
    pub kind: ProcKind,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub use_dynamic_port: bool,
    /// Manual override for this command's port, entered on the dashboard.
    /// `None` = auto-assign from the owning project's port block (see
    /// `ports::PortRegistry::project_port`). Ignored when `use_dynamic_port`
    /// is false.
    #[serde(default)]
    pub fixed_port: Option<u16>,
    /// Per-command environment overrides, one `KEY=VALUE` per line. Values may
    /// reference existing vars via `${NAME}` / `%NAME%` (e.g.
    /// `PATH=C:\node;%PATH%` to prepend a real node dir past the nvm symlink).
    #[serde(default)]
    pub env: String,
}

/// Dashboard / API view of one supervised process.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ProcInfo {
    pub id: String,
    pub project: String,
    pub name: String,
    pub kind: ProcKind,
    pub status: ProcStatus,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    /// Resident memory of the whole process subtree (the pid plus every
    /// descendant), in bytes. `None` when stopped or not yet sampled. Summed
    /// over descendants because the heavy RAM (e.g. a linker storm under
    /// `cargo`) lives in grandchildren, not the top pid.
    #[serde(default)]
    pub mem_bytes: Option<u64>,
    /// Subtree CPU usage, as a percentage of TOTAL system capacity (comparable
    /// directly to `SystemStats::cpu_pct`, not per-core). `None` when stopped or
    /// not yet sampled (the first reading needs a prior tick to diff against).
    #[serde(default)]
    pub cpu_pct: Option<f32>,
    /// Unix epoch millis when the current run started, for the dashboard's
    /// "started N ago" uptime line. `None` when stopped.
    #[serde(default)]
    pub started_at: Option<u64>,
    /// True when this run is bound to a fallback dynamic port instead of its
    /// usual stable project-block/override port, because the usual port was
    /// occupied at spawn time (e.g. a second instance of the same project, or
    /// something unrelated squatting it). Always false when stopped.
    #[serde(default)]
    pub fallback_port: bool,
}

/// Composite runtime id for a (project, command) pair. Uses `:` (never emitted
/// by `slug`) so the id stays a single URL path segment for the API.
pub fn unit_id(project_id: &str, command_id: &str) -> String {
    format!("{project_id}:{command_id}")
}

impl ProcSpec {
    /// Flatten a project + command into a runnable spec.
    pub fn from_unit(project: &Project, command: &Command) -> ProcSpec {
        ProcSpec {
            id: unit_id(&project.id, &command.id),
            project: project.name.clone(),
            name: command.name.clone(),
            cmd: command.cmd.clone(),
            cwd: project.root.clone(),
            kind: command.kind.clone(),
            autostart: command.autostart,
            use_dynamic_port: command.use_dynamic_port,
            fixed_port: command.fixed_port,
            env: command.env.clone(),
        }
    }
}

/// A runnable command within a project.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct Command {
    pub id: String,
    pub name: String,
    /// Full shell command, run via `cmd /C`.
    pub cmd: String,
    #[serde(default)]
    pub kind: ProcKind,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub use_dynamic_port: bool,
    /// Manual port override, editable on the dashboard's add/edit-command
    /// modal. `None` (the field left empty) means auto-assign from the
    /// project's port block; ignored when `use_dynamic_port` is false.
    #[serde(default)]
    pub fixed_port: Option<u16>,
    /// Per-command environment overrides, one `KEY=VALUE` per line. Values may
    /// reference existing vars via `${NAME}` / `%NAME%`.
    #[serde(default)]
    pub env: String,
    /// Optional FE/BE badge for the dashboard row. `None` for a command with no
    /// declared side (and for every command saved before this field existed -
    /// `#[serde(default)]` keeps those `projects.json` entries loading).
    #[serde(default)]
    pub role: Option<Role>,
}

/// A project: a named root folder with a set of runnable commands. This is the
/// source-of-truth config the user edits (persisted to `projects.json`).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// Absolute path the commands run in.
    pub root: String,
    #[serde(default)]
    pub commands: Vec<Command>,
}

/// A command candidate surfaced by auto-detection, before the user accepts it.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DetectedCommand {
    /// Where it was found: "package.json", "launch.json", or "readme".
    pub source: String,
    pub name: String,
    pub cmd: String,
    pub kind: ProcKind,
}

/// One captured line of process output.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct LogLine {
    /// Unix epoch millis when the line was captured.
    pub ts: u64,
    /// "stdout" or "stderr".
    pub stream: String,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_kind_flags_flutter_commands() {
        assert_eq!(ProcKind::infer("flutter run --machine"), ProcKind::Flutter);
        assert_eq!(ProcKind::infer("fvm flutter run"), ProcKind::Flutter);
        assert_eq!(ProcKind::infer("npm run dev"), ProcKind::Generic);
        assert_eq!(ProcKind::infer("node server.js"), ProcKind::Generic);
        assert_eq!(ProcKind::infer("cargo run"), ProcKind::Generic);
    }
}
