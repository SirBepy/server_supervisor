use super::proxy;
use crate::types::{EnvVar, LogLine, ProcInfo, ProcKind, ProcSpec, ProcStatus};
use std::collections::VecDeque;
use std::process::{Child, ChildStdin};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

mod stop;
pub use stop::StopHandle;

// Process spawn/refresh (the OS-facing half: launching the child, detecting
// its exit) vs the Flutter-daemon reload machinery (writing `app.restart`
// over stdin to an already-running `flutter run --machine` process) are two
// distinct concerns that both got long; each is its own sibling module.
mod reload;
mod spawn;

/// Max log lines retained per process (ring buffer).
pub(super) const LOG_CAP: usize = 2000;

/// A crash within this window of start counts as dead-on-arrival (never came up).
const DOA_WINDOW_MS: u64 = 8_000;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One supervised process: its spec, current child handle, and a bounded log buffer.
pub struct ManagedProc {
    pub spec: ProcSpec,
    pub status: ProcStatus,
    pub pid: Option<u32>,
    pub started_at: Option<u64>,
    /// When the process last transitioned Running -> Crashed (unix ms). Paired
    /// with `started_at` to measure crash uptime: a tiny uptime means the launch
    /// never really came up (dead-on-arrival), e.g. a port-conflict variant.
    pub crashed_at: Option<u64>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    logs: Arc<Mutex<VecDeque<LogLine>>>,
    /// Flutter daemon appId, captured from the `app.started` stdout event.
    app_id: Arc<Mutex<Option<String>>>,
    /// Dynamic port handed out by the registry for the current run, if any.
    /// The Supervisor reads this on stop to release it back to the registry.
    acquired_port: Option<u16>,
    /// True when this proc was re-adopted from a prior app instance: it has a
    /// live PID but no Child handle and no stdio pipes (logs are frozen until
    /// the user restarts it). `refresh` polls the OS for its liveness instead
    /// of `try_wait`.
    adopted: bool,
    /// Live-reload reverse proxy in front of a flutter web-server run, if any.
    /// Dropping it triggers a graceful shutdown + thread join.
    proxy: Option<proxy::ProxyTask>,
    /// Broadcast sender the stdout reader fires on a finished (re)start; the
    /// proxy's SSE endpoint forwards it to open browser tabs. Some only while a
    /// proxy is live.
    reload_tx: Option<tokio::sync::broadcast::Sender<()>>,
    /// The internal ephemeral port flutter binds when proxied (the proxy fronts
    /// it on the public port). The registry releases this on stop.
    internal_port: Option<u16>,
    /// Subtree resident bytes, cached by the background sampler. Read by `info()`;
    /// never computed on the UI poll path. `None` until the first sample after a
    /// start, and cleared when the proc stops/crashes.
    sampled_mem: Option<u64>,
    /// Subtree CPU usage (% of total system capacity), cached by the background
    /// sampler alongside `sampled_mem`. `None` until the first sample lands
    /// (which needs a prior tick's reading to diff against - see `sampler.rs`),
    /// and cleared when the proc stops/crashes.
    sampled_cpu_pct: Option<f32>,
    /// OS-detected listening port, cached by the background sampler (same value
    /// the old inline `fill_ports` computed). `info()` prefers this over the
    /// forced `acquired_port` so the dashboard shows the port actually bound.
    sampled_port: Option<u16>,
    /// True when the current run is bound to a fallback dynamic port instead
    /// of its usual stable project-block/override port. Set by the supervisor
    /// right after a successful spawn (`set_fallback_port`); read by `info()`.
    fallback_port: bool,
    /// The env overrides actually applied to the current run's child, captured
    /// in `start()` from `spawn_env::parse_env(&spec.env)` plus the injected
    /// `PORT` - not the full inherited environment. `Some` only while a real
    /// spawn's values are known for this app instance; cleared on `stop()`, on
    /// a self-detected exit (`refresh()`), and never set for an adopted proc
    /// (its spawn-time env belonged to a prior app instance and is unknown).
    resolved_env: Option<Vec<EnvVar>>,
}

