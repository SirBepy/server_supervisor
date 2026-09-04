use super::proxy;
use super::ManagedProc;
use crate::types::ProcStatus;
use std::process::Child;

/// The slow half of a stop, returned by `begin_stop`. Run `finish()` only
/// after releasing whatever lock guarded the proc.
pub struct StopHandle {
    pid: Option<u32>,
    child: Option<Child>,
    proxy: Option<proxy::ProxyTask>,
}

impl StopHandle {
    /// Kills the process tree, waits for exit, and joins the proxy's shutdown thread.
    pub fn finish(self) {
        if let Some(pid) = self.pid {
            super::super::reaper::kill_tree(pid);
        }
        if let Some(mut child) = self.child {
            let _ = child.wait();
        }
        drop(self.proxy);
    }
}

impl ManagedProc {
    /// Clears bookkeeping and reports Stopped immediately; hands back the
    /// slow OS-kill bits as a `StopHandle` to run with no registry lock held
    /// (observed 10-20s for some Windows process trees - see `StopHandle`).
    pub fn begin_stop(&mut self) -> StopHandle {
        let handle = StopHandle {
            pid: self.pid.take(),
            child: self.child.take(),
            proxy: self.proxy.take(),
        };
        self.reload_tx = None;
        self.internal_port = None;
        self.stdin = None;
        self.status = ProcStatus::Stopped;
        self.started_at = None;
        self.sampled_mem = None;
        self.sampled_cpu_pct = None;
        self.sampled_port = None;
        self.fallback_port = false;
        self.resolved_env = None;
        *self.app_id.lock().unwrap() = None;
        self.push_log("stdout", "[supervisor] stopped".to_string());
        handle
    }
}

#[cfg(test)]
mod tests {
    use super::super::ManagedProc;
    use crate::types::{ProcKind, ProcSpec};

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
    fn stopped_process_shows_no_env() {
        let mut spec = test_spec();
        spec.kind = ProcKind::Generic;
        spec.cmd = "cmd /C exit 0".to_string();
        spec.env = "FOO=bar".to_string();
        let mut p = ManagedProc::new(spec);
        let _ = p.start(None, None);
        assert!(p.resolved_env.is_some(), "sanity: a real run captures env");

        p.begin_stop().finish();
        assert!(p.resolved_env.is_none(), "stop() must drop the previous run's env");
        let info = p.info();
        assert!(!info.env_unknown, "stopped is a distinct, known state, not 'unknown'");
        assert!(info.resolved_env.is_none(), "stopped proc must not show a stale env");
    }
}
