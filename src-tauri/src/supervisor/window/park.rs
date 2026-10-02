//! Parks a headless proc's windows off-screen the instant they are created,
//! instead of waiting for `dock::headless`'s next poll tick to find and embed
//! them. A `SetWinEventHook(EVENT_OBJECT_CREATE..EVENT_OBJECT_SHOW)` delivered
//! out-of-context through a dedicated thread's own message queue (that is
//! what `WINEVENT_OUTOFCONTEXT` buys - no window of this process's own is
//! needed to receive it) sees every new top-level window from any process on
//! the machine; the callback only acts on one whose pid is in the shared
//! `parked pids` set `dock::headless` keeps in sync with which procs are
//! currently meant to run headless.
//!
//! This also catches popups and dialogs, not just the main window: a headless
//! app's "we are now syncing" dialog is a second top-level window owned by
//! the same pid tree, never a child of the docked window, so nothing short of
//! a hook on window creation can stop it from flashing on the real desktop
//! (see todo 0056). `dock::headless` never reparents a popup - a dialog owned
//! by the docked window must stay owned - so parking it off-screen in place is
//! the only option; `capture`'s `PrintWindow(PW_RENDERFULLCONTENT)` is what
//! lets an agent still see and `input` still drive it from there.
//!
//! Deliberately free of any `tauri::` type: `tests/window_popup_spike.rs`
//! links this module from a plain console test binary with no WebView2
//! runtime alongside it, and naming a Tauri type anywhere reachable from that
//! binary pulls in the windowing runtime and aborts pre-main
//! (STATUS_ENTRYPOINT_NOT_FOUND) - the same proven constraint `api.rs`'s
//! `DockFn`/`PermissionFlags` doc comments describe for `tests/api_test.rs`.

use super::ffi::Rect;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, HDC, HMONITOR,
    MONITORINFO, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetAncestor, GetMessageW, GetWindowThreadProcessId, IsWindow,
    SetWindowPos, TranslateMessage, CHILDID_SELF, EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, GA_ROOT,
    MSG, OBJID_WINDOW, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS,
};
use windows::core::BOOL;

/// Matches `dock::headless::PARK_AT` - both describe the same "nowhere a
/// monitor can be" offscreen coordinate, but this module must not depend on
/// `dock` (the hook fires for ANY process, long before `dock` decides what to
/// do about it), so the constant is duplicated rather than shared.
const PARK_AT: i32 = -32_000;

struct Parked {
    pid: u32,
    original: Rect,
}

fn parked() -> &'static Mutex<HashMap<isize, Parked>> {
    static PARKED: OnceLock<Mutex<HashMap<isize, Parked>>> = OnceLock::new();
    PARKED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pids() -> &'static Mutex<HashSet<u32>> {
    static PIDS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
    PIDS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn to_rect(r: RECT) -> Rect {
    Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

/// Starts the hook thread. Idempotent - safe to call from every place that
/// might need the hook running without coordinating who calls it first.
pub fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        std::thread::spawn(hook_thread);
    });
}