impl ManagedProc {
    pub fn new(spec: ProcSpec) -> Self {
        Self {
            spec,
            status: ProcStatus::Stopped,
            pid: None,
            started_at: None,
            crashed_at: None,
            child: None,
            stdin: None,
            logs: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAP))),
            app_id: Arc::new(Mutex::new(None)),
            acquired_port: None,
            adopted: false,
            proxy: None,
            reload_tx: None,
            internal_port: None,
            sampled_mem: None,
            sampled_cpu_pct: None,
            sampled_port: None,
            fallback_port: false,
            resolved_env: None,
        }
    }

    /// Store the latest background-sampler reading (subtree RAM + CPU + detected
    /// port). Called only from `Supervisor::sample_tick`, never on the UI poll path.
    pub fn set_sample(&mut self, mem: Option<u64>, cpu_pct: Option<f32>, port: Option<u16>) {
        self.sampled_mem = mem;
        self.sampled_cpu_pct = cpu_pct;
        self.sampled_port = port;
    }

    /// True when this proc is a flutter web-server launch with a `{PORT}`
    /// placeholder we can redirect: only these can sit behind a live-reload
    /// proxy (we move flutter onto an internal port and front it on the public
    /// one). Anything else runs unproxied.
    pub fn wants_proxy(&self) -> bool {
        self.spec.kind == ProcKind::Flutter
            && self.spec.cmd.contains("web-server")
            && self.spec.cmd.contains("{PORT}")
    }

    /// The internal port flutter binds behind the proxy, if proxied. The registry
    /// releases it on stop (it is acquired separately from the public port).
    pub fn internal_port(&self) -> Option<u16> {
        self.internal_port
    }

    /// The dynamic port currently held for this run, if any.
    pub fn acquired_port(&self) -> Option<u16> {
        self.acquired_port
    }

    /// Flag whether the current run landed on a fallback (non-usual) dynamic
    /// port rather than its stable project-block/override port. Called by the
    /// supervisor right after a successful spawn.
    pub fn set_fallback_port(&mut self, v: bool) {
        self.fallback_port = v;
    }

    /// Dead-on-arrival: crashed within `DOA_WINDOW_MS` of starting, i.e. the
    /// launch never really came up. Used to auto-prune failed `/run` attempts
    /// while sparing a real server that ran a long time and then crashed.
    pub fn is_dead_on_arrival(&self) -> bool {
        matches!(self.status, ProcStatus::Crashed)
            && match (self.started_at, self.crashed_at) {
                (Some(s), Some(c)) => c.saturating_sub(s) < DOA_WINDOW_MS,
                _ => false,
            }
    }

    pub fn is_adopted(&self) -> bool {
        self.adopted
    }

    /// Re-attach to a process from a prior app instance. We have only its PID
    /// (no Child, no pipes), so mark it Running+adopted, restore start time and
    /// port, and push one line explaining the frozen log pane.
    pub fn adopt(&mut self, pid: u32, started_at: u64, port: Option<u16>) {
        self.status = ProcStatus::Running;
        self.pid = Some(pid);
        self.started_at = Some(started_at);
        self.crashed_at = None;
        self.acquired_port = port;
        self.adopted = true;
        // The prior app instance's resolved env died with it - unknown here.
        self.resolved_env = None;
        self.push_log(
            "stdout",
            "[supervisor] re-adopted after restart - live logs paused until you restart this process".to_string(),
        );
    }

    pub fn info(&self) -> ProcInfo {
        ProcInfo {
            id: self.spec.id.clone(),
            project: self.spec.project.clone(),
            name: self.spec.name.clone(),
            kind: self.spec.kind.clone(),
            status: self.status.clone(),
            pid: self.pid,
            // Prefer the sampler's OS-detected port; fall back to the forced
            // port until the first sample lands (≤ one sampler tick after start).
            port: self.sampled_port.or(self.acquired_port),
            // Cached by the background sampler, never computed here: the UI poll
            // path must not enumerate the process table (that was the lag).
            mem_bytes: self.sampled_mem,
            cpu_pct: self.sampled_cpu_pct,
            started_at: self.started_at,
            fallback_port: self.fallback_port,
            resolved_env: self.resolved_env.clone(),
            env_unknown: self.adopted,
        }
    }

    pub fn logs_snapshot(&self) -> Vec<LogLine> {
        self.logs.lock().unwrap().iter().cloned().collect()
    }

    fn push_log(&self, stream: &str, text: String) {
        super::proc_log::push_line(&self.logs, stream, text);
    }
}

#[cfg(test)]
mod tests {
    use super::ManagedProc;
    use crate::types::{ProcKind, ProcSpec, ProcStatus};

