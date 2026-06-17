//! Flutter `--machine` daemon-protocol parsing, split out from `proc.rs`.
//!
//! These are pure functions over the JSON-RPC strings the `flutter run
//! --machine` daemon emits on stdout (plus the `--machine` flag injection
//! applied to the launch command). `proc.rs` owns the generic process
//! lifecycle and calls into here for the flutter-specific bits.

use crate::types::ProcKind;

/// Force `--machine` onto a flutter launch command so the daemon speaks the
/// JSON-RPC protocol. Machine mode is what lets us drive an `app.restart` over
/// stdin for a fast hot restart (and later a browser-reload signal); without it
/// the daemon ignores our requests. Inserts the flag right after `run` when
/// present, else appends it; idempotent when `--machine` is already there.
pub(crate) fn inject_machine_flag(cmd: &str, kind: &ProcKind) -> String {
    let mut cmd_str = cmd.to_string();
    if *kind == ProcKind::Flutter && !cmd_str.contains("--machine") {
        let mut tokens: Vec<String> = cmd_str.split_whitespace().map(|t| t.to_string()).collect();
        match tokens.iter().position(|t| t == "run") {
            Some(i) => tokens.insert(i + 1, "--machine".to_string()),
            None => tokens.push("--machine".to_string()),
        }
        cmd_str = tokens.join(" ");
    }
    cmd_str
}

/// Parse a `flutter run --machine` line for the `app.started` event and return its appId.
pub(crate) fn parse_flutter_app_id(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') || !trimmed.contains("app.started") {
        return None;
    }
    let arr: Vec<serde_json::Value> = serde_json::from_str(trimmed).ok()?;
    for evt in arr {
        if evt.get("event").and_then(|e| e.as_str()) == Some("app.started") {
            if let Some(id) = evt
                .get("params")
                .and_then(|p| p.get("appId"))
                .and_then(|a| a.as_str())
            {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// The humanized result of parsing one `flutter run --machine` JSON-RPC line:
/// the readable text line(s) to push to the log pane, plus whether this line
/// signals a completed (re)start that should trigger a browser reload.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct FlutterLog {
    /// Readable lines to push verbatim into the log buffer (already prefixed).
    pub lines: Vec<String>,
    /// True when this line means a restart finished and an open tab should reload.
    pub fire_reload: bool,
}

/// Parse a `flutter run --machine` JSON-RPC line (an array of event/response
/// objects) into readable log lines plus a reload-completion signal. Returns
/// `None` when the line is not machine JSON (caller falls back to verbatim).
pub(crate) fn parse_flutter_machine_line(line: &str) -> Option<FlutterLog> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') {
        return None;
    }
    let arr: Vec<serde_json::Value> = serde_json::from_str(trimmed).ok()?;
    let mut out = FlutterLog::default();
    for el in &arr {
        if let Some(event) = el.get("event").and_then(|e| e.as_str()) {
            let params = el.get("params");
            match event {
                // A full restart refreshing an already-open tab: announce + reload.
                "app.started" => {
                    out.lines.push("[flutter] app started".to_string());
                    out.fire_reload = true;
                }
                "app.progress" => {
                    if let Some(msg) = params
                        .and_then(|p| p.get("message"))
                        .and_then(|m| m.as_str())
                        .filter(|m| !m.is_empty())
                    {
                        let finished = params
                            .and_then(|p| p.get("finished"))
                            .and_then(|f| f.as_bool())
                            .unwrap_or(false);
                        if finished {
                            out.lines.push(format!("[flutter] {msg} (done)"));
                        } else {
                            out.lines.push(format!("[flutter] {msg}"));
                        }
                    }
                    // No message: emit nothing (transient progress with no text).
                }
                // The app's own stdout/print output: pass through with no prefix.
                "app.log" => {
                    if let Some(log) = params.and_then(|p| p.get("log")).and_then(|l| l.as_str()) {
                        out.lines.push(log.to_string());
                    }
                }
                "daemon.logMessage" => {
                    if let Some(msg) =
                        params.and_then(|p| p.get("message")).and_then(|m| m.as_str())
                    {
                        out.lines.push(format!("[flutter] {msg}"));
                    }
                }
                "app.webLaunchUrl" => {
                    if let Some(url) = flutter_url(params) {
                        out.lines.push(format!("[flutter] serving at {url}"));
                    }
                }
                other => {
                    if let Some(url) = flutter_url(params) {
                        out.lines.push(format!("[flutter] serving at {url}"));
                    } else {
                        out.lines.push(format!("[flutter] {other}"));
                    }
                }
            }
        } else if el.get("id").is_some() {
            // A response to one of our requests (e.g. the id:0 app.restart).
            if let Some(error) = el.get("error") {
                out.lines
                    .push(format!("[flutter] reload error: {error}"));
            } else if let Some(result) = el.get("result") {
                let code_ok = result
                    .get("code")
                    .and_then(|c| c.as_i64())
                    .map(|c| c == 0)
                    .unwrap_or(true); // no code field == success
                if code_ok {
                    out.lines.push("[flutter] reload complete".to_string());
                    out.fire_reload = true;
                } else {
                    out.lines.push(format!("[flutter] reload error: {result}"));
                }
            }
        }
    }
    Some(out)
}

