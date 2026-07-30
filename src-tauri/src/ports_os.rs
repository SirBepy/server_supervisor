//! OS-level port probing: what's actually listening right now, read live from
//! the OS TCP table (and a loopback bind-probe as a secondary check). Pure
//! detection, no bookkeeping - see `ports` for the registry that tracks which
//! ports this app has assigned to which project/command.

use std::net::TcpListener;

/// A port counts as free only if it can be bound on BOTH the IPv4 and the IPv6
/// loopback. Servers (notably Node) frequently bind the IPv6 wildcard `[::]:port`,
/// which occupies the port for localhost clients while leaving the IPv4 bind free;
/// probing only `127.0.0.1` would then hand out a port that is actually taken.
/// On Windows `IPV6_V6ONLY` defaults to true, so the two binds are independent and
/// must both succeed. Either failure means "taken".
///
/// This is a secondary check: on Windows a process bound to the wildcard
/// `[::]:port` does NOT block a later bind to the specific loopback `[::1]:port`
/// (specific-vs-wildcard binds don't conflict without `SO_EXCLUSIVEADDRUSE`), so
/// the bind-probe alone can report such a port free. `listening_ports()` (the OS
/// TCP table) is the primary detector for those holders; see `acquire`.
pub(crate) fn port_free(port: u16) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
        && TcpListener::bind((Ipv6Addr::LOCALHOST, port)).is_ok()
}

/// The set of local ports currently in TCP LISTENING state, read once from the
/// OS via `netstat -ano`. Covers both IPv4 (`0.0.0.0:port`) and IPv6
/// (`[::]:port`) wildcard listeners regardless of how they were bound, which a
/// bind-probe cannot reliably detect on Windows. Best-effort: returns an empty
/// set if netstat is unavailable (the bind-probe still applies). Derived from the
/// single `listeners()` netstat reader, dropping the owning PID.
pub(crate) fn listening_ports() -> std::collections::HashSet<u16> {
    listeners().into_iter().map(|(p, _)| p).collect()
}

/// `(port, owning pid)` for every TCP listener, read once from the OS via
/// `netstat -ano`. This is the single netstat call site in the codebase; other
/// modules (e.g. `supervisor::sampler`) consume this. Best-effort: empty on any
/// failure (callers keep forced ports / fall back to the bind-probe).
#[cfg(windows)]
pub(crate) fn listeners() -> Vec<(u16, u32)> {
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
pub(crate) fn listeners() -> Vec<(u16, u32)> {
    Vec::new()
}

/// Pure parser for `netstat -ano` output. Columns: Proto, Local Address, Foreign
/// Address, State, PID. We keep only TCP rows in the LISTENING state. Kept pure
/// (separate from the `Command` invocation) so it stays unit-testable on any
/// platform.
pub(crate) fn parse_listeners(text: &str) -> Vec<(u16, u32)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 5 && cols[0].eq_ignore_ascii_case("TCP") && cols[3] == "LISTENING" {
            if let (Some(port), Ok(pid)) = (local_port(cols[1]), cols[4].parse::<u32>()) {
                out.push((port, pid));
            }
        }
    }
    out
}

/// Parse the port from a netstat local-address column: `0.0.0.0:42000`,
/// `[::]:42000`, `127.0.0.1:42000`, `[::1]:42000`. The port is the segment after
/// the final `:`. The single local-address port parser for the codebase.
pub(crate) fn local_port(local: &str) -> Option<u16> {
    local.rsplit(':').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_free_detects_ipv6_only_bind() {
        use std::net::{Ipv6Addr, TcpListener};
        // Bind the IPv6 loopback only (mirrors a server on `[::]:port`); IPv4 is
        // still free, so an IPv4-only probe would wrongly report the port free.
        let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!port_free(port), "IPv6-bound port must count as taken");
    }

    #[test]
    fn local_port_parses_netstat_addresses() {
        assert_eq!(local_port("0.0.0.0:42000"), Some(42000));
        assert_eq!(local_port("[::]:42000"), Some(42000));
        assert_eq!(local_port("127.0.0.1:6969"), Some(6969));
        assert_eq!(local_port("[::1]:1"), Some(1));
        assert_eq!(local_port("*:*"), None);
    }

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

    #[cfg(windows)]
    #[test]
    fn listening_ports_includes_a_bound_port() {
        use std::net::{Ipv4Addr, TcpListener};
        // Bind a real port and confirm the OS TCP table reports it as listening.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(
            listening_ports().contains(&port),
            "a freshly bound listener must show up in the OS LISTENING set"
        );
    }
}
