//! Shared process-tree helpers over a `sysinfo::System` snapshot.
//!
//! Both the RAM column (`mem::subtree_rss`) and the port detector
//! (`ports_detect`) need the same thing: the parent->children pid map from a
//! refreshed `System`, then a cycle-guarded BFS of the subtree under a root pid.
//! These are the single source of truth for that graph walk; the consumers layer
//! their own summation / set logic on top of the returned pid set.

use std::collections::{HashMap, HashSet};
use sysinfo::System;

/// Build the parent pid -> direct child pids map from an already-refreshed
/// `System`. The caller owns the `refresh_processes` pass.
pub(crate) fn snapshot_children(sys: &System) -> HashMap<u32, Vec<u32>> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, p) in sys.processes() {
        if let Some(parent) = p.parent() {
            children.entry(parent.as_u32()).or_default().push(pid.as_u32());
        }
    }
    children
}

/// All pids in the subtree rooted at `root` (inclusive), via a cycle-guarded
/// BFS. `seen` guards against a malformed (cyclic) pid graph looping forever.
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

    #[test]
    fn subtree_walks_root_plus_all_descendants_from_pid_map() {
        // Mirrors the tree-walk that mem::subtree_rss used to do inline:
        // 100 -> 200 -> {300, 301}; 999 unrelated, must be excluded.
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        children.insert(100, vec![200]);
        children.insert(200, vec![300, 301]);
        let tree = subtree(100, &children);
        assert_eq!(tree, HashSet::from([100, 200, 300, 301]));
        assert!(!tree.contains(&999));
        // A leaf is just itself.
        assert_eq!(subtree(300, &children), HashSet::from([300]));
    }

    #[test]
    fn unknown_root_is_just_itself() {
        let children: HashMap<u32, Vec<u32>> = HashMap::new();
        assert_eq!(subtree(42, &children), HashSet::from([42]));
    }
}
