//! Per-process CPU sampling for the dashboard's "your apps" CPU figure.
//!
//! Mirrors `mem.rs`: the figure reported for a supervised process is the
//! summed CPU usage of its whole subtree (the pid plus every descendant), not
//! just the top pid, for the same reason RAM is summed - the real load often
//! lives in descendants (a linker under `cargo`, a bundler under `node`), not
//! the top wrapper process.
//!
//! `Process::cpu_usage()` is normalized to a SINGLE core (a process pegging
//! two cores reports ~200%), and needs the `System` refreshed at least twice
//! with real time elapsed between calls to compute a meaningful delta -
//! `sampler.rs` owns a `System` that persists across ticks (~3s apart) so that
//! delta is always available after the first tick.

use super::proc_tree;
use std::collections::HashMap;
use sysinfo::System;

/// pid -> (parent pid, own CPU usage %, normalized to one core). The shape
/// `subtree_cpu` walks.
pub(crate) type CpuMap = HashMap<u32, (Option<u32>, f32)>;

/// Sum CPU usage of `root` plus every descendant, given the full process map.
/// Still normalized to one core per process, so a subtree spanning several
/// busy cores can sum past 100 - callers divide by core count to compare
/// against the system-wide (already-normalized-to-100) figure.
pub(crate) fn subtree_cpu(root: u32, procs: &CpuMap) -> f32 {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, &(parent, _)) in procs {
        if let Some(pp) = parent {
            children.entry(pp).or_default().push(pid);
        }
    }
    proc_tree::subtree(root, &children)
        .iter()
        .filter_map(|pid| procs.get(pid).map(|&(_, cpu)| cpu))
        .sum()
}

/// Snapshot an already-refreshed `System` into the pid map `subtree_cpu`
/// consumes. The caller owns the `refresh_processes` pass (shared with `mem`
/// and `ports_detect` in `sampler.rs`).
pub(crate) fn snapshot(sys: &System) -> CpuMap {
    sys.processes()
        .iter()
        .map(|(pid, p)| (pid.as_u32(), (p.parent().map(|pp| pp.as_u32()), p.cpu_usage())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(u32, Option<u32>, f32)]) -> CpuMap {
        entries.iter().map(|&(p, par, c)| (p, (par, c))).collect()
    }

    #[test]
    fn subtree_sums_root_plus_descendants_not_just_top() {
        // 100 (cargo, ~idle) -> 200 (cmd) -> 300, 301 (rustc workers, busy).
        let procs = map(&[
            (100, None, 0.5),
            (200, Some(100), 1.0),
            (300, Some(200), 85.0),
            (301, Some(200), 40.0),
            (999, None, 9999.0), // unrelated process, must be excluded
        ]);
        assert_eq!(subtree_cpu(100, &procs), 0.5 + 1.0 + 85.0 + 40.0);
        // A leaf is just itself.
        assert_eq!(subtree_cpu(300, &procs), 85.0);
    }

    #[test]
    fn unknown_root_is_zero_and_cycles_terminate() {
        assert_eq!(subtree_cpu(42, &map(&[(1, None, 10.0)])), 0.0);
        let cyclic = map(&[(1, Some(2), 10.0), (2, Some(1), 20.0)]);
        assert_eq!(subtree_cpu(1, &cyclic), 30.0);
    }
}
