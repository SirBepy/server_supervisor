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
//! shared `refresh_processes(All)` pass and reuses the pure helpers in `mem`,
//! `cpu` and `ports_detect` so RAM, CPU and port detection share one
//! enumeration.
//!
//! Unlike RAM (a point-in-time reading), per-process CPU needs the `System`
//! refreshed at least twice with real elapsed time between calls to compute a
//! meaningful delta. The caller (`Supervisor::sample_tick`) owns a `System`
//! that persists across ticks (~3s apart, comfortably past sysinfo's
//! `MINIMUM_CPU_UPDATE_INTERVAL`) and passes it in here each time, rather than
//! this function creating a fresh one - a fresh `System` would never have a
//! prior reading to diff against and every CPU figure would read as ~0.

use super::{cpu, mem, ports_detect};
use std::collections::{HashMap, HashSet};
use sysinfo::{ProcessesToUpdate, System};

/// One sampled snapshot for a running process: subtree resident bytes, subtree
/// CPU usage (normalized to the fraction of TOTAL system capacity, i.e. on the
/// same 0-100 scale as `sysstats::Sample::cpu_pct` - comparable directly, not
/// per-core), and the resolved port to display.
pub struct Sample {
    pub mem: u64,
    pub cpu_pct: f32,
    pub port: Option<u16>,
}

/// Sample RAM + CPU + detected port for each running process, keyed by
/// composite id. `sys` is the caller's persistent `System` (see module docs -
/// reused across ticks so CPU deltas are meaningful); this function only
/// refreshes and reads it, never (re)creates it.
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
pub fn sample(sys: &mut System, running: &[(String, u32, Option<u16>)]) -> HashMap<String, Sample> {
    let mut out = HashMap::new();
    if running.is_empty() {
        return out;
    }

    // The single shared pass: one full process enumeration, one netstat read.
    // `refresh_processes` already includes CPU (see `System::refresh_processes`
    // docs: `ProcessRefreshKind::nothing().with_memory().with_cpu()...`).
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let procmap = mem::snapshot(sys);
    let cpumap = cpu::snapshot(sys);
    let children = ports_detect::children_map(sys);
    let listeners = crate::ports::listeners();
    let global: HashSet<u16> = listeners.iter().map(|(port, _)| *port).collect();
    // `Process::cpu_usage()` is normalized to one core; dividing the subtree
    // sum by core count puts it on the same 0-100 "fraction of total system
    // capacity" scale sysstats' global_cpu_usage() already uses.
    let num_cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f32;

    for (id, pid, forced) in running {
        let pid = *pid;
        let forced = *forced;
        let mem_bytes = mem::subtree_rss(pid, &procmap);
        let cpu_pct = cpu::subtree_cpu(pid, &cpumap) / num_cpus;
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
        out.insert(id.clone(), Sample { mem: mem_bytes, cpu_pct, port });
    }
    out
}
