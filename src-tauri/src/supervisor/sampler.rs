//! Combined off-UI-thread sampler for the dashboard's RAM + detected-port
//! columns.
//!
//! Both figures need a full `System` process snapshot (to attribute descendants
//! to a supervised pid), and the port figure also needs the OS TCP listener
//! table. Computing them used to happen inline in `Supervisor::list()`, which
//! runs on the **main UI thread** on every poll - two full `refresh_processes`
//! passes plus a `netstat` subprocess, every couple of seconds, blocking window
//! drag and click handling.
//!
//! Now the background reaper thread calls `Supervisor::sample_tick`, which runs
//! this once per tick on its own thread and caches the results on each
//! `ManagedProc`. `list()` just reads the cache. This module does the single
//! shared `refresh_processes(All)` pass and reuses the pure helpers in `mem`
//! and `ports_detect` so RAM and port detection share one enumeration.

use super::{mem, ports_detect};
use std::collections::{HashMap, HashSet};
use sysinfo::{ProcessesToUpdate, System};

/// One sampled snapshot for a running process: subtree resident bytes and the
/// resolved port to display.
pub struct Sample {
    pub mem: u64,
    pub port: Option<u16>,
}

/// Sample RAM + detected port for each running process, keyed by composite id.
///
/// `running` is `(id, pid, forced_port)` for every proc that currently holds a
/// pid; `forced_port` is the port the supervisor advertises (a dynamic/forced
/// port, or the flutter proxy's public port). Returns an empty map - and skips
/// the expensive enumeration entirely - when nothing is running.
///
/// Port precedence mirrors the old `fill_ports`: a forced/public port that is
/// actually listening is authoritative (covers a working `--port` force and the
/// flutter proxy, whose public port is bound by the supervisor itself and so is
/// absent from the child subtree); otherwise the lowest port any process in the
/// child's subtree listens on (covers a child that ignored our port and bound
/// its own, or a command with no dynamic port at all); otherwise the forced
/// value is kept as a best-effort default until it (or a real port) shows up.
pub fn sample(running: &[(String, u32, Option<u16>)]) -> HashMap<String, Sample> {
    let mut out = HashMap::new();
    if running.is_empty() {
        return out;
    }

    // The single shared pass: one full process enumeration, one netstat read.
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let procmap = mem::snapshot(&sys);
    let children = ports_detect::children_map(&sys);
    let listeners = crate::ports::listeners();
    let global: HashSet<u16> = listeners.iter().map(|(port, _)| *port).collect();

    for (id, pid, forced) in running {
        let pid = *pid;
        let forced = *forced;
        let mem_bytes = mem::subtree_rss(pid, &procmap);
        let port = forced
            .filter(|f| global.contains(f))
            .or_else(|| {
                let tree = ports_detect::subtree(pid, &children);
                listeners
                    .iter()
                    .filter(|(_, owner)| tree.contains(owner))
                    .map(|(port, _)| *port)
                    .min()
            })
            .or(forced);
        out.insert(id.clone(), Sample { mem: mem_bytes, port });
    }
    out
}
