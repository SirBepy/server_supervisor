use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What kind of process this is. Generic = spawn + tree-kill. Flutter = owns a
/// `flutter run --machine` daemon with `app.restart` reload (wired in Phase 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum ProcKind {
    Generic,
    Flutter,
    /// Never auto-inferred (see `infer` below) - only set via an explicit
    /// `/run` payload `kind`. Its entry is deleted on exit instead of being
    /// retained as `stopped` (see `registry::reap_tick`, `Supervisor::stop`).
    Ephemeral,
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

/// One resolved env var actually applied to a spawned child (the parsed
/// `KEY=VALUE`/`Command::env` pair, not the raw unexpanded `spec.env` line).
/// `secret` is true when `key` case-insensitively matches
/// `TOKEN|SECRET|KEY|PASSWORD|PASSWD|CREDENTIAL`, so the frontend renders the
/// value masked behind a click-to-reveal instead of plainly; URL-ish vars
/// (what a dev actually needs, to see which backend a running process is
/// pointed at) never match and stay visible. The classification lives here,
/// not duplicated in TS, so there is one source of truth for "secret-looking".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
    pub secret: bool,
}

impl EnvVar {
    pub fn new(key: String, value: String) -> Self {
        let secret = is_secret_key(&key);
        Self { key, value, secret }
    }
}

/// Case-insensitive substring match against the secret-looking key patterns.
/// A false positive (e.g. a key that happens to contain "KEY" but isn't
/// actually sensitive) just costs an extra click to reveal - acceptable; a
/// false negative would leak a real secret in the clear, which is not.
fn is_secret_key(key: &str) -> bool {
    const PATTERNS: [&str; 6] = ["TOKEN", "SECRET", "KEY", "PASSWORD", "PASSWD", "CREDENTIAL"];
    let upper = key.to_uppercase();
    PATTERNS.iter().any(|p| upper.contains(p))
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
    /// The resolved per-command env overrides (parsed `spec.env` plus the
    /// injected `PORT`) actually applied to the child at spawn time - scoped
    /// to those overrides only, NOT the full inherited Windows environment
    /// (hundreds of vars, pure noise), and NOT including PATH (routinely
    /// thousands of characters). `Some` (even `Some(vec![])`, meaning zero
    /// overrides were configured) only while a real spawn's values are known
    /// for this app instance; `None` when stopped, or when re-adopted after a
    /// restart (see `env_unknown`).
    #[serde(default)]
    pub resolved_env: Option<Vec<EnvVar>>,
    /// True only for a re-adopted process: it is Running but its spawn-time
    /// env was never observed by this app instance (no live Child, no stdio -
    /// the prior instance had it, and it died with that instance). The
    /// frontend must render an explicit "unknown, re-adopted after restart"
    /// state rather than an empty block or a stale value from a previous run.
    /// Always false once stopped, or once a real restart supersedes adoption.
    #[serde(default)]
    pub env_unknown: bool,
    /// Param axes carried from `spec.params` (see `supervisor::param_sub`),
    /// cloned for free alongside `resolved_env` - both cost only a clone, not
    /// a syscall, unlike `WindowInfo`'s deliberate exclusion from this struct.
    /// Includes `ParamValue.flag`: a params-without-flag view type would just
    /// restate this one.
    #[serde(default)]
    pub params: Vec<CommandParam>,
    /// `spec.cmd` with every `{NAME}` placeholder already substituted, so the
    /// dashboard row shows the resolved line instead of a raw template.
    /// `None` when `params` is empty (nothing to resolve).
    #[serde(default)]
    pub resolved_cmd: Option<String>,
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

/// A named upstream target for a project's reverse-proxy hub (see
/// `supervisor::proxy_hub`). The dashboard/AI-agent picks one preset as
/// active; the hub forwards every request to whichever one is active, without
/// ever rebinding its listener.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct UpstreamPreset {
    pub id: String,
    pub name: String,
    /// e.g. `http://localhost:9000` or `https://staging.example.com`. No
    /// trailing slash required; the hub strips one if present.
    pub base_url: String,
    /// The dev sets this explicitly for a prod-like target, so the dashboard
    /// can warn before a swap - purely advisory, never enforced backend-side.
    pub danger: bool,
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
    /// Reverse-proxy hub upstream presets (see `supervisor::proxy_hub`).
    /// Empty = no hub listener for this project.
    #[serde(default)]
    pub presets: Vec<UpstreamPreset>,
    /// The currently active preset's `id`. `None`, or an id no longer present
    /// in `presets`, falls back to the first preset in the list.
    #[serde(default)]
    pub active_preset: Option<String>,
    /// True for a project registered from a throwaway root (see
    /// `supervisor::transient`). Never written to `projects.json`, only to the
    /// side `transient_projects.json`, so a still-running one survives a
    /// restart without polluting the permanent project list.
    #[serde(default)]
    pub transient: bool,
    /// Branch name, else the worktree/scratch dir name. Computed once at
    /// registration (see `supervisor::transient`), never re-derived in TS.
    #[serde(default)]
    pub transient_label: Option<String>,
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

/// Screen-coordinate rectangle for docking a supervised process's window
/// into a dashboard pane. A serde/TS-friendly mirror of
/// `supervisor::window::Rect` (that type stays FFI-shaped, with no serde or
/// TS derives, since it is passed by pointer straight into Win32 calls) so
/// IPC and the frontend never see FFI plumbing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
pub struct DockRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// How a dock attempt actually landed. Mirrors
/// `supervisor::window::DockOutcome` variant-for-variant. Kept a real enum
/// (not a bool) all the way to the frontend: an embedded window has no title
/// bar of its own to fight with, a soft-docked one still does, and the UI
/// must render the two differently rather than collapsing them into one
/// generic "docked" pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DockOutcome {
    Embedded,
    SoftDocked,
    /// Embedded into the invisible headless host instead of a dashboard pane.
    Headless,
}

