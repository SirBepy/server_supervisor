//! Spike leg for todo 0057: proves posted input
//! (`supervisor::window::input`) drives a Flutter Windows app. Flutter takes
//! mouse input only on its inner FLUTTERVIEW child, never the runner
//! window, and reads keys through its own key embedder - both unverified
//! against a real Flutter build before this.
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

/// Flutter's Windows embedding renders via its own GPU swapchain, presented
/// straight into the FLUTTERVIEW child - confirmed by a diagnostic run
/// showing `PrintWindow(PW_RENDERFULLCONTENT)` on the parent
/// FLUTTER_RUNNER_WIN32_WINDOW coming back blank even though the posted
/// click and posted text both land. Capture has to target FLUTTERVIEW
/// itself; this is a capture-only fix, not an input.rs one, since input
/// delivery already worked against the parent via `target_at`.
fn find_flutterview(root: HWND) -> HWND {
    use windows::core::BOOL;
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW};

    struct Ctx {
        found: Option<HWND>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let ctx = unsafe { &mut *(lp.0 as *mut Ctx) };
        let mut buf = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        if String::from_utf16_lossy(&buf[..len as usize]) == "FLUTTERVIEW" {
            ctx.found = Some(hwnd);
        }
        BOOL(1)
    }
    let mut ctx = Ctx { found: None };
    unsafe {
        let _ = EnumChildWindows(Some(root), Some(cb), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.found.unwrap_or(root)
}

#[test]
#[ignore]
fn spike_flutter_probe_accepts_posted_input() {
    let exe = probe_exe();
    let state_path = std::env::temp_dir().join(format!("flutter_probe_state_{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&state_path);

    let child = Command::new(&exe).arg(&state_path).spawn().expect("spawn flutter_input_probe");
    let pid = child.id();
    // HostKind::Offscreen positions the host far off the desktop; Flutter's
    // DXGI swapchain treats that as fully occluded and stops presenting
    // frames (unlike Chromium/WebView2, confirmed by a diagnostic run: the
    // capture came back blank even targeting FLUTTERVIEW directly, with
    // posted input still landing). Cloaked keeps the host on-screen at a
    // normal position but DWM-hidden (DWMWA_CLOAK set before the first
    // ShowWindow), so the dev never sees it while the swapchain still
    // thinks it is visible.
    let host = create_host(HostKind::Cloaked);
    let _cleanup = Cleanup { pid, host };

    let guest = find_window(pid, Duration::from_secs(15)).expect("flutter_input_probe window never appeared");
    let embedded = embed(guest, host);
    assert!(embedded, "SetParent into the offscreen host failed");
    pump_for(Duration::from_millis(1500));

    wait_for(Duration::from_secs(5), || read_state(&state_path)).expect("probe never wrote its initial state");

    // Click the button filling the top 900x200: target_at must walk down
    // past the runner window into the FLUTTERVIEW child, since Flutter
    // takes mouse input only there.
    let g = guest.0 as isize;
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

    let flutterview = find_flutterview(guest);
    let frame = grab(Some(flutterview), RECT::default());
    frame.save("flutter_probe"); // saved before the assert so a failure still leaves evidence
    assert!(frame.non_blank(), "screenshot of the flutter probe's FLUTTERVIEW child was blank");

    println!("flutter leg: click landed (count={count}), text landed ({text:?}), screenshot non-blank");
}
