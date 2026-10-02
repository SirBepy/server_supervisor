//! Hand-declared Win32 primitives, mirroring `tests/embed_spike.rs`'s own
//! choice not to pull in the `windows` crate (a heavy codegen-metadata
//! dependency) for the roughly dozen functions this module actually calls.
//! That tradeoff was made for the spike and carries over unchanged here:
//! this file adds zero new dependencies.
//!
//! Only the declarations `find.rs` and `place.rs` need survive; the spike's
//! screenshot, BMP-writing and PrintWindow machinery stops at the test
//! harness and is not repeated here.

#![allow(clippy::upper_case_acronyms)]

use std::ffi::c_void;

pub(super) type HWND = *mut c_void;
pub(super) type HANDLE = *mut c_void;
pub(super) type BOOL = i32;
pub(super) type LPARAM = isize;

/// Win32's `RECT`, laid out to match exactly (`repr(C)`, four `i32`s) since
/// it is passed by pointer directly into `GetWindowRect` / `SetWindowPos`
/// call sites. Doubles as this module's public rect type so callers never
/// see a second, converted copy.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

#[repr(C)]
pub(super) struct Processentry32W {
    pub dw_size: u32,
    pub cnt_usage: u32,
    pub th32_process_id: u32,
    pub th32_default_heap_id: usize,
    pub th32_module_id: u32,
    pub cnt_threads: u32,
    pub th32_parent_process_id: u32,
    pub pc_pri_class_base: i32,
    pub dw_flags: u32,
    pub sz_exe_file: [u16; 260],
}

pub(super) const WS_OVERLAPPEDWINDOW: u32 = 0x00CF0000;
pub(super) const WS_CHILD: u32 = 0x4000_0000;
pub(super) const WS_POPUP: u32 = 0x8000_0000;
pub(super) const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
pub(super) const WS_EX_TRANSPARENT: u32 = 0x0000_0020;
pub(super) const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
pub(super) const SW_SHOW: i32 = 5;
pub(super) const SW_HIDE: i32 = 0;
// Shows the window without activating it, i.e. it does not steal foreground
// from whatever window currently holds it. Used by `focus::guard_focus` on a
// just-launched child's window, on the child's FIRST paint only - this is
// paired with restoring the pre-spawn foreground window immediately after.
pub(super) const SW_SHOWNOACTIVATE: i32 = 4;
pub(super) const GWL_STYLE: i32 = -16;
pub(super) const GWL_EXSTYLE: i32 = -20;
pub(super) const GW_OWNER: u32 = 4;
// A real application main window is never this small, but 200px would
// reject legitimate narrow HUD-style windows (a 200x100 or 468x48 utility
// window, confirmed live). 32 sits above the 16x16 helper surface that
// caused the original wrong-window bug and below the smallest real window
// dimension seen, separating the two without re-admitting the helper.
pub(super) const MIN_MAIN_WINDOW_DIM: i32 = 32;
pub(super) const SWP_NOZORDER: u32 = 0x0004;
pub(super) const SWP_FRAMECHANGED: u32 = 0x0020;
pub(super) const SWP_SHOWWINDOW: u32 = 0x0040;
pub(super) const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
// DPI_HOSTING_BEHAVIOR_MIXED. Since Windows 10 1607, SetParent fails
// outright when the parent and child threads have different DPI awareness
// contexts unless the parent's thread opts into mixed-mode hosting first.
// Proven in tests/embed_spike.rs to be the first thing to check if
// SetParent starts failing.
pub(super) const DPI_HOSTING_BEHAVIOR_MIXED: i32 = 1;

#[link(name = "user32")]
extern "system" {
    pub(super) fn EnumWindows(
        lp_enum_func: unsafe extern "system" fn(HWND, LPARAM) -> BOOL,
        l_param: LPARAM,
    ) -> BOOL;
    pub(super) fn GetWindowThreadProcessId(hwnd: HWND, lpdw_process_id: *mut u32) -> u32;
    pub(super) fn IsWindowVisible(hwnd: HWND) -> BOOL;
    pub(super) fn IsWindow(hwnd: HWND) -> BOOL;
    pub(super) fn GetWindowRect(hwnd: HWND, lp_rect: *mut Rect) -> BOOL;
    pub(super) fn GetWindow(hwnd: HWND, u_cmd: u32) -> HWND;
    pub(super) fn GetParent(hwnd: HWND) -> HWND;
    pub(super) fn SetParent(h_wnd_child: HWND, h_wnd_new_parent: HWND) -> HWND;
    pub(super) fn GetWindowLongPtrW(hwnd: HWND, n_index: i32) -> isize;
    pub(super) fn SetWindowLongPtrW(hwnd: HWND, n_index: i32, dw_new_long: isize) -> isize;
    pub(super) fn SetWindowPos(
        hwnd: HWND,
        h_wnd_insert_after: HWND,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        u_flags: u32,
    ) -> BOOL;
    pub(super) fn ShowWindow(hwnd: HWND, n_cmd_show: i32) -> BOOL;
    pub(super) fn MapWindowPoints(
        h_wnd_from: HWND,
        h_wnd_to: HWND,
        lp_points: *mut Rect,
        c_points: u32,
    ) -> i32;
    pub(super) fn SetThreadDpiHostingBehavior(value: i32) -> i32;
    pub(super) fn GetWindowTextW(hwnd: HWND, lp_string: *mut u16, n_max_count: i32) -> i32;
    pub(super) fn GetForegroundWindow() -> HWND;
    // Can legitimately fail: Windows only grants this to a process that
    // already holds (or was started by) the foreground process, and even
    // then can still refuse it (an open menu, ALT held, etc). Callers must
    // treat a zero return as an expected, non-fatal outcome, never a bug.
    pub(super) fn SetForegroundWindow(hwnd: HWND) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    pub(super) fn GetLastError() -> u32;
    pub(super) fn CloseHandle(h_object: HANDLE) -> BOOL;
    pub(super) fn CreateToolhelp32Snapshot(dw_flags: u32, th32_process_id: u32) -> HANDLE;
    pub(super) fn Process32FirstW(h_snapshot: HANDLE, lppe: *mut Processentry32W) -> BOOL;
    pub(super) fn Process32NextW(h_snapshot: HANDLE, lppe: *mut Processentry32W) -> BOOL;
}
