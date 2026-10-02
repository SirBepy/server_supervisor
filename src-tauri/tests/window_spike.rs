//! Spike for headless docking, window capture and posted input. Answers,
//! against a real Edge process, the unknowns the production
//! `supervisor::window::capture` module is built on: which invisible host
//! window keeps a docked Chromium guest rendering live (offscreen,
//! DWM-cloaked, near-transparent layered), and does a Chromium guest accept
//! mouse/keyboard input posted as window messages, with no real cursor or
//! focus change? Split from `tests/media_spike.rs`'s window-spike half
//! (todo 0060); the Core Audio half now lives in `tests/audio_spike.rs`.
//!
//! The only `#[test]` here is `#[ignore]`: it spawns a real GUI app and
//! pokes real windows. Run with:
//!   cargo test --test window_spike -- --ignored --nocapture --test-threads=1
//! Set `SPIKE_OUT` to a directory to also write PNG evidence.

#![cfg(windows)]

mod spike_common;
mod window_capture;
mod window_host;

use window_capture::{find_msedge, run_host_case, PAGE};
use window_host::HostKind;

#[test]
#[ignore]
fn spike_headless_hosts_capture_and_input() {
    let Some(msedge) = find_msedge() else {
        println!("SKIPPED: msedge.exe not found");
        return;
    };
    let html_dir = std::env::temp_dir().join("media_spike_html");
    let _ = std::fs::create_dir_all(&html_dir);
    let html_path = html_dir.join("page.html");
    std::fs::write(&html_path, PAGE).unwrap();
    let html_url = format!("file:///{}", html_path.to_string_lossy().replace('\\', "/"));
    let only = std::env::var("SPIKE_HOST").ok();
    for kind in [HostKind::Visible, HostKind::Offscreen, HostKind::Cloaked, HostKind::Layered] {
        if only.as_deref().is_some_and(|o| !format!("{kind:?}").eq_ignore_ascii_case(o)) {
            continue;
        }
        run_host_case(kind, &msedge, &html_url);
    }
}
