//! Best-effort detection of the REAL listening port of a running supervised
//! process.
//!
//! The OS TCP listening table (`netstat -ano`, which lists the owning PID) is
//! matched against each process's subtree. This is what lets the dashboard show
//! a port even when the child ignored our forced `PORT` and bound its own (e.g. a
//! config hardcoded to 8080), and corrects a forced port the child silently
//! refused. A forced/public port that is genuinely listening is treated as
//! authoritative, which also keeps a flutter live-reload proxy showing its public
//! port (bound by the supervisor itself, not by the child subtree).

use std::collections::{HashMap, HashSet};
use sysinfo::System;

// The port-resolution logic that used to live in `fill_ports` now lives in the
// combined background sampler (`supervisor::sampler`), which owns the single
// shared `System` refresh and reuses these pure helpers. The netstat reader and
// its local-address port parser live in `crate::ports` (the single OS-probe
// surface); this module keeps only the process-subtree helpers.

/// parent pid -> direct child pids, from a refreshed `System`.
pub(crate) fn children_map(sys: &System) -> HashMap<u32, Vec<u32>> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, p) in sys.processes() {
        if let Some(parent) = p.parent() {
            children.entry(parent.as_u32()).or_default().push(pid.as_u32());
        }
    }
    children
}

/// All pids in the subtree rooted at `root` (inclusive). `seen` guards against a
/// malformed (cyclic) pid graph looping forever, mirroring `mem::subtree_rss`.
pub(crate) fn subtree(root: u32, children: &HashMap<u32, Vec<u32>>) -> HashSet<u32> {
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids);
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtree_includes_root_and_all_descendants() {
        // 100 -> 200 -> {300, 301}; 999 unrelated.
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        children.insert(100, vec![200]);
        children.insert(200, vec![300, 301]);
        let tree = subtree(100, &children);
        assert_eq!(tree, HashSet::from([100, 200, 300, 301]));
        // A leaf is just itself; an unrelated pid is excluded.
        assert_eq!(subtree(300, &children), HashSet::from([300]));
        assert!(!tree.contains(&999));
    }

    #[test]
    fn subtree_terminates_on_cyclic_graph() {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        children.insert(1, vec![2]);
        children.insert(2, vec![1]);
        assert_eq!(subtree(1, &children), HashSet::from([1, 2]));
    }
}
