//! Agent-facing "every window" read path for `GET /procs/:id/windows` and the
//! `window=` validation `/screenshot`/`/input` use, split out of `find.rs` to
//! keep that file under its line budget. `find.rs`'s own `find_window`/
//! `find_window_once` apply a strict "is this the one real main window" policy
//! (see that file's module doc); this is the deliberately unfiltered
//! counterpart - a dialog or popup IS the owned, small, possibly-toolwindow
//! kind of window that policy exists to reject, and this is exactly what an
//! agent needs to reach one that `window::park` moved off-screen (todo 0056).

use super::ffi::{self, GA_ROOT, Rect};
use super::find::{enum_proc, is_window_alive, window_title, EnumResult};
use super::headless_host::window_class;
use std::collections::HashSet;

/// The pid owning `hwnd`, or `None` for a stale/zero handle - used to check a
/// caller-supplied hwnd actually belongs to the proc's own pid tree before
/// the API acts on it.
pub fn hwnd_pid(hwnd: isize) -> Option<u32> {
    if hwnd == 0 || !is_window_alive(hwnd) {
        return None;
    }
    let mut pid: u32 = 0;
    unsafe {
        ffi::GetWindowThreadProcessId(hwnd as ffi::HWND, &mut pid);
    }
    (pid != 0).then_some(pid)
}

/// One visible top-level window belonging to a pid tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSummary {
    pub hwnd: isize,
    pub title: String,
    pub class: String,
    pub rect: Rect,
}

/// Every visible top-level window owned by any pid in `tree`. One
/// `EnumWindows` pass, reusing `find.rs`'s own enumeration shell minus its
/// accept/reject policy.
pub fn list_windows_of(tree: &HashSet<u32>) -> Vec<WindowSummary> {
    let mut found = EnumResult { hwnds: Vec::new() };
    unsafe {
        ffi::EnumWindows(enum_proc, &mut found as *mut EnumResult as ffi::LPARAM);
    }
    found
        .hwnds
        .into_iter()
        .filter_map(|hwnd| {
            let mut pid: u32 = 0;
            unsafe {
                ffi::GetWindowThreadProcessId(hwnd, &mut pid);
            }
            if !tree.contains(&pid) || unsafe { ffi::IsWindowVisible(hwnd) } == 0 {
                return None;
            }
            // Top-level only: EnumWindows already only enumerates top-level
            // windows, but GetAncestor(GA_ROOT) is the same owned-popup-vs-
            // real-top-level distinction `find.rs`'s `evaluate` draws via its
            // HasOwner rule - kept here as a defensive filter, not a
            // correctness fix, since EnumWindows's own contract already
            // guarantees this.
            if unsafe { ffi::GetAncestor(hwnd, GA_ROOT) } != hwnd {
                return None;
            }
            let mut rect = Rect::default();
            unsafe {
                ffi::GetWindowRect(hwnd, &mut rect);
            }
            Some(WindowSummary {
                hwnd: hwnd as isize,
                title: window_title(hwnd),
                class: window_class(hwnd as isize),
                rect,
            })
        })
        .collect()
}
