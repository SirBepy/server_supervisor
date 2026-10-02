//! Spike leg for todo 0057: proves posted input
//! (`supervisor::window::input`) drives a WebView2 window, the same shape
//! any Tauri v2 app has - a tao top-level window hosting a WebView2 content
//! child owned by a *different* process (msedgewebview2.exe). The Edge leg
//! in `window_spike.rs` already proved posted input against a same-process
//! Chromium window; this is the cross-process case Cueline (the dev's real
//! Tauri app, untested directly here) actually has.
//!
//! The probe lives in `spikes/webview2_probe` (a standalone cargo project,
//! not a workspace member). Build it first:
//!   $env:CARGO_TARGET_DIR='D:/cargo-target/webview2_probe'
//!   cargo build --release --manifest-path spikes/webview2_probe/Cargo.toml
//!
//! Run this leg with:
//!   cargo test --test window_webview2_spike -- --ignored --nocapture --test-threads=1
//! Set `SPIKE_OUT` to a directory to also write a PNG of the capture.
#![cfg(windows)]

mod spike_common;
mod window_capture;
mod window_host;

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

use server_supervisor_lib::supervisor::window::input::{send, InputAction};
use spike_common::kill_tree;
use window_capture::grab;
use window_host::{create_host, embed, find_window, pump_for, HostKind};

fn probe_exe() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        PathBuf::from(r"D:\cargo-target\webview2_probe\release\webview2_probe.exe"),
        manifest.join("../spikes/webview2_probe/target/release/webview2_probe.exe"),
    ];
    candidates.into_iter().find(|p| p.is_file()).unwrap_or_else(|| {
        panic!(
            "webview2_probe.exe not built. Run:\n  \
             $env:CARGO_TARGET_DIR='D:/cargo-target/webview2_probe'; \
             cargo build --release --manifest-path spikes/webview2_probe/Cargo.toml"
        )
    })
}

/// Parses the probe's `<count>\n<text>` state file.
fn read_state(path: &PathBuf) -> Option<(u32, String)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut lines = raw.splitn(2, '\n');
    let count: u32 = lines.next()?.trim().parse().ok()?;
    Some((count, lines.next().unwrap_or("").to_string()))
}

fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let end = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= end {
            return None;
        }
        pump_for(Duration::from_millis(50));
    }
}

/// Kills the probe's whole process tree (including its msedgewebview2.exe
/// children) and destroys the offscreen host even if an assert panics
/// mid-test.
struct Cleanup {
    pid: u32,
    host: HWND,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        kill_tree(self.pid);
        unsafe {
            let _ = DestroyWindow(self.host);
        }
    }
}

#[test]
#[ignore]
fn spike_webview2_probe_accepts_posted_input() {
    let exe = probe_exe();
    let state_path = std::env::temp_dir().join(format!("webview2_probe_state_{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&state_path);

    let child = Command::new(&exe).arg(&state_path).spawn().expect("spawn webview2_probe");
    let pid = child.id();
    let host = create_host(HostKind::Offscreen);
    let _cleanup = Cleanup { pid, host };

    let guest = find_window(pid, Duration::from_secs(15)).expect("webview2_probe window never appeared");
    let embedded = embed(guest, host);
    assert!(embedded, "SetParent into the offscreen host failed");
    pump_for(Duration::from_millis(1500));

    wait_for(Duration::from_secs(5), || read_state(&state_path)).expect("probe never wrote its initial state");

    // Click the button (fills the top 900x200 of the window). target_at
    // must walk down into the cross-process msedgewebview2.exe content
    // child for this to land at all.
    let g = guest.0 as isize;
    send(g, &InputAction::Click { x: 450, y: 100, button: Default::default(), double: false }).expect("posted click");
    let (count, _) = wait_for(Duration::from_secs(5), || read_state(&state_path).filter(|(c, _)| *c > 0))
        .expect("click never landed: count stayed 0");
    assert_eq!(count, 1, "button click count");

    // Click into the text field, then type - the same two-step dance the
    // Edge leg needs, since a never-activated window fires no autofocus.
    send(g, &InputAction::Click { x: 300, y: 240, button: Default::default(), double: false })
        .expect("posted click into field");
    pump_for(Duration::from_millis(300));
    send(g, &InputAction::Text { text: "hi0057".into() }).expect("posted text");
    let (_, text) = wait_for(Duration::from_secs(5), || read_state(&state_path).filter(|(_, t)| !t.is_empty()))
        .expect("typed text never landed");
    assert_eq!(text, "hi0057", "typed text");

    let frame = grab(Some(guest), RECT::default());
    assert!(frame.non_blank(), "screenshot of the webview2 probe was blank");
    frame.save("webview2_probe");

    println!("webview2 leg: click landed (count={count}), text landed ({text:?}), screenshot non-blank");
}
