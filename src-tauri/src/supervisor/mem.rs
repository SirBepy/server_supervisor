//! Per-process memory sampling for the dashboard RAM column.
//!
//! The figure reported for a supervised process is the resident memory of its
//! whole subtree (the pid plus every descendant), not just the top pid. A
//! `cargo`/`npm` launch spawns the real memory hogs (linkers, bundlers) as
//! grandchildren, so the top pid's own RSS reads near zero while RAM actually
//! disappears into descendants. Summing the subtree is the load-bearing part.
//!
//! These are pure helpers over a `System` snapshot. The combined background
//! sampler (`supervisor::sampler`) owns the single `refresh_processes` pass and
//! drives both this and `ports_detect` off it, so the UI poll path never
//! enumerates the process table.

use super::proc_tree;
use std::collections::HashMap;
use sysinfo::System;

/// pid -> (parent pid, own resident bytes). The shape `subtree_rss` walks.
pub(crate) type ProcMap = HashMap<u32, (Option<u32>, u64)>;

/// Sum resident bytes of `root` plus every descendant, given the full process
/// map. The cycle-guarded subtree BFS is shared (`proc_tree::subtree`); this
/// just builds the parent->children map from the `ProcMap` and sums RSS over the
/// returned pid set. Pure and deterministic so it's unit-testable without a real
/// `System`.
pub(crate) fn subtree_rss(root: u32, procs: &ProcMap) -> u64 {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, &(parent, _)) in procs {
        if let Some(pp) = parent {
            children.entry(pp).or_default().push(pid);
        }
    }
    proc_tree::subtree(root, &children)
        .iter()
        .filter_map(|pid| procs.get(pid).map(|&(_, mem)| mem))
        .sum()
}

/// Snapshot an already-refreshed `System` into the pid map `subtree_rss`
/// consumes. The caller owns the `refresh_processes(All)` pass (the sampler does
/// it once and shares it with `ports_detect`).
pub(crate) fn snapshot(sys: &System) -> ProcMap {
    sys.processes()
        .iter()
        .map(|(pid, p)| (pid.as_u32(), (p.parent().map(|pp| pp.as_u32()), p.memory())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(u32, Option<u32>, u64)]) -> ProcMap {
        entries.iter().map(|&(p, par, m)| (p, (par, m))).collect()
    }

    #[test]
    fn subtree_sums_root_plus_descendants_not_just_top() {
        // 100 (cargo, ~tiny) -> 200 (cmd) -> 300, 301 (linkers, heavy).
        let procs = map(&[
            (100, None, 1_000),
            (200, Some(100), 2_000),
            (300, Some(200), 500_000),
            (301, Some(200), 400_000),
            (999, None, 9_999_999), // unrelated process, must be excluded
        ]);
        // Whole tree under 100, not the 1_000 the top pid reports alone.
        assert_eq!(subtree_rss(100, &procs), 1_000 + 2_000 + 500_000 + 400_000);
        // A leaf is just itself.
        assert_eq!(subtree_rss(300, &procs), 500_000);
    }

    #[test]
    fn unknown_root_is_zero_and_cycles_terminate() {
        assert_eq!(subtree_rss(42, &map(&[(1, None, 10)])), 0);
        // Malformed graph with a cycle (1<->2) must not loop forever.
        let cyclic = map(&[(1, Some(2), 10), (2, Some(1), 20)]);
        assert_eq!(subtree_rss(1, &cyclic), 30);
    }
}
