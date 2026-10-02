//! Spike leg for todo 0057: proves posted input
//! (`supervisor::window::input`) drives a Flutter Windows app, AND (this
//! revision) drives the actual production headless-host/embed/capture path
//! (`supervisor::window::headless_host`, `::place::embed`,
//! `::capture::capture`) end to end against it - not a test-only stand-in.
//! Flutter takes mouse input only on its inner FLUTTERVIEW child, never the
//! runner window, and reads keys through its own key embedder; its GPU
//! swapchain also stops presenting into a fully off-screen host, which is
//! why production picks a DWM-cloaked, on-screen host for this window class
//! (see `supervisor::window::headless_host`'s module doc).
//!
//! The probe lives in `spikes/flutter_input_probe`. Build it first:
//!   flutter build windows --release
//! (run from `spikes/flutter_input_probe`, or with --target absolute path).
//!
//! Run this leg with:
//!   cargo test --test window_flutter_spike -- --ignored --nocapture --test-threads=1
//! Set `SPIKE_OUT` to a directory to also write a PNG of the capture.
#![cfg(windows)]

mod spike_common;

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect, WindowFromPoint};

use server_supervisor_lib::supervisor::window::headless_host::{
    create_host, destroy_host, find_flutterview_child, host_kind_for_class, screen_rect, window_class, HostKind,
};
use server_supervisor_lib::supervisor::window::input::{send, InputAction};
use server_supervisor_lib::supervisor::window::{capture, embed, find_window, park, DockOutcome};
use spike_common::kill_tree;

fn probe_exe() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let exe = manifest.join("../spikes/flutter_input_probe/build/windows/x64/runner/Release/flutter_input_probe.exe");
    if exe.is_file() {
        return exe;
    }
    panic!(
        "flutter_input_probe.exe not built. Run (from spikes/flutter_input_probe):\n  \
         flutter build windows --release"
    );
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