/// Dock status of one supervised process, as read by the UI/API.
/// `WindowLost` is distinct from both `NotDocked` and `Docked`: it means the
/// process is alive but the window this module previously found (or is
/// looking for) can't be located - the proven case is a force-killed host
/// destroying an embedded guest's window while the guest process itself
/// survives untouched (see `supervisor::window` module docs). The UI renders
/// this as recoverable (offer a restart), never as plain "not docked".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DockState {
    NotDocked,
    Docked { mode: DockOutcome },
    WindowLost,
    /// The app rejected `SetParent` (soft-docked back out) the last time
    /// embedding was tried, so it is still an ordinary window on the dev's
    /// desktop rather than headless. Distinct from `NotDocked` so the UI
    /// never shows a refused headless toggle as if it had worked.
    Refused,
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

    #[test]
    fn env_var_masks_secret_looking_keys() {
        assert!(EnvVar::new("API_TOKEN".to_string(), "x".to_string()).secret);
        assert!(EnvVar::new("DB_PASSWORD".to_string(), "x".to_string()).secret);
        assert!(EnvVar::new("SECRET_KEY".to_string(), "x".to_string()).secret);
        assert!(EnvVar::new("PASSWD".to_string(), "x".to_string()).secret);
        assert!(EnvVar::new("AWS_CREDENTIAL_PROFILE".to_string(), "x".to_string()).secret);
        // Case-insensitive.
        assert!(EnvVar::new("password".to_string(), "x".to_string()).secret);
        assert!(EnvVar::new("apiKey".to_string(), "x".to_string()).secret);
    }

    #[test]
    fn env_var_keeps_url_and_plain_vars_visible() {
        assert!(!EnvVar::new("BACKEND_URL".to_string(), "http://localhost:9000".to_string()).secret);
        assert!(!EnvVar::new("API_BASE_URL".to_string(), "https://api.example.com".to_string()).secret);
        assert!(!EnvVar::new("PORT".to_string(), "3000".to_string()).secret);
        assert!(!EnvVar::new("NODE_ENV".to_string(), "development".to_string()).secret);
    }

    fn command_with_dock(dock_window: bool) -> Command {
        Command {
            id: "c".to_string(),
            name: "c".to_string(),
            cmd: "cmd /C exit 0".to_string(),
            kind: ProcKind::Generic,
            autostart: false,
            use_dynamic_port: false,
            fixed_port: None,
            env: String::new(),
            role: None,
            dock_window,
            play_sound: false,
            dock_headless: false,
            params: Vec::new(),
        }
    }

    #[test]
    fn command_dock_window_round_trips_through_json() {
        let cmd = command_with_dock(true);
        let json = serde_json::to_string(&cmd).unwrap();
        let back: Command = serde_json::from_str(&json).unwrap();
        assert!(back.dock_window, "dock_window: true must survive a serialize/deserialize round trip");
    }

    #[test]
    fn command_dock_window_defaults_false_when_key_absent() {
        // No `dock_window` key at all - simulates an existing projects.json
        // written before this field existed.
        let json = r#"{
            "id": "c", "name": "c", "cmd": "cmd /C exit 0", "kind": "generic",
            "autostart": false, "use_dynamic_port": false, "fixed_port": null,
            "env": "", "role": null
        }"#;
        let cmd: Command = serde_json::from_str(json).unwrap();
        assert!(!cmd.dock_window, "an absent dock_window key must default to false");
        assert!(!cmd.play_sound, "an absent play_sound key must default to muted");
        assert!(cmd.params.is_empty(), "an absent params key must default to an empty vec");
    }

    #[test]
    fn command_param_round_trips_through_json() {
        let param = CommandParam {
            name: "flavor".to_string(),
            label: "Flavor".to_string(),
            values: vec![ParamValue {
                value: "dev".to_string(),
                label: "Dev".to_string(),
                flag: "--flavor dev".to_string(),
            }],
            last_value: Some("dev".to_string()),
        };
        let json = serde_json::to_string(&param).unwrap();
        let back: CommandParam = serde_json::from_str(&json).unwrap();
        assert_eq!(back, param, "CommandParam must survive a serialize/deserialize round trip");
    }

    #[test]
    fn proc_spec_from_unit_carries_command_dock_window() {
        let project = Project {
            id: "p".to_string(),
            name: "p".to_string(),
            root: ".".to_string(),
            commands: Vec::new(),
            presets: Vec::new(),
            active_preset: None,
            transient: false,
            transient_label: None,
        };
        let spec = ProcSpec::from_unit(&project, &command_with_dock(true));
        assert!(spec.dock_window, "from_unit must carry the command's dock_window, not hardcode false");
        let mut loud = command_with_dock(false);
        loud.play_sound = true;
        assert!(ProcSpec::from_unit(&project, &loud).play_sound, "from_unit must carry play_sound");
    }
}
