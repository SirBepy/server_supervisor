//! Auto-tunnel for the proxy hub reaching a Flutter mobile device over
//! `adb reverse`, so the hub's baked-in `127.0.0.1:<port>` address (project
//! `CLAUDE.md` hard rule: the hub binds loopback only, never externally)
//! stays byte-identical across desktop, Android emulator, and a USB-attached
//! device. On the emulator and over USB, `localhost:<port>` on the device is
//! NOT the host - `adb reverse tcp:<port> tcp:<port>` is what makes it one.
//!
//! Only the device Flutter itself selected gets tunnelled, parsed out of the
//! `--machine` daemon's `app.start` event, never every attached device: the
//! tunnel is a property of the running app, not of the machine, and
//! tunnelling devices nobody asked about would be a surprise on teardown.
//!
//! WiFi-connected devices stay out of scope: `adb reverse` only works over
//! USB or to an emulator, and relaxing the hub's loopback-only bind to reach
//! a WiFi device would break the hard rule above for a marginal case.
//!
//! Known edge, accepted rather than fixed: two Flutter procs tunnelling the
//! same port to the same device have the tunnel removed on the FIRST one's
//! stop, not reference-counted.
//!
//! A missing `adb`, or no device, must never fail or block a launch - every
//! fallible call here returns `Result<_, String>` for the caller to log and
//! continue past, not `panic!`/`expect`.