fn hook_thread() {
    unsafe {
        let hook = SetWinEventHook(
            EVENT_OBJECT_CREATE,
            EVENT_OBJECT_SHOW,
            None,
            Some(win_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        if hook.is_invalid() {
            log::warn!(
                "supervisor::window::park: SetWinEventHook failed; headless windows will only \
                 be parked on the next poll tick, not the instant they are created"
            );
            return;
        }
        // Out-of-context hooks are delivered through the INSTALLING thread's
        // own message queue - this loop is what actually invokes
        // `win_event_proc`, not a side effect of it.
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWinEvent(hook);
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if hwnd.is_invalid() || id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 {
        return;
    }
    if event != EVENT_OBJECT_CREATE && event != EVENT_OBJECT_SHOW {
        return;
    }
    unsafe {
        // Top-level only: a child control firing EVENT_OBJECT_SHOW (a button,
        // say) is not a window `dock::headless` or an agent's `/screenshot`
        // would ever treat as a separate target.
        if GetAncestor(hwnd, GA_ROOT) != hwnd {
            return;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || !pids().lock().unwrap().contains(&pid) {
            return;
        }
        park_window(hwnd, pid);
    }
}

/// Records `hwnd`'s current rect as its "original" (only the first time -
/// a second CREATE/SHOW for an already-parked window must never overwrite a
/// good recorded position with -32000) and moves it off-screen. Skips a
/// window that is already off every monitor: nothing to hide, and recording
/// -32000 as the "original" is exactly the bug this module exists to avoid.
fn park_window(hwnd: HWND, pid: u32) {
    let key = hwnd.0 as isize;
    {
        let guard = parked().lock().unwrap();
        if guard.contains_key(&key) {
            return;
        }
    }
    let mut rect = RECT::default();
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return;
        }
        if MonitorFromRect(&rect, MONITOR_DEFAULTTONULL).is_invalid() {
            return;
        }
    }
    parked().lock().unwrap().insert(key, Parked { pid, original: to_rect(rect) });
    unsafe {
        let _ = SetWindowPos(hwnd, None, PARK_AT, PARK_AT, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Replaces the whole set of pids whose new top-level windows get parked.
/// Called once per `headless_tick`, from the union of every currently
/// headless-wanted proc's full process tree - a plain replace (not a merge)
/// is correct because the tick already recomputed the complete, current
/// membership.
pub fn set_parked_pids(new_pids: HashSet<u32>) {
    *pids().lock().unwrap() = new_pids;
}

/// Adds one pid without waiting for the next tick's full replace - called the
/// moment a headless-flagged proc is spawned, since the gap between spawn and
/// the first tick (up to 250ms) is exactly the window this module exists to
/// close for a brand-new process's own first window.
pub fn add_parked_pid(pid: u32) {
    pids().lock().unwrap().insert(pid);
}

/// Every currently-parked hwnd belonging to any pid in `tree`, for an agent
/// route that needs to know what is reachable off-screen.
pub fn parked_windows_of(tree: &HashSet<u32>) -> Vec<isize> {
    parked().lock().unwrap().iter().filter(|(_, p)| tree.contains(&p.pid)).map(|(&h, _)| h).collect()
}

/// The true pre-park position of `hwnd`, if this module ever parked it. Used
/// by `dock::headless` to fix up the "original" an `embed()` snapshot would
/// otherwise capture AFTER this module already moved the window to -32000.
pub fn original_rect_of(hwnd: isize) -> Option<Rect> {
    parked().lock().unwrap().get(&hwnd).map(|p| p.original)
}

/// Restores every currently-parked window of any pid in `tree` to its
/// recorded original rect and forgets both the parked entry and the pid
/// (a later re-toggle gets a fresh park). Called when a proc's headless flag
/// turns off, so a popup left off-screen does not stay orphaned there.
pub fn unpark_all_of(tree: &HashSet<u32>) {
    let doomed: Vec<(isize, Rect)> = {
        let mut guard = parked().lock().unwrap();
        let keys: Vec<isize> = guard.iter().filter(|(_, p)| tree.contains(&p.pid)).map(|(&h, _)| h).collect();
        keys.into_iter().filter_map(|h| guard.remove(&h).map(|p| (h, p.original))).collect()
    };
    for (h, rect) in doomed {
        let hwnd = HWND(h as *mut _);
        if unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            unsafe {
                let _ = SetWindowPos(hwnd, None, rect.left, rect.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
    }
    pids().lock().unwrap().retain(|p| !tree.contains(p));
}

unsafe extern "system" fn monitor_enum_proc(_h: HMONITOR, _hdc: HDC, rect: *mut RECT, lparam: LPARAM) -> BOOL {
    unsafe {
        let out = &mut *(lparam.0 as *mut Vec<Rect>);
        out.push(to_rect(*rect));
    }
    BOOL(1)
}

/// Every monitor's rect, for `place::safe_restore_rect`'s off-screen check.
pub(crate) fn monitor_rects() -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(monitor_enum_proc), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// The primary monitor's work area (excludes the taskbar), for centering a
/// guest that otherwise has nowhere sane to land.
pub(crate) fn primary_work_area() -> Rect {
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut info).as_bool() {
            to_rect(info.rcWork)
        } else {
            Rect { left: 0, top: 0, right: 1920, bottom: 1080 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercises the pure bookkeeping (insert/filter/remove) without a real
    // window: `unpark_all_of`'s `IsWindow` check rejects the fabricated hwnd
    // as stale, so only the map/set mutations are under test here, the same
    // split `find.rs`'s own "stale handle" tests rely on.
    #[test]
    fn parked_windows_of_filters_by_pid_and_unpark_clears_bookkeeping() {
        let fake_hwnd: isize = 0x7FFF_FFFF;
        parked().lock().unwrap().insert(
            fake_hwnd,
            Parked { pid: 4242, original: Rect { left: 10, top: 20, right: 110, bottom: 220 } },
        );
        pids().lock().unwrap().insert(4242);

        let tree = HashSet::from([4242]);
        assert_eq!(parked_windows_of(&tree), vec![fake_hwnd]);
        assert_eq!(original_rect_of(fake_hwnd), Some(Rect { left: 10, top: 20, right: 110, bottom: 220 }));

        unpark_all_of(&tree);
        assert!(original_rect_of(fake_hwnd).is_none());
        assert!(!pids().lock().unwrap().contains(&4242));
    }
}
