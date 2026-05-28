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

/// One captured line of process output.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct LogLine {
    /// Unix epoch millis when the line was captured.
    pub ts: u64,
    /// "stdout" or "stderr".
    pub stream: String,
    pub text: String,
}
