use crate::types::LogLine;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

use super::proc::{now_ms, LOG_CAP};

pub(super) fn push_line(logs: &Arc<Mutex<VecDeque<LogLine>>>, stream: &str, text: String) {
    let mut buf = logs.lock().unwrap();
    if buf.len() >= LOG_CAP {
        buf.pop_front();
    }
    buf.push_back(LogLine {
        ts: now_ms(),
        stream: stream.to_string(),
        text,
    });
}

/// `tunnel` is `Some((port, state))` only for the stdout reader of a Flutter
/// proc whose project has hub presets (`None` for stderr, and for any other
/// proc kind) - see `ManagedProc::hub_tunnel_port` / `adb_reverse`.
pub(super) fn spawn_reader<R: Read + Send + 'static>(
    reader: R,
    stream: &'static str,
    logs: Arc<Mutex<VecDeque<LogLine>>>,
    app_id: Option<Arc<Mutex<Option<String>>>>,
    reload_tx: Option<broadcast::Sender<()>>,
    tunnel: Option<(u16, Arc<Mutex<Option<(String, u16)>>>)>,
) {
    std::thread::spawn(move || {
        let buffered = BufReader::new(reader);
        for line in buffered.lines() {
            let Ok(text) = line else { break };
            // Capture the Flutter daemon appId from the raw JSON, exactly as before.
            if let Some(slot) = &app_id {
                if slot.lock().unwrap().is_none() {
                    if let Some(id) = super::flutter::parse_flutter_app_id(&text) {
                        *slot.lock().unwrap() = Some(id);
                    }
                }
            }
            // On the daemon's app.start event for a mobile device, kick off
            // the adb reverse tunnel. The check-and-set under `state`'s lock
            // is synchronous (fast) so a second app.start line never double-
            // schedules; the actual adb call runs on a detached thread so a
            // slow or missing adb can never block this reader.
            if let Some((port, state)) = &tunnel {
                super::adb_reverse::maybe_forward(&text, Some(*port), |device, port| {
                    let mut guard = state.lock().unwrap();
                    if guard.is_some() {
                        return;
                    }
                    *guard = Some((device.to_string(), port));
                    drop(guard);
                    let device = device.to_string();
                    let state = state.clone();
                    let logs = logs.clone();
                    std::thread::spawn(move || match super::adb_reverse::forward(&device, port) {
                        Ok(()) => push_line(&logs, "stdout", format!("[supervisor] adb reverse tcp:{port} -> {device}")),
                        Err(e) => {
                            push_line(
                                &logs,
                                "stderr",
                                format!("[supervisor] adb reverse tcp:{port} -> {device} failed: {e}; continuing without tunnel"),
                            );
                            *state.lock().unwrap() = None;
                        }
                    });
                });
            }
            // Humanize machine JSON into readable lines; on any non-JSON line
            // (pre-daemon "Launching...", plain stderr) push it verbatim once.
            match super::flutter::parse_flutter_machine_line(&text) {
                Some(parsed) => {
                    for l in parsed.lines {
                        push_line(&logs, stream, l);
                    }
                    if parsed.fire_reload {
                        if let Some(tx) = &reload_tx {
                            let _ = tx.send(());
                        }
                    }
                }
                None => push_line(&logs, stream, text),
            }
        }
    });
}
