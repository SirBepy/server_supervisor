//! Tauri-free core of the headless host window: choose `Offscreen` vs
//! `Cloaked` placement from the guest's window class, create/destroy the
//! host, and find a Flutter guest's `FLUTTERVIEW` child for `capture` to
//! target. Kept free of any `tauri::` type for the same reason
//! `window::park` is (see that module's own doc comment for the full
//! story): `tests/window_flutter_spike.rs` and `tests/window_webview2_spike.rs`
//! call these functions directly from a plain console test binary with no
//! WebView2 runtime alongside it, and naming a Tauri type anywhere reachable
//! from that binary pulls in the windowing runtime and aborts pre-main
//! (STATUS_ENTRYPOINT_NOT_FOUND).
//!
//! `dock::headless` is the only production caller; it wraps `create_host`
//! with the `on_main`/`AppHandle` plumbing Win32 window creation needs to
//! run on Tauri's main thread. The decision this module exists for: Flutter's
//! Windows embedding presents its GPU swapchain straight into the
//! `FLUTTERVIEW` child, and treats a fully off-screen host as occluded,
//! which stops that swapchain from presenting at all (`PrintWindow` then
//! comes back blank even though posted input still lands) - confirmed by
//! `tests/window_flutter_spike.rs` capturing blank with an off-screen host
//! and real content with a DWM-cloaked one, same guest, only the host kind
//! changed. Chromium/WebView2 keep presenting off-screen, so they stay on
//! the simpler, zero-on-screen-footprint `Offscreen` path.

use std::sync::OnceLock;

use windows::core::{w, BOOL};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, EnumChildWindows, GetClassNameW, GetSystemMetrics,
    GetWindowRect, RegisterClassW, SetWindowPos, ShowWindow, HWND_BOTTOM, SM_CXSCREEN, SM_CYSCREEN,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNOACTIVATE, WNDCLASSW, WS_CLIPCHILDREN,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use super::ffi::Rect;

/// Far outside any monitor layout, so an `Offscreen` host is never on
/// screen. Mirrors `window::park::PARK_AT` - duplicated rather than shared
/// for the same "no cross-module dependency" reason given in that file.
pub const PARK_AT: i32 = -32_000;

/// A Flutter runner window's class name, exactly as `GetClassNameW` reports
/// it. Any other class (WebView2/tao, a plain Win32 app, ...) gets the
/// default `Offscreen` host.
const FLUTTER_RUNNER_CLASS: &str = "FLUTTER_RUNNER_WIN32_WINDOW";

/// How a headless host hides itself from the dev.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKind {
    /// Parked at `PARK_AT`: cheapest, works for every guest whose renderer
    /// keeps presenting while occluded (Chromium, WebView2).
    Offscreen,
    /// On-screen but DWM-cloaked (invisible, skipped by Alt-Tab/taskbar):
    /// what a Flutter guest needs to keep its swapchain presenting.
    Cloaked,
}

/// Pure policy, unit-tested without touching Win32.
pub fn host_kind_for_class(class: &str) -> HostKind {
    if class == FLUTTER_RUNNER_CLASS {
        HostKind::Cloaked
    } else {
        HostKind::Offscreen
    }
}

/// `GetClassNameW` on a live window, or an empty string for a stale handle -
/// an empty string is never `FLUTTER_RUNNER_CLASS`, so it falls through to
/// the safe `Offscreen` default rather than erroring.
pub fn window_class(hwnd: isize) -> String {
    let hwnd = HWND(hwnd as *mut _);
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

unsafe extern "system" fn host_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(h, m, w, l) }
}

fn primary_screen_size() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

/// Main thread only: a window's messages are dispatched by the thread that
/// created it, and only the caller's own main-thread pump (Tauri's in
/// production, a manual `pump_for` loop in the spikes) processes them.
pub fn create_host(kind: HostKind, width: i32, height: i32) -> Result<isize, String> {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    let class = w!("ServerSupervisorHeadlessHost");
    unsafe {
        let hinst = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        REGISTERED.get_or_init(|| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(host_proc),
                hInstance: hinst.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&wc);
        });
        let (x, y) = match kind {
            HostKind::Offscreen => (PARK_AT, PARK_AT),
            // Centered on the primary monitor: any on-screen rect satisfies
            // the DXGI "not fully occluded" check the cloak exists for, and
            // centering keeps it away from screen-edge UI (taskbar, window
            // snap zones) in case cloak ever stops masking hit-testing.
            HostKind::Cloaked => {
                let (sw, sh) = primary_screen_size();
                ((sw - width).max(0) / 2, (sh - height).max(0) / 2)
            }
        };
        let host = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("server_supervisor headless host"),
            WS_POPUP | WS_CLIPCHILDREN,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(|e| format!("could not create the headless host: {e}"))?;
        if kind == HostKind::Cloaked {
            let on = BOOL(1);
            DwmSetWindowAttribute(host, DWMWA_CLOAK, &on as *const _ as _, 4)
                .map_err(|e| format!("DwmSetWindowAttribute(DWMWA_CLOAK): {e}"))?;
        }
        let _ = ShowWindow(host, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(host, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        Ok(host.0 as isize)
    }
}

pub fn destroy_host(host: isize) {
    unsafe {
        let _ = DestroyWindow(HWND(host as *mut _));
    }
}

pub fn screen_rect(host: isize) -> Rect {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(HWND(host as *mut _), &mut r);
    }
    Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

/// The `FLUTTERVIEW` descendant of `root`, if any. `EnumChildWindows` walks
/// every descendant (not just direct children), which matters here since
/// some Flutter versions nest an extra layer between the runner window and
/// `FLUTTERVIEW`.
pub fn find_flutterview_child(root: isize) -> Option<isize> {
    // A null/zero hwnd is not "no parent" here, just a stale handle - passing
    // it through to EnumChildWindows would enumerate the whole desktop
    // (NULL means "equivalent to EnumWindows" per MSDN) instead of failing
    // fast the way `capture`'s own stale-handle check expects.
    if root == 0 {
        return None;
    }
    struct Ctx {
        found: Option<HWND>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let ctx = unsafe { &mut *(lp.0 as *mut Ctx) };
        if window_class(hwnd.0 as isize) == "FLUTTERVIEW" {
            ctx.found = Some(hwnd);
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut ctx = Ctx { found: None };
    unsafe {
        let _ = EnumChildWindows(Some(HWND(root as *mut _)), Some(cb), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.found.map(|h| h.0 as isize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flutter_runner_class_gets_a_cloaked_host() {
        assert_eq!(host_kind_for_class("FLUTTER_RUNNER_WIN32_WINDOW"), HostKind::Cloaked);
    }

    #[test]
    fn any_other_class_gets_an_offscreen_host() {
        assert_eq!(host_kind_for_class("Chrome_WidgetWin_1"), HostKind::Offscreen);
        assert_eq!(host_kind_for_class(""), HostKind::Offscreen);
    }
}
