//! Per-process memory sampling for the dashboard RAM column.
//!
//! The figure reported for a supervised process is the resident memory of its
//! whole subtree (the pid plus every descendant), not just the top pid. A
//! `cargo`/`npm` launch spawns the real memory hogs (linkers, bundlers) as
//! grandchildren, so the top pid's own RSS reads near zero while RAM actually
//! disappears into descendants. Summing the subtree is the load-bearing part.

use crate::types::ProcInfo;
use std::collections::{HashMap, HashSet};
use sysinfo::{ProcessesToUpdate, System};

/// pid -> (parent pid, own resident bytes). The shape `subtree_rss` walks.
type ProcMap = HashMap<u32, (Option<u32>, u64)>;

/// Sum resident bytes of `root` plus every descendant, given the full process
/// map. Pure and deterministic so the tree-walk is unit-testable without a real
/// `System`. Cycles (which shouldn't occur in a pid graph) are guarded via
/// `seen` so a malformed map can't loop forever.
fn subtree_rss(root: u32, procs: &ProcMap) -> u64 {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, &(parent, _)) in procs {
        if let Some(pp) = parent {
            children.entry(pp).or_default().push(pid);
        }
    }
    let mut total = 0u64;
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(&(_, mem)) = procs.get(&pid) {
            total += mem;
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids);
        }
    }
    total
}

/// Snapshot a refreshed `System` into the pid map `subtree_rss` consumes.
fn snapshot(sys: &System) -> ProcMap {
    sys.processes()
        .iter()
        .map(|(pid, p)| (pid.as_u32(), (p.parent().map(|pp| pp.as_u32()), p.memory())))
        .collect()
}

/// Fill `mem_bytes` for every running entry (one with a pid) using a single
/// shared `System` refresh pass. Stopped entries are left `None`. Does nothing
/// (and skips the refresh entirely) when nothing is running.
pub fn fill_memory(infos: &mut [ProcInfo]) {
    if infos.iter().all(|i| i.pid.is_none()) {
        return;
    }
    let mut sys = System::new();
    // One pass over all processes; `subtree_rss` needs the full parent graph to
    // attribute descendants, so we refresh All rather than just the tracked pids.
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let procs = snapshot(&sys);
    for info in infos.iter_mut() {
        if let Some(pid) = info.pid {
            info.mem_bytes = Some(subtree_rss(pid, &procs));
        }
    }
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