/// Kills the probe's process tree and destroys the offscreen host even if
/// an assert panics mid-test.
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
fn spike_flutter_probe_accepts_posted_input() {
    let exe = probe_exe();
    let state_path = std::env::temp_dir().join(format!("flutter_probe_state_{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&state_path);

    // Mirrors production `proc::spawn`'s own sequencing for a
    // headless-flagged command exactly: the hook is armed and the pid is
    // registered as "park this pid's new top-level windows" BEFORE the
    // process exists, so the runner's very first window is parked at
    // -32000 the instant it is created - never found by `find_window` at
    // its real, on-screen spawn position.
    park::start();
    let child = Command::new(&exe).arg(&state_path).spawn().expect("spawn flutter_input_probe");
    let pid = child.id();
    park::add_parked_pid(pid);

    // Same production entry point `dock::headless` uses: polls for the
    // guest's real main window, applying the same accept/reject policy
    // (`find.rs`'s `evaluate`/`pick_best`), not a test-only stand-in.
    let found = find_window(pid, Duration::from_secs(15), false).expect("flutter_input_probe window never appeared");
    let guest = found.hwnd;

    // Class-based host choice, exactly as `dock::headless::try_dock_headless`
    // computes it.
    let class = window_class(guest);
    assert_eq!(class, "FLUTTER_RUNNER_WIN32_WINDOW", "unexpected runner window class: {class:?}");
    let kind = host_kind_for_class(&class);
    assert_eq!(kind, HostKind::Cloaked, "a Flutter guest must get a Cloaked host, not Offscreen");

    let (w, h) = (found.rect.width(), found.rect.height());
    let (hw, hh) = if w >= 320 && h >= 320 { (w, h) } else { (1280, 800) };
    let host = create_host(kind, hw, hh).expect("create_host");
    let _cleanup = Cleanup { pid, host };

    let target = screen_rect(host);
    let (outcome, _original) = embed(guest, host, target).expect("embed() must not report the window gone");
    assert_eq!(
        outcome,
        DockOutcome::Embedded,
        "SetParent into the cloaked host failed; soft-dock is not acceptable for headless"
    );
    pump_for(Duration::from_millis(1500));

    // --- Click-through measurement: a point inside the cloaked host's own
    // screen rect must not resolve (via WindowFromPoint) to the host or the
    // now-embedded guest, or the dev's real clicks over that screen area
    // would be silently eaten by an invisible window.
    let probe_point = POINT { x: (target.left + target.right) / 2, y: (target.top + target.bottom) / 2 };
    let hit = unsafe { WindowFromPoint(probe_point) };
    let hit_isize = hit.0 as isize;
    println!(
        "WindowFromPoint({},{}) = {hit_isize:#x} (host={host:#x}, guest={guest:#x})",
        probe_point.x, probe_point.y
    );
    let eats_clicks = hit_isize == host || hit_isize == guest;
    println!("click-through check: cloaked host/guest intercepts WindowFromPoint = {eats_clicks}");

    // --- FLUTTERVIEW offset measurement: `/input` posts clicks in the
    // guest's own client pixels; `capture` must hand back an image in that
    // same space. Confirms FLUTTERVIEW sits at (0,0) in the guest's client
    // area and is the same size, so redirecting `capture` to it changes
    // which HWND gets drawn, never the coordinate numbers.
    let flutterview = find_flutterview_child(guest).expect("FLUTTERVIEW child not found");
    let guest_hwnd = HWND(guest as *mut _);
    let mut guest_client = RECT::default();
    unsafe {
        let _ = GetClientRect(guest_hwnd, &mut guest_client);
    }
    let mut fv_screen = RECT::default();
    unsafe {
        let _ = GetWindowRect(HWND(flutterview as *mut _), &mut fv_screen);
    }
    let mut guest_screen = RECT::default();
    unsafe {
        let _ = GetWindowRect(guest_hwnd, &mut guest_screen);
    }
    let fv_offset = (fv_screen.left - guest_screen.left, fv_screen.top - guest_screen.top);
    let fv_size = (fv_screen.right - fv_screen.left, fv_screen.bottom - fv_screen.top);
    let guest_client_size = (guest_client.right - guest_client.left, guest_client.bottom - guest_client.top);
    println!(
        "FLUTTERVIEW offset in guest client space = {fv_offset:?}, FLUTTERVIEW size = {fv_size:?}, guest client size = {guest_client_size:?}"
    );
    assert_eq!(fv_offset, (0, 0), "FLUTTERVIEW must sit at (0,0) in the guest's client area for /input and capture to share a coordinate space");
    assert_eq!(fv_size, guest_client_size, "FLUTTERVIEW must fill the guest's whole client area");

    wait_for(Duration::from_secs(5), || read_state(&state_path)).expect("probe never wrote its initial state");

    // Click the button filling the top 900x200: target_at must walk down
    // past the runner window into the FLUTTERVIEW child, since Flutter
    // takes mouse input only there.
    let g = guest;
    send(g, &InputAction::Click { x: 450, y: 100, button: Default::default(), double: false }).expect("posted click");
    let (count, _) = wait_for(Duration::from_secs(5), || read_state(&state_path).filter(|(c, _)| *c > 0))
        .expect("click never landed: count stayed 0");
    assert_eq!(count, 1, "button click count");

    // Click into the text field, then type. Flutter's focus is tracked by
    // its own framework (FocusNode), not OS focus, so a tap is expected to
    // be enough for the field to start accepting WM_CHAR.
    send(g, &InputAction::Click { x: 300, y: 225, button: Default::default(), double: false })
        .expect("posted click into field");
    pump_for(Duration::from_millis(300));
    send(g, &InputAction::Text { text: "hi0057".into() }).expect("posted text");
    let (_, text) = wait_for(Duration::from_secs(5), || read_state(&state_path).filter(|(_, t)| !t.is_empty()))
        .expect("typed text never landed");
    assert_eq!(text, "hi0057", "typed text");

    // Through the production capture path: `capture::capture` must detect
    // the FLUTTERVIEW child itself and redirect to it.
    let shot = capture::capture(g).expect("capture the embedded Flutter guest");
    let colors: std::collections::HashSet<(u8, u8, u8)> =
        shot.rgba.chunks_exact(4).map(|p| (p[0], p[1], p[2])).collect();
    if let Some(dir) = std::env::var_os("SPIKE_OUT").map(PathBuf::from) {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(png) = shot.to_png() {
            let _ = std::fs::write(dir.join("flutter_probe.png"), png);
        }
    }
    assert!(colors.len() > 4, "screenshot of the flutter probe through the production capture path was blank");

    println!(
        "flutter leg: click landed (count={count}), text landed ({text:?}), screenshot non-blank ({} colors), \
         click-through eats_clicks={eats_clicks}",
        colors.len()
    );
}
