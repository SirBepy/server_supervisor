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

pub(super) fn spawn_reader<R: Read + Send + 'static>(
    reader: R,
    stream: &'static str,
    logs: Arc<Mutex<VecDeque<LogLine>>>,
    app_id: Option<Arc<Mutex<Option<String>>>>,
    reload_tx: Option<broadcast::Sender<()>>,
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
