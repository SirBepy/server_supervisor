//! Shared by `window_spike.rs` (via `window_capture`): creating an invisible
//! host window in one of several DWM/layering configurations, finding a
//! guest process's top-level window, and reparenting it in. Split out of
//! the former `tests/media_spike.rs` (todo 0060).
#![allow(dead_code)]

use std::collections::HashSet;
use std::time::{Duration, Instant};

use windows::core::BOOL;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{SetThreadDpiHostingBehavior, DPI_HOSTING_BEHAVIOR_MIXED};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::spike_common::process_tree;

pub(crate) const HOST_W: i32 = 900;
pub(crate) const HOST_H: i32 = 600;

unsafe extern "system" fn host_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(h, m, w, l) }
}

pub(crate) fn pump_for(d: Duration) {
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

#[derive(Clone, Copy, Debug)]
pub(crate) enum HostKind {
    Visible,
    Offscreen,
    Cloaked,
    Layered,
}

pub(crate) fn create_host(kind: HostKind) -> HWND {
    unsafe {
        let class = windows::core::w!("MediaSpikeHost");
        let hinst = GetModuleHandleW(None).expect("module handle");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(host_proc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let mut ex = WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        if matches!(kind, HostKind::Layered) {
            ex |= WS_EX_LAYERED | WS_EX_TRANSPARENT;
        }
        let (x, y) = match kind {
            HostKind::Offscreen => (-20_000, -20_000),
            _ => (120, 120),
        };
        let host = CreateWindowExW(
            ex,
            class,
            windows::core::w!("media spike host"),
            WS_POPUP | WS_CLIPCHILDREN,
            x,
            y,
            HOST_W,
            HOST_H,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .expect("CreateWindowExW host");
        match kind {
            HostKind::Cloaked => {
                let on = BOOL(1);
                DwmSetWindowAttribute(host, DWMWA_CLOAK, &on as *const _ as _, 4).expect("cloak");
            }
            HostKind::Layered => {
                SetLayeredWindowAttributes(host, COLORREF(0), 1, LWA_ALPHA).expect("layered alpha");
            }
            _ => {}
        }
        let _ = ShowWindow(host, SW_SHOWNOACTIVATE);
        if !matches!(kind, HostKind::Visible) {
            let _ = SetWindowPos(host, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        host
    }
}

struct FindCtx {
    pids: HashSet<u32>,
    best: Option<(HWND, i32)>,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
    unsafe {
        let ctx = &mut *(lp.0 as *mut FindCtx);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if !ctx.pids.contains(&pid) || !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return BOOL(1);
        }
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w >= 32 && h >= 32 && ctx.best.is_none_or(|(_, a)| w * h > a) {
            ctx.best = Some((hwnd, w * h));
        }
        BOOL(1)
    }
}

pub(crate) fn find_window(root: u32, timeout: Duration) -> Option<HWND> {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        let mut ctx = FindCtx { pids: process_tree(root), best: None };
        unsafe {
            let _ = EnumWindows(Some(enum_cb), LPARAM(&mut ctx as *mut _ as isize));
        }
        if let Some((h, _)) = ctx.best {
            return Some(h);
        }
        pump_for(Duration::from_millis(100));
    }
    None
}

pub(crate) fn embed(guest: HWND, host: HWND) -> bool {
    unsafe {
        let _ = SetThreadDpiHostingBehavior(DPI_HOSTING_BEHAVIOR_MIXED);
        let style = GetWindowLongPtrW(guest, GWL_STYLE) as u32;
        let new_style = (style & !WS_OVERLAPPEDWINDOW.0 & !WS_POPUP.0) | WS_CHILD.0;
        SetWindowLongPtrW(guest, GWL_STYLE, new_style as isize);
        let ok = SetParent(guest, Some(host)).is_ok();
        let _ = SetWindowPos(guest, None, 0, 0, HOST_W, HOST_H, SWP_FRAMECHANGED | SWP_SHOWWINDOW | SWP_NOACTIVATE);
        ok
    }
}
