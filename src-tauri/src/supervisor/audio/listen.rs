//! Lets an agent hear a supervised app by sampling its sessions' peak
//! meters. Works whether or not the dev can hear the app, because the
//! session meter reads the signal before the mute (see `Scanner::meters_for`).
//! A recording would not: process loopback captures after the mute, so a
//! muted app records as pure silence (`tests/media_spike.rs`).

use super::session::Scanner;
use serde::Serialize;
use std::time::{Duration, Instant};

/// Below about -60 dBFS counts as silence.
const ACTIVE_PEAK: f32 = 0.001;
/// The engine updates a session meter once per ~10ms processing period.
const SAMPLE_EVERY: Duration = Duration::from_millis(10);
/// Picks up a session the app opens mid-listen (many open one lazily on
/// their first sound).
const RESCAN_EVERY: Duration = Duration::from_millis(250);

#[derive(Debug, Serialize)]
pub struct Listening {
    pub duration_ms: u64,
    /// Loudest sample seen, 0.0 to 1.0 of full scale.
    pub peak: f32,
    /// How long the app was audibly producing sound, in ms.
    pub active_ms: u64,
    /// Ms from the start of listening to the first audible sample.
    pub first_sound_ms: Option<u64>,
    /// Audio sessions the app had open at the end; 0 means it never
    /// touched the audio stack at all.
    pub sessions: usize,
    pub heard: bool,
}

/// Blocks for `duration`, so callers run it off any async executor.
pub fn listen(root_pid: u32, duration: Duration) -> Result<Listening, String> {
    let scanner = Scanner::new().map_err(|e| format!("Core Audio unavailable: {e}"))?;
    let start = Instant::now();
    let mut meters = scanner.meters_for(root_pid);
    let mut last_scan = start;
    let mut last_tick = start;
    let mut peak = 0f32;
    let mut active = Duration::ZERO;
    let mut first_sound = None;
    while start.elapsed() < duration {
        if last_scan.elapsed() >= RESCAN_EVERY {
            meters = scanner.meters_for(root_pid);
            last_scan = Instant::now();
        }
        let now = Instant::now();
        let p = meters
            .iter()
            .filter_map(|m| unsafe { m.GetPeakValue() }.ok())
            .fold(0f32, f32::max);
        peak = peak.max(p);
        if p >= ACTIVE_PEAK {
            active += now - last_tick;
            first_sound.get_or_insert(start.elapsed().as_millis() as u64);
        }
        last_tick = now;
        std::thread::sleep(SAMPLE_EVERY);
    }
    Ok(Listening {
        duration_ms: start.elapsed().as_millis() as u64,
        peak,
        active_ms: active.as_millis() as u64,
        first_sound_ms: first_sound,
        sessions: meters.len(),
        heard: first_sound.is_some(),
    })
}