use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Flutter device ids that are never mobile (desktop + web + browser
/// targets). Anything else - `emulator-5554`, a USB serial - is mobile.
const NON_MOBILE_DEVICES: &[&str] = &["chrome", "web-server", "edge", "windows", "macos", "linux"];

/// True for any device id Flutter can launch onto that is NOT in the
/// known-non-mobile list above (case-insensitive: Flutter itself lowercases
/// these, but match defensively).
pub(crate) fn is_mobile_device(id: &str) -> bool {
    !NON_MOBILE_DEVICES.iter().any(|d| d.eq_ignore_ascii_case(id))
}

/// Parse a `flutter run --machine` `app.start` event line for its
/// `deviceId`. Mirrors `flutter::parse_flutter_app_id`'s shape (same JSON
/// array-of-events protocol), a different event name and field: `app.start`
/// fires once, early, carrying the device the daemon is about to launch onto
/// - before `app.started` carries the appId once the app is actually up.
pub(crate) fn parse_start_device(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') {
        return None;
    }
    let arr: Vec<serde_json::Value> = serde_json::from_str(trimmed).ok()?;
    for evt in arr {
        if evt.get("event").and_then(|e| e.as_str()) == Some("app.start") {
            if let Some(id) = evt
                .get("params")
                .and_then(|p| p.get("deviceId"))
                .and_then(|d| d.as_str())
            {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Given one daemon stdout line and the hub tunnel port configured for this
/// proc (`None` when the proc isn't Flutter or its project has no hub
/// presets), call `forward(device, port)` exactly when the line is the
/// `app.start` event for a mobile device. Pure aside from the injected
/// callback, so the reader-wiring behavior is testable without touching adb.
pub(crate) fn maybe_forward<F: FnMut(&str, u16)>(line: &str, tunnel_port: Option<u16>, mut forward: F) {
    let Some(port) = tunnel_port else { return };
    let Some(device) = parse_start_device(line) else { return };
    if !is_mobile_device(&device) {
        return;
    }
    forward(&device, port);
}

/// Resolve the hub tunnel port for a just-starting proc, or `None` when it
/// doesn't apply. `None` covers: not a Flutter proc, or its project has no
/// hub presets configured yet - in both cases behavior must stay
/// byte-identical to a build without this feature.
pub(crate) fn resolve_tunnel_port(
    ports: &crate::ports::PortRegistry,
    projects: &[crate::types::Project],
    project_id: &str,
    kind: &crate::types::ProcKind,
) -> Option<u16> {
    if *kind != crate::types::ProcKind::Flutter {
        return None;
    }
    let has_presets = projects.iter().any(|p| p.id == project_id && !p.presets.is_empty());
    if !has_presets {
        return None;
    }
    ports.project_hub_port(project_id).ok()
}

/// `adb`'s executable name for this platform (there is no `.exe` on
/// non-Windows, though the app otherwise targets Windows only).
fn adb_exe_name() -> &'static str {
    if cfg!(windows) { "adb.exe" } else { "adb" }
}

/// Resolve `adb`: first via PATH, then the common Android SDK env vars and
/// install location, mirroring the project's other toolchain-resolution code
/// (`spawn_env::registry_merged_path`). `None` when nothing exists - the
/// caller must treat that as "skip the tunnel", never a launch failure.
pub(crate) fn adb_path() -> Option<PathBuf> {
    let name = adb_exe_name();
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(v) = std::env::var_os("ANDROID_HOME") {
        roots.push(PathBuf::from(v));
    }
    if let Some(v) = std::env::var_os("ANDROID_SDK_ROOT") {
        roots.push(PathBuf::from(v));
    }
    if let Some(v) = std::env::var_os("LOCALAPPDATA") {
        roots.push(Path::new(&v).join("Android").join("Sdk"));
    }
    roots
        .into_iter()
        .map(|root| root.join("platform-tools").join(name))
        .find(|candidate| candidate.is_file())
}

/// Tunnel the device's own `localhost:<port>` to the host's. `pub` (not
/// `pub(crate)`): the `#[ignore]`d live spike in `tests/adb_reverse_spike.rs`
/// calls this directly against the real emulator.
pub fn forward(device: &str, port: u16) -> Result<(), String> {
    run_adb(&["-s", device, "reverse", &format!("tcp:{port}"), &format!("tcp:{port}")])
}

/// Remove a previously-set tunnel. Safe to call even if none was ever set
/// (adb reports an error on stderr; the caller only logs it). `pub` for the
/// same reason as `forward`.
pub fn remove(device: &str, port: u16) -> Result<(), String> {
    run_adb(&["-s", device, "reverse", "--remove", &format!("tcp:{port}")])
}

fn run_adb(args: &[&str]) -> Result<(), String> {
    let adb = adb_path().ok_or_else(|| "adb not found on PATH or in a known SDK location".to_string())?;
    let mut command = Command::new(adb);
    command.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let out = command.output().map_err(|e| format!("failed to run adb: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if stderr.is_empty() {
            Err(format!("adb exited with {}", out.status))
        } else {
            Err(stderr)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Project, UpstreamPreset};

    #[test]
    fn parses_app_start_device_id() {
        let line = r#"[{"event":"app.start","params":{"appId":"abc","deviceId":"emulator-5554"}}]"#;
        assert_eq!(parse_start_device(line), Some("emulator-5554".to_string()));
    }

    #[test]
    fn ignores_other_events_and_non_json() {
        assert_eq!(
            parse_start_device(r#"[{"event":"app.started","params":{"appId":"abc"}}]"#),
            None,
            "app.started (a different event) must not be mistaken for app.start"
        );
        assert_eq!(parse_start_device(r#"[{"event":"app.progress"}]"#), None);
        assert_eq!(parse_start_device("Launching lib/main.dart"), None);
        assert_eq!(parse_start_device("[not json"), None);
    }

    #[test]
    fn mobile_device_table() {
        for id in ["chrome", "web-server", "edge", "windows", "macos", "linux"] {
            assert!(!is_mobile_device(id), "{id} must not be treated as mobile");
        }
        for id in ["emulator-5554", "ZY223JQZPT", "R58M12AB3CD"] {
            assert!(is_mobile_device(id), "{id} must be treated as mobile");
        }
    }

    #[test]
    fn maybe_forward_schedules_only_for_mobile_device_with_a_tunnel_port() {
        let mobile_line = r#"[{"event":"app.start","params":{"appId":"abc","deviceId":"emulator-5554"}}]"#;
        let chrome_line = r#"[{"event":"app.start","params":{"appId":"abc","deviceId":"chrome"}}]"#;

        let mut calls: Vec<(String, u16)> = Vec::new();
        maybe_forward(mobile_line, Some(4123), |d, p| calls.push((d.to_string(), p)));
        assert_eq!(
            calls,
            vec![("emulator-5554".to_string(), 4123)],
            "a mobile device with a tunnel port configured must schedule a forward"
        );

        let mut chrome_calls: Vec<(String, u16)> = Vec::new();
        maybe_forward(chrome_line, Some(4123), |d, p| chrome_calls.push((d.to_string(), p)));
        assert!(chrome_calls.is_empty(), "chrome must never be tunnelled");

        let mut no_port_calls: Vec<(String, u16)> = Vec::new();
        maybe_forward(mobile_line, None, |d, p| no_port_calls.push((d.to_string(), p)));
        assert!(
            no_port_calls.is_empty(),
            "no hub tunnel port configured (not flutter, or no presets): never forwards"
        );
    }

    fn preset() -> UpstreamPreset {
        UpstreamPreset { id: "p1".into(), name: "P1".into(), base_url: "http://x".into(), danger: false }
    }

    fn project(id: &str, presets: Vec<UpstreamPreset>) -> Project {
        Project {
            id: id.to_string(),
            name: id.to_string(),
            root: ".".into(),
            commands: vec![],
            presets,
            active_preset: None,
            transient: false,
            transient_label: None,
        }
    }

    #[test]
    fn resolve_tunnel_port_is_none_for_non_flutter_or_no_presets() {
        let dir = tempfile::tempdir().unwrap();
        let reg = crate::ports::PortRegistry::new(dir.path().to_path_buf());
        let projects = vec![project("proj", vec![preset()])];

        assert_eq!(
            resolve_tunnel_port(&reg, &projects, "proj", &crate::types::ProcKind::Generic),
            None,
            "a generic (non-flutter) proc never gets a tunnel port"
        );

        let no_presets = vec![project("proj", vec![])];
        assert_eq!(
            resolve_tunnel_port(&reg, &no_presets, "proj", &crate::types::ProcKind::Flutter),
            None,
            "a flutter proc in a project with no hub presets never gets a tunnel port"
        );
    }

    #[test]
    fn resolve_tunnel_port_resolves_the_hub_port_for_flutter_with_presets() {
        let dir = tempfile::tempdir().unwrap();
        let reg = crate::ports::PortRegistry::new(dir.path().to_path_buf());
        let projects = vec![project("proj", vec![preset()])];
        let expected = reg.project_hub_port("proj").unwrap();
        assert_eq!(
            resolve_tunnel_port(&reg, &projects, "proj", &crate::types::ProcKind::Flutter),
            Some(expected)
        );
    }
}
