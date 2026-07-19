//! One-shot system-wide RAM + CPU sampling for the dashboard's stats tile.
//!
//! Unlike `sampler.rs` (which caches per-process figures on a background tick
//! against a fresh `System` each pass), this is a plain synchronous getter with
//! no cache and no long-lived `System` in shared state - callers are infrequent
//! (a dashboard tile, not a per-second poll), so a fresh `System` per call
//! mirrors the existing per-process sampling and avoids holding sysinfo state
//! for the life of the app. `sysinfo` needs two `refresh_cpu_usage` calls
//! `MINIMUM_CPU_UPDATE_INTERVAL` apart to compute a meaningful CPU delta, so
//! this briefly blocks - call it off the UI thread.

use sysinfo::{System, MINIMUM_CPU_UPDATE_INTERVAL};

/// Total/used system RAM in bytes, and overall CPU usage as a percentage.
pub struct Sample {
    pub total_mem_bytes: u64,
    pub used_mem_bytes: u64,
    pub cpu_pct: f32,
}

/// Sample system-wide RAM + CPU.
pub fn sample() -> Sample {
    let mut sys = System::new();
    sys.refresh_cpu_usage();
    std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    Sample {
        total_mem_bytes: sys.total_memory(),
        used_mem_bytes: sys.used_memory(),
        cpu_pct: sys.global_cpu_usage(),
    }
}
