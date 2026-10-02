//! Spike for todo 0056: does `window::park`'s `SetWinEventHook` park a
//! headless app's startup dialog off-screen before the dev ever sees it, is
//! it still screenshot-able and clickable while parked, and how long (if
//! any) was it actually visible on a real monitor first?
//!
//! The only `#[test]` here is `#[ignore]`: it spawns a real GUI dialog and
//! pokes real windows. Run with:
//!   $env:CARGO_TARGET_DIR='D:/cargo-target/server_supervisor__src-tauri'
//!   cargo test --manifest-path src-tauri/Cargo.toml --test window_popup_spike -- --ignored --nocapture --test-threads=1

#![cfg(windows)]

use std::collections::HashSet;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use server_supervisor_lib::supervisor::window::{capture, input, park};
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{MonitorFromRect, ScreenToClient, MONITOR_DEFAULTTONULL};
use windows::Win32::UI::WindowsAndMessaging::*;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Matches `window::park`'s own park coordinate - this spike asserts against
/// the production constant's effect, not a copy of its value's meaning.
const PARK_AT: i32 = -32_000;

/// Kills the guest's whole process tree on drop, including an early
/// return/panic - a leaked `powershell.exe` with a live WPF dialog is exactly
/// the kind of orphan this repo's process-hygiene rules exist to prevent.
struct GuestGuard(u32);
impl Drop for GuestGuard {
    fn drop(&mut self) {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &self.0.to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
}

struct Found {
    hwnd: HWND,
    rect: RECT,
    visible: bool,
}

struct FindCtx {
    pid: u32,
    found: Option<Found>,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
    unsafe {
        let ctx = &mut *(lp.0 as *mut FindCtx);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != ctx.pid {
            return BOOL(1);
        }
        // Unowned only: a plain `MessageBox.Show(text)` with no owner window
        // is itself the top-level dialog this spike is looking for.
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return BOOL(1);
        }
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        ctx.found = Some(Found { hwnd, rect: r, visible: IsWindowVisible(hwnd).as_bool() });
        BOOL(0) // found it, stop enumerating
    }
}

fn find_once(pid: u32) -> Option<Found> {
    let mut ctx = FindCtx { pid, found: None };
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.found
}

fn on_a_monitor(r: &RECT) -> bool {
    unsafe { !MonitorFromRect(r, MONITOR_DEFAULTTONULL).is_invalid() }
}

fn is_parked(r: &RECT) -> bool {
    r.left == PARK_AT && r.top == PARK_AT
}

fn save_png(bytes: &[u8]) {
    let dir = std::path::Path::new(".for_bepy/screenshots/48576-134354254616522473");
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join("popup_spike.png"), bytes);
}

/// Cheap proxy for "this capture shows real rendered content", not a
/// solid/blank window - mirrors `window_capture::Frame::non_blank`'s own
/// reasoning without depending on that off-limits-to-edit module.
fn distinct_colors(rgba: &[u8]) -> usize {
    rgba.chunks_exact(4).map(|p| (p[0], p[1], p[2])).collect::<HashSet<_>>().len()
}

/// Center, in `dialog`'s own client coordinates (what `input::send` expects),
/// of its one "Button" class child - the OK button a plain
/// `MessageBox.Show(text)` creates.
fn ok_button_client_center(dialog: HWND) -> Option<(i32, i32)> {
    struct Ctx {
        button: Option<HWND>,
    }
    unsafe extern "system" fn child_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        unsafe {
            let ctx = &mut *(lp.0 as *mut Ctx);
            let mut buf = [0u16; 64];
            let len = GetClassNameW(hwnd, &mut buf);
            if len > 0 && String::from_utf16_lossy(&buf[..len as usize]) == "Button" {
                ctx.button = Some(hwnd);
                return BOOL(0);
            }
            BOOL(1)
        }
    }
    let mut ctx = Ctx { button: None };
    unsafe {
        let _ = EnumChildWindows(Some(dialog), Some(child_cb), LPARAM(&mut ctx as *mut _ as isize));
    }
    let button = ctx.button?;
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(button, &mut r);
    }
    let mut top_left = POINT { x: r.left, y: r.top };
    let mut bottom_right = POINT { x: r.right, y: r.bottom };
    unsafe {
        let _ = ScreenToClient(dialog, &mut top_left);
        let _ = ScreenToClient(dialog, &mut bottom_right);
    }
    Some(((top_left.x + bottom_right.x) / 2, (top_left.y + bottom_right.y) / 2))
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
#[ignore]
fn headless_popup_is_parked_screenshot_able_and_clickable() {
    park::start();

    let mut child = Command::new("powershell")
        .args([
            "-NoProfile",
            "-WindowStyle",
            "Hidden",
            "-Command",
            "Add-Type -AssemblyName PresentationFramework; \
             [System.Windows.MessageBox]::Show('popup spike')",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("spawn powershell guest");
    let pid = child.id();
    let _guard = GuestGuard(pid);

    // Registered before the dialog exists, exactly like `proc::spawn` does
    // for a headless-flagged command (see `spawn.rs`'s own
    // `park::add_parked_pid` call) - this is the real mechanism under test,
    // not a shortcut around it.
    park::add_parked_pid(pid);

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut flashed_at: Option<Instant> = None;
    let mut parked_at: Option<Instant> = None;
    let mut dialog: Option<HWND> = None;
    while Instant::now() < deadline && parked_at.is_none() {
        if let Some(found) = find_once(pid) {
            dialog = Some(found.hwnd);
            if found.visible && flashed_at.is_none() && on_a_monitor(&found.rect) {
                flashed_at = Some(Instant::now());
            }
            if is_parked(&found.rect) {
                parked_at = Some(Instant::now());
            }
        }
        // Tight poll: the whole point is sub-100ms resolution on a race this
        // repo's own 250ms `headless_tick` could never measure.
        std::thread::sleep(Duration::from_millis(2));
    }

    let dialog = dialog.expect("the guest's dialog window was never found within the timeout");
    match (flashed_at, parked_at) {
        (None, Some(_)) => {
            println!("RESULT: zero flash observed - EVENT_OBJECT_CREATE alone parked it before it ever touched a monitor");
        }
        (Some(f), Some(p)) => {
            println!(
                "RESULT: flashed on a monitor for {:?} before the hook parked it (EVENT_OBJECT_CREATE alone was NOT enough to prevent all visibility)",
                p.duration_since(f)
            );
        }
        (_, None) => println!("RESULT: dialog was never observed parked within the timeout - hook did not catch this window"),
    }

    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(dialog, &mut rect);
    }
    assert!(
        is_parked(&rect),
        "dialog must be parked at ({PARK_AT},{PARK_AT}), was at ({},{})",
        rect.left,
        rect.top
    );

    let shot = capture::capture(dialog.0 as isize).expect("capture an off-screen top-level window");
    let colors = distinct_colors(&shot.rgba);
    println!("capture {}x{}, {colors} distinct colors", shot.width, shot.height);
    assert!(colors > 4, "PrintWindow(PW_RENDERFULLCONTENT) must see real content, not a blank surface, while parked off-screen");
    save_png(&shot.to_png().expect("encode png"));

    let (bx, by) = ok_button_client_center(dialog).expect("find the dialog's OK button");
    input::send(dialog.0 as isize, &input::InputAction::Click { x: bx, y: by, button: Default::default(), double: false })
        .expect("post a click to the OK button");

    assert!(wait_for_exit(&mut child, Duration::from_secs(5)), "posted click did not close the dialog / exit the guest");
}
