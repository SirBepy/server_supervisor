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