/// Extract a serving URL from an event's params (`url` or `wsUri`), if present.
fn flutter_url(params: Option<&serde_json::Value>) -> Option<String> {
    let p = params?;
    p.get("url")
        .or_else(|| p.get("wsUri"))
        .and_then(|u| u.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProcKind;

    #[test]
    fn parses_app_started_event() {
        let line = r#"[{"event":"app.started","params":{"appId":"abc123","supportsRestart":true}}]"#;
        assert_eq!(parse_flutter_app_id(line), Some("abc123".to_string()));
    }

    #[test]
    fn ignores_non_started_lines() {
        assert_eq!(parse_flutter_app_id("Performing hot restart..."), None);
        assert_eq!(parse_flutter_app_id(r#"[{"event":"app.progress"}]"#), None);
        assert_eq!(parse_flutter_app_id("[not json"), None);
    }

    #[test]
    fn machine_line_app_started_fires_reload() {
        let line = r#"[{"event":"app.started","params":{"appId":"abc"}}]"#;
        let out = parse_flutter_machine_line(line).unwrap();
        assert_eq!(out.lines, vec!["[flutter] app started".to_string()]);
        assert!(out.fire_reload);
    }

    #[test]
    fn machine_line_restart_response_completes() {
        let line = r#"[{"id":0,"result":{"code":0,"message":"ok"}}]"#;
        let out = parse_flutter_machine_line(line).unwrap();
        assert_eq!(out.lines, vec!["[flutter] reload complete".to_string()]);
        assert!(out.fire_reload);
    }

    #[test]
    fn machine_line_restart_error_does_not_fire() {
        let line = r#"[{"id":0,"error":{"code":-32000,"message":"boom"}}]"#;
        let out = parse_flutter_machine_line(line).unwrap();
        assert!(out.lines[0].starts_with("[flutter] reload error:"));
        assert!(!out.fire_reload);
    }

    #[test]
    fn machine_line_progress_message_and_done() {
        let started = r#"[{"event":"app.progress","params":{"message":"Hot restarting"}}]"#;
        assert_eq!(
            parse_flutter_machine_line(started).unwrap().lines,
            vec!["[flutter] Hot restarting".to_string()]
        );
        let done = r#"[{"event":"app.progress","params":{"message":"Hot restart","finished":true}}]"#;
        assert_eq!(
            parse_flutter_machine_line(done).unwrap().lines,
            vec!["[flutter] Hot restart (done)".to_string()]
        );
        // No message: no line emitted.
        let empty = r#"[{"event":"app.progress","params":{}}]"#;
        assert!(parse_flutter_machine_line(empty).unwrap().lines.is_empty());
    }

    #[test]
    fn machine_line_app_log_passes_through() {
        let line = r#"[{"event":"app.log","params":{"log":"hello from app"}}]"#;
        assert_eq!(
            parse_flutter_machine_line(line).unwrap().lines,
            vec!["hello from app".to_string()]
        );
    }

    #[test]
    fn non_machine_line_returns_none() {
        assert_eq!(parse_flutter_machine_line("Launching lib/main.dart"), None);
        assert_eq!(parse_flutter_machine_line("[not json"), None);
    }

    #[test]
    fn flutter_start_injects_machine_after_run() {
        let injected = inject_machine_flag("flutter run -d chrome", &ProcKind::Flutter);
        assert_eq!(injected, "flutter run --machine -d chrome");
        let already = inject_machine_flag("flutter run --machine -d chrome", &ProcKind::Flutter);
        assert_eq!(already, "flutter run --machine -d chrome");
        let no_run = inject_machine_flag("flutter", &ProcKind::Flutter);
        assert_eq!(no_run, "flutter --machine");
        // Non-flutter kind: untouched.
        let generic = inject_machine_flag("npm run dev", &ProcKind::Generic);
        assert_eq!(generic, "npm run dev");
    }
}
