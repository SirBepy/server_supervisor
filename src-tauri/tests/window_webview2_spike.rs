//! Spike leg for todo 0057: proves posted input
//! (`supervisor::window::input`) drives a WebView2 window, the same shape
//! any Tauri v2 app has - a tao top-level window hosting a WebView2 content
//! child owned by a *different* process (msedgewebview2.exe). The Edge leg
//! in `window_spike.rs` already proved posted input against a same-process
//! Chromium window; this is the cross-process case Cueline (the dev's real
//! Tauri app, untested directly here) actually has.
//!
//! This revision also drives the actual production headless-host/embed/
//! capture path (`supervisor::window::headless_host`, `::place::embed`,
//! `::capture::capture`) end to end, proving the DEFAULT `Offscreen` host
//! kind (everything that is not a Flutter runner window, see
//! `headless_host`'s module doc) still captures and accepts input - the
//! Flutter leg in `window_flutter_spike.rs` is the one exception to this
//! default, not a replacement for it.
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

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use server_supervisor_lib::supervisor::window::headless_host::{
    create_host, destroy_host, host_kind_for_class, screen_rect, window_class, HostKind,
};
use server_supervisor_lib::supervisor::window::input::{send, InputAction};
use server_supervisor_lib::supervisor::window::{capture, embed, find_window, park, DockOutcome};
use spike_common::kill_tree;

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

fn pump_for(d: Duration) {
    use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE};
    let end = Instant::now() + d;
    while Instant::now() < end {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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
    host: isize,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        kill_tree(self.pid);
        destroy_host(self.host);
    }
}

#[test]
#[ignore]
fn spike_webview2_probe_accepts_posted_input() {
    let exe = probe_exe();
    let state_path = std::env::temp_dir().join(format!("webview2_probe_state_{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&state_path);

    // Same production sequencing as the Flutter leg: park armed and the pid
    // registered before the process exists.
    park::start();
    let child = Command::new(&exe).arg(&state_path).spawn().expect("spawn webview2_probe");
    let pid = child.id();
    park::add_parked_pid(pid);

    let found = find_window(pid, Duration::from_secs(15), false).expect("webview2_probe window never appeared");
    let guest = found.hwnd;

    // Class-based host choice, exactly as `dock::headless::try_dock_headless`
    // computes it: anything other than a Flutter runner window must stay on
    // the cheaper, zero-on-screen-footprint Offscreen path.
    let class = window_class(guest);
    let kind = host_kind_for_class(&class);
    assert_eq!(kind, HostKind::Offscreen, "a non-Flutter guest (class {class:?}) must get an Offscreen host");

    let (w, h) = (found.rect.width(), found.rect.height());
    let (hw, hh) = if w >= 320 && h >= 320 { (w, h) } else { (1280, 800) };
    let host = create_host(kind, hw, hh).expect("create_host");
    let _cleanup = Cleanup { pid, host };

    let target = screen_rect(host);
    let (outcome, _original) = embed(guest, host, target).expect("embed() must not report the window gone");
    assert_eq!(
        outcome,
        DockOutcome::Embedded,
        "SetParent into the offscreen host failed; soft-dock is not acceptable for headless"
    );
    pump_for(Duration::from_millis(1500));

    wait_for(Duration::from_secs(5), || read_state(&state_path)).expect("probe never wrote its initial state");

    // Click the button (fills the top 900x200 of the window). target_at
    // must walk down into the cross-process msedgewebview2.exe content
    // child for this to land at all.
    let g = guest;
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

    // Through the production capture path - no FLUTTERVIEW child exists, so
    // this must capture `guest` itself, proving the default path is
    // untouched by the Flutter-only redirect in `capture::capture`.
    let shot = capture::capture(g).expect("capture the embedded webview2 guest");
    let colors: std::collections::HashSet<(u8, u8, u8)> =
        shot.rgba.chunks_exact(4).map(|p| (p[0], p[1], p[2])).collect();
    if let Some(dir) = std::env::var_os("SPIKE_OUT").map(PathBuf::from) {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(png) = shot.to_png() {
            let _ = std::fs::write(dir.join("webview2_probe.png"), png);
        }
    }
    assert!(colors.len() > 4, "screenshot of the webview2 probe through the production capture path was blank");

    println!(
        "webview2 leg: click landed (count={count}), text landed ({text:?}), screenshot non-blank ({} colors)",
        colors.len()
    );
}
