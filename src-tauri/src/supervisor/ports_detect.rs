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

use crate::types::ProcInfo;
use std::collections::{HashMap, HashSet};
use sysinfo::{ProcessesToUpdate, System};

/// Fill `port` for every running entry with the port it is actually listening on
/// when that differs from (or is missing from) the forced value. One shared
/// `netstat` + `System` pass. No-ops when nothing is running or netstat is
/// unavailable, leaving any forced port in place.
pub fn fill_ports(infos: &mut [ProcInfo]) {
    if infos.iter().all(|i| i.pid.is_none()) {
        return;
    }
    let listeners = listeners();
    if listeners.is_empty() {
        return;
    }
    let global: HashSet<u16> = listeners.iter().map(|(port, _)| *port).collect();

    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let children = children_map(&sys);

    for info in infos.iter_mut() {
        let Some(pid) = info.pid else { continue };
        // A forced/public port that is actually listening is authoritative. This
        // covers a working `--port` force AND a flutter proxy whose public port is
        // bound by the supervisor (so it never appears in the child's subtree).
        if let Some(forced) = info.port {
            if global.contains(&forced) {
                continue;
            }
        }
        // Otherwise detect: the lowest port any process in the child's subtree is
        // listening on. Covers a child that ignored our port and bound its own,
        // and a command launched with no dynamic port at all.
        let tree = subtree(pid, &children);
        let detected = listeners
            .iter()
            .filter(|(_, owner)| tree.contains(owner))
            .map(|(port, _)| *port)
            .min();
        if let Some(port) = detected {
            info.port = Some(port);
        }
    }
}

/// `(port, owning pid)` for every TCP listener, read once from the OS via
/// `netstat -ano`. Best-effort: empty on any failure (callers keep forced ports).
#[cfg(windows)]
fn listeners() -> Vec<(u16, u32)> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(out) = std::process::Command::new("netstat")
        .args(["-ano"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };
    parse_listeners(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(windows))]
fn listeners() -> Vec<(u16, u32)> {
    Vec::new()
}

/// Pure parser for `netstat -ano` output. Columns: Proto, Local Address, Foreign
/// Address, State, PID. We keep only TCP rows in the LISTENING state.
fn parse_listeners(text: &str) -> Vec<(u16, u32)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 5 && cols[0].eq_ignore_ascii_case("TCP") && cols[3] == "LISTENING" {
            if let (Some(port), Ok(pid)) = (port_of(cols[1]), cols[4].parse::<u32>()) {
                out.push((port, pid));
            }
        }
    }
    out
}

/// Parse the port from a netstat local-address column (`0.0.0.0:8080`,
/// `[::]:8080`, `127.0.0.1:6969`, `[::1]:1`). The port is the final `:` segment.
fn port_of(local: &str) -> Option<u16> {
    local.rsplit(':').next()?.parse().ok()
}

/// parent pid -> direct child pids, from a refreshed `System`.
fn children_map(sys: &System) -> HashMap<u32, Vec<u32>> {
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
fn subtree(root: u32, children: &HashMap<u32, Vec<u32>>) -> HashSet<u32> {
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
    fn parse_listeners_keeps_only_listening_tcp_with_pid() {
        // Mixed netstat output: a header, a LISTENING IPv4 row, an ESTABLISHED row
        // (must be dropped), a LISTENING IPv6 wildcard row, and a UDP row.
        let text = "\
Active Connections
  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:8080           0.0.0.0:0              LISTENING       1234
  TCP    127.0.0.1:6970         127.0.0.1:51000        ESTABLISHED     1234
  TCP    [::]:42013             [::]:0                 LISTENING       5678
  UDP    0.0.0.0:5353           *:*                                    900";
        let mut got = parse_listeners(text);
        got.sort();
        assert_eq!(got, vec![(8080, 1234), (42013, 5678)]);
    }

    #[test]
    fn port_of_parses_v4_and_v6() {
        assert_eq!(port_of("0.0.0.0:8080"), Some(8080));
        assert_eq!(port_of("[::]:42013"), Some(42013));
        assert_eq!(port_of("127.0.0.1:6969"), Some(6969));
        assert_eq!(port_of("[::1]:1"), Some(1));
        assert_eq!(port_of("*:*"), None);
    }

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