    fn find_env<'a>(env: &'a [crate::types::EnvVar], key: &str) -> Option<&'a crate::types::EnvVar> {
        env.iter().find(|e| e.key == key)
    }

    fn test_spec() -> ProcSpec {
        ProcSpec {
            id: "proj:cmd".to_string(),
            project: "proj".to_string(),
            name: "cmd".to_string(),
            cmd: "flutter run".to_string(),
            cwd: ".".to_string(),
            kind: ProcKind::Flutter,
            autostart: false,
            use_dynamic_port: true,
            fixed_port: None,
            env: String::new(),
        }
    }

    #[test]
    fn dead_on_arrival_only_for_young_crashes() {
        let mut p = ManagedProc::new(test_spec());

        // Never started: not DOA.
        assert!(!p.is_dead_on_arrival());

        // Crashed 3s after start: DOA.
        p.status = ProcStatus::Crashed;
        p.started_at = Some(1_000);
        p.crashed_at = Some(4_000);
        assert!(p.is_dead_on_arrival());

        // Crashed 20min after start: a real crash, NOT DOA.
        p.crashed_at = Some(1_000 + 20 * 60 * 1_000);
        assert!(!p.is_dead_on_arrival());

        // Running (not crashed): not DOA regardless of timestamps.
        p.status = ProcStatus::Running;
        p.crashed_at = Some(4_000);
        assert!(!p.is_dead_on_arrival());

        // Exactly at the window edge (started_at + DOA_WINDOW_MS): NOT DOA - the
        // predicate uses strict `<`. Pins the off-by-one so the constant and the
        // comparison can never silently drift apart.
        p.status = ProcStatus::Crashed;
        p.started_at = Some(1_000);
        p.crashed_at = Some(1_000 + 8_000);
        assert!(!p.is_dead_on_arrival());
    }

    #[test]
    fn adopt_marks_running_without_a_child() {
        let mut p = ManagedProc::new(test_spec());
        p.adopt(4321, 1_000, Some(42013));
        assert_eq!(p.status, ProcStatus::Running);
        assert_eq!(p.pid, Some(4321));
        assert_eq!(p.started_at, Some(1_000));
        assert_eq!(p.acquired_port(), Some(42013));
        assert!(p.is_adopted());
        // Adopted with no child handle: is_dead_on_arrival must stay false
        // (it only fires on a Crashed status, which adopt never sets).
        assert!(!p.is_dead_on_arrival());
    }

    #[test]
    fn restart_clears_adoption() {
        // A re-adopted proc that is later restarted owns a real Child, so the
        // adopted flag must clear. start() spawns `cmd`, so drive it through a
        // command that exits immediately and assert the flag flipped.
        let mut spec = test_spec();
        spec.cmd = "cmd /C exit 0".to_string(); // trivial, exits instantly
        spec.kind = ProcKind::Generic;
        let mut p = ManagedProc::new(spec);
        p.adopt(4321, 1_000, None);
        assert!(p.is_adopted());
        let _ = p.start(None, None); // real spawn -> sets a Child, must clear adopted
        assert!(!p.is_adopted(), "start() must supersede adoption");
    }

    #[test]
    fn wants_proxy_gates_flutter_web_server_with_port_placeholder() {
        // flutter + web-server + {PORT}: proxiable.
        let mut spec = test_spec();
        spec.kind = ProcKind::Flutter;
        spec.cmd = "flutter run -d web-server --web-port {PORT}".to_string();
        assert!(ManagedProc::new(spec).wants_proxy());

        // flutter but no web-server (e.g. chrome device): not proxiable.
        let mut spec = test_spec();
        spec.kind = ProcKind::Flutter;
        spec.cmd = "flutter run -d chrome --web-port {PORT}".to_string();
        assert!(!ManagedProc::new(spec).wants_proxy());

        // flutter web-server but no {PORT} placeholder to redirect: not proxiable.
        let mut spec = test_spec();
        spec.kind = ProcKind::Flutter;
        spec.cmd = "flutter run -d web-server".to_string();
        assert!(!ManagedProc::new(spec).wants_proxy());

        // generic command: never proxiable.
        let mut spec = test_spec();
        spec.kind = ProcKind::Generic;
        spec.cmd = "npm run dev -- --port {PORT}".to_string();
        assert!(!ManagedProc::new(spec).wants_proxy());
    }

    #[test]
    fn start_captures_resolved_env_with_port_and_secret_flags() {
        let mut spec = test_spec();
        spec.kind = ProcKind::Generic;
        spec.cmd = "cmd /C exit 0".to_string(); // trivial, exits instantly
        spec.env = "BACKEND_URL=http://localhost:9000\nAPI_TOKEN=abc123".to_string();
        let mut p = ManagedProc::new(spec);
        let _ = p.start(Some(4321), None);

        let env = p.resolved_env.clone().expect("resolved_env set after a real spawn");
        let url = find_env(&env, "BACKEND_URL").expect("BACKEND_URL captured");
        assert_eq!(url.value, "http://localhost:9000");
        assert!(!url.secret, "URL-ish var must stay visible");

        let token = find_env(&env, "API_TOKEN").expect("API_TOKEN captured");
        assert_eq!(token.value, "abc123");
        assert!(token.secret, "TOKEN-named key must be masked");

        let port = find_env(&env, "PORT").expect("injected PORT captured");
        assert_eq!(port.value, "4321");
        assert!(!port.secret);

        // info() must expose the same, with env_unknown false (a real spawn,
        // not an adopted one).
        let info = p.info();
        assert!(!info.env_unknown);
        assert_eq!(info.resolved_env.expect("info exposes resolved_env").len(), 3);
    }

    #[test]
    fn adopted_process_reports_env_unknown_not_stale_or_empty() {
        let mut p = ManagedProc::new(test_spec());
        p.adopt(4321, 1_000, None);
        // No live handle, so the spawn-time env cannot be known.
        assert!(p.resolved_env.is_none());
        let info = p.info();
        assert!(info.env_unknown, "adopted proc must flag env as unknown");
        assert!(info.resolved_env.is_none(), "adopted proc must not show a stale/empty env block");
    }
}
