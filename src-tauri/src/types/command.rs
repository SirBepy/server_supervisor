use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{ProcKind, Project, Role};

/// Composite runtime id for a (project, command) pair. Uses `:` (never emitted
/// by `slug`) so the id stays a single URL path segment for the API.
pub fn unit_id(project_id: &str, command_id: &str) -> String {
    format!("{project_id}:{command_id}")
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
    /// Whether this process's window should be docked into the dashboard
    /// instead of left as a separate top-level window on the dev's desktop.
    /// Per-command, off by default: most supervised processes are headless
    /// dev servers with no window at all, so docking must be something the
    /// dev opts into per command rather than something the supervisor tries
    /// on everything.
    #[serde(default)]
    pub dock_window: bool,
    /// Whether this process tree's audio reaches the dev's speakers. Off by
    /// default: the audio watcher mutes every supervised app unless its
    /// command opted in (see `supervisor::audio`). Toggled live, no restart.
    #[serde(default)]
    pub play_sound: bool,
    /// Keep this process's window in the invisible headless host, where only
    /// agents (screenshot/input API) and the dashboard's preview see it.
    /// The backend docks it on its own; no dashboard needs to be open.
    #[serde(default)]
    pub dock_headless: bool,
    /// Param axes carried from `Command.params`, resolved by `substitute_params`
    /// at spawn time (see `supervisor::param_sub`).
    #[serde(default)]
    pub params: Vec<CommandParam>,
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
            dock_window: command.dock_window,
            play_sound: command.play_sound,
            dock_headless: command.dock_headless,
            params: command.params.clone(),
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
    /// Whether this command's window should be docked into the dashboard (see
    /// `ProcSpec::dock_window`). `#[serde(default)]` so an existing
    /// `projects.json` with no such key on disk still loads, defaulting off.
    #[serde(default)]
    pub dock_window: bool,
    /// Whether this command's audio is audible (see `ProcSpec::play_sound`).
    /// Absent on disk means muted, which is the default for every command.
    #[serde(default)]
    pub play_sound: bool,
    /// Run this command's window headless (see `ProcSpec::dock_headless`).
    #[serde(default)]
    pub dock_headless: bool,
    /// Named axes this command's `cmd` template varies along (see
    /// `supervisor::param_sub`). Empty means `cmd` spawns byte-for-byte as
    /// stored.
    #[serde(default)]
    pub params: Vec<CommandParam>,
}

/// One named axis a command can vary along (Flutter: flavor, device,
/// dart-define file; Node: script, mode; or a free-form one-off). Lives on
/// `Command`, never on `ProcSpec` directly - the resolved cmd string is what
/// `ProcSpec::from_unit` flattens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct CommandParam {
    /// Axis key, e.g. "flavor". Uppercased, this is the `{FLAVOR}` token
    /// `cmd` must contain exactly once.
    pub name: String,
    /// UI label, e.g. "Flavor".
    pub label: String,
    pub values: Vec<ParamValue>,
    /// The `ParamValue.value` id last chosen. `None` (or an id no longer
    /// present) falls back to `values.first()` - same "stale selection never
    /// hard-fails" stance as `Project::active_preset`.
    #[serde(default)]
    pub last_value: Option<String>,
}

/// One concrete choice for a `CommandParam`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct ParamValue {
    /// Stable id for this choice, e.g. "dev". Never re-derived from `label`,
    /// so renaming the label doesn't orphan a stored `last_value`.
    pub value: String,
    /// UI label, e.g. "Dev".
    pub label: String,
    /// Literal text substituted at the `{NAME}` site. May be the empty
    /// string ("no flag").
    pub flag: String,
}
