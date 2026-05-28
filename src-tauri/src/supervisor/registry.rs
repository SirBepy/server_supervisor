use super::config;
use super::proc::ManagedProc;
use super::reaper::{self, PidEntry};
use crate::types::{LogLine, ProcInfo};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Owns every supervised process. The single owner of all spawned children:
/// nothing else holds their handles, so cleanup has one authority.
pub struct Supervisor {
    procs: Mutex<HashMap<String, ManagedProc>>,
    data_dir: PathBuf,
}

impl Supervisor {
    /// Build from the declared registry in `<data_dir>/procs.json`. All entries
    /// start in the Stopped state.
    pub fn new(data_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&data_dir);
        let specs = config::load(&data_dir);
        let mut map = HashMap::new();
        for spec in specs {
            map.insert(spec.id.clone(), ManagedProc::new(spec));
        }
        Self {
            procs: Mutex::new(map),
            data_dir,
        }
    }

    /// Kill any leftover children from a prior crashed session.
    pub fn reconcile_orphans(&self) {
        reaper::reconcile(&self.data_dir);
    }

    /// Start every process flagged `autostart` in the registry config.
    pub fn start_autostart(&self) {
        let ids: Vec<String> = {
            let guard = self.procs.lock().unwrap();
            guard
                .values()
                .filter(|p| p.spec.autostart)
                .map(|p| p.spec.id.clone())
                .collect()
        };
        for id in ids {
            if let Err(e) = self.start(&id) {
                log::error!("supervisor: autostart failed for {id}: {e}");
            }
        }
    }

    pub fn list(&self) -> Vec<ProcInfo> {
        let mut guard = self.procs.lock().unwrap();
        let mut out: Vec<ProcInfo> = guard
            .values_mut()
            .map(|p| {
                p.refresh();
                p.info()
            })
            .collect();
        out.sort_by(|a, b| (&a.project, &a.name).cmp(&(&b.project, &b.name)));
        out
    }

    pub fn logs(&self, id: &str) -> Result<Vec<LogLine>, String> {
        let guard = self.procs.lock().unwrap();
        guard
            .get(id)
            .map(|p| p.logs_snapshot())
            .ok_or_else(|| format!("unknown process id: {id}"))
    }

    pub fn start(&self, id: &str) -> Result<(), String> {
        {
            let mut guard = self.procs.lock().unwrap();
            let p = guard
                .get_mut(id)
                .ok_or_else(|| format!("unknown process id: {id}"))?;
            p.start().map_err(|e| e.to_string())?;
        }
        self.persist_pids();
        Ok(())
    }

    pub fn stop(&self, id: &str) -> Result<(), String> {
        {
            let mut guard = self.procs.lock().unwrap();
            let p = guard
                .get_mut(id)
                .ok_or_else(|| format!("unknown process id: {id}"))?;
            p.stop();
        }
        self.persist_pids();
        Ok(())
    }

    pub fn restart(&self, id: &str) -> Result<(), String> {
        self.stop(id)?;
        self.start(id)
    }

    /// Flutter-only hot reload / restart via the daemon's stdin.
    pub fn reload(&self, id: &str, full: bool) -> Result<(), String> {
        let mut guard = self.procs.lock().unwrap();
        let p = guard
            .get_mut(id)
            .ok_or_else(|| format!("unknown process id: {id}"))?;
        p.reload(full)
    }

    /// Kill every running child and clear the PID file. Called on app exit.
    pub fn shutdown_all(&self) {
        {
            let mut guard = self.procs.lock().unwrap();
            for p in guard.values_mut() {
                if p.pid.is_some() {
                    p.stop();
                }
            }
        }
        reaper::write_pids(&self.data_dir, &[]);
    }

    /// Snapshot running PIDs to `pids.json` for crash-recovery reaping.
    fn persist_pids(&self) {
        let entries: Vec<PidEntry> = {
            let guard = self.procs.lock().unwrap();
            guard
                .values()
                .filter_map(|p| {
                    p.pid.map(|pid| PidEntry {
                        id: p.spec.id.clone(),
                        pid,
                        started_at: p.started_at.unwrap_or(0),
                    })
                })
                .collect()
        };
        reaper::write_pids(&self.data_dir, &entries);
    }
}
