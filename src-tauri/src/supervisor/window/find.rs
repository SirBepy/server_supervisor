//! Finds a running process's real top-level main window, promoted from
//! `tests/embed_spike.rs`'s `find_guest_window` / `best_tier` /
//! `descendant_pids`. This is the highest-risk piece of the whole feature:
//! a naive "biggest window owned by this pid" rule picked the wrong window
//! twice during live spike testing (see `evaluate`'s and `pick_best`'s doc
//! comments), so the filter and the tiering below are load-bearing, not
//! decorative.
//!
//! The decision logic (`evaluate`, `pick_best`) is pure and takes plain data
//! in, so it is unit-testable without a desktop. The Win32-calling half
//! (`find_window`) is the thin, untested shell that produces that data.

use super::ffi::{
    self, GW_OWNER, MIN_MAIN_WINDOW_DIM, Rect, TH32CS_SNAPPROCESS, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// A window that survived every rejection rule and is eligible to be picked
/// as the guest's real main window. Deliberately holds only what the
/// tiering in `pick_best` needs; title and rect are fetched once, for the
/// winner only, by `find_window`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Candidate {
    pub hwnd: isize,
    pub visible: bool,
    // Owned by the root pid itself rather than a descendant process.
    // Chromium/WebView2 helper subprocesses (gpu-process, renderer, the
    // msedgewebview2.exe host) reliably own a hidden, monitor-sized
    // compositor surface window carrying none of the reject flags below,
    // which would otherwise win outright under a plain largest-area rule -
    // confirmed live via Get-Process on the owning pid.
    pub root_owned: bool,
    pub area: i64,
}

/// Raw per-window attributes as read from Win32, before any accept/reject
/// policy is applied. Kept separate from `Candidate` so `evaluate` can be
/// exercised with hand-built values in tests.
#[derive(Clone, Copy, Debug)]
pub(super) struct WindowAttrs {
    pub visible: bool,
    pub width: i32,
    pub height: i32,
    pub exstyle: u32,
    pub has_owner: bool,
    pub root_owned: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RejectReason {
    NotVisible,
    HasOwner,
    TooSmall,
    ToolWindow,
    Transparent,
    NoActivate,
}

/// Applies the three reject rules to one window's attributes and, if it
/// survives, turns it into a `Candidate`. Each rule exists because a naive
/// filter picked the wrong window in real testing:
///
/// - An owned window (`GW_OWNER` non-null): dialogs and tool popups are
///   owned by their main window and must never be picked instead of it.
/// - Either dimension below 32px: rejects 16x16 helper surfaces that
///   Tauri/Electron apps create for their own bookkeeping, while still
///   admitting legitimately narrow real windows (the 32px floor, not a
///   naive 200px one, is what lets a 200x100 or 468x48 HUD window through).
/// - `WS_EX_TOOLWINDOW`, `WS_EX_TRANSPARENT` or `WS_EX_NOACTIVATE`: marks a
///   helper/overlay surface that is never a real main window.
///
/// `require_visible` rejects a hidden window only when the caller demands
/// one (the spawn path, still waiting for a just-launched app to paint);
/// the attach path passes `false` because a window hidden to the tray is
/// still a legitimate dock target.
pub(super) fn evaluate(
    hwnd: isize,
    attrs: WindowAttrs,
    require_visible: bool,
) -> Result<Candidate, RejectReason> {
    if require_visible && !attrs.visible {
        return Err(RejectReason::NotVisible);
    }
    if attrs.has_owner {
        return Err(RejectReason::HasOwner);
    }
    if attrs.width < MIN_MAIN_WINDOW_DIM || attrs.height < MIN_MAIN_WINDOW_DIM {
        return Err(RejectReason::TooSmall);
    }
    if attrs.exstyle & WS_EX_TOOLWINDOW != 0 {
        return Err(RejectReason::ToolWindow);
    }
    if attrs.exstyle & WS_EX_TRANSPARENT != 0 {
        return Err(RejectReason::Transparent);
    }
    if attrs.exstyle & WS_EX_NOACTIVATE != 0 {
        return Err(RejectReason::NoActivate);
    }
    Ok(Candidate {
        hwnd,
        visible: attrs.visible,
        root_owned: attrs.root_owned,
        area: (attrs.width as i64) * (attrs.height as i64),
    })
}

/// Picks the single best candidate. Visible beats hidden; within a
/// visibility tier, root-pid-owned beats descendant-owned; largest-by-area
/// is the tiebreak WITHIN that chosen tier only, never across tiers - a
/// hidden 1920x1023 WebView2 compositor window must never outrank a
/// smaller visible root-owned window just because it has more pixels, and
/// falling back a tier (root-owned to descendant-owned, or visible to
/// hidden) only happens when the better tier is completely empty, which is
/// what lets the spawn path's real case (the launcher process handing the
/// window to a child) still resolve correctly.
pub(super) fn pick_best(candidates: &[Candidate]) -> Option<Candidate> {
    for want_visible in [true, false] {
        let visible_tier: Vec<&Candidate> =
            candidates.iter().filter(|c| c.visible == want_visible).collect();
        if visible_tier.is_empty() {
            continue;
        }
        let root_tier: Vec<&Candidate> =
            visible_tier.iter().filter(|c| c.root_owned).copied().collect();
        let pool = if root_tier.is_empty() { visible_tier } else { root_tier };
        return pool.into_iter().max_by_key(|c| c.area).copied();
    }
    None
}

/// The window this module found, plus enough context for the caller to
/// decide what to do with it. `was_hidden` distinguishes an intentionally
/// hidden dock target (a tray-minimised app) from the visible common case;
/// callers that need the window shown before docking check this flag.
#[derive(Clone, Debug)]
pub struct FoundWindow {
    pub hwnd: isize,
    pub was_hidden: bool,
    pub pid: u32,
    pub title: String,
    pub rect: Rect,
}

/// pid -> parent pid for every process on the machine, from one Toolhelp
/// snapshot (cheap enough for the audio watcher's 100ms tick, unlike a
/// `sysinfo` refresh).
pub(crate) fn snapshot_parent_map() -> HashMap<u32, u32> {
    let mut map = HashMap::new();
    unsafe {
        let snap = ffi::CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() {
            return map;
        }
        let mut entry: ffi::Processentry32W = std::mem::zeroed();
        entry.dw_size = std::mem::size_of::<ffi::Processentry32W>() as u32;
        if ffi::Process32FirstW(snap, &mut entry) != 0 {
            loop {
                map.insert(entry.th32_process_id, entry.th32_parent_process_id);
                entry.dw_size = std::mem::size_of::<ffi::Processentry32W>() as u32;
                if ffi::Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        ffi::CloseHandle(snap);
    }
    map
}

/// Fixed-point iteration over the whole (pid, parent_pid) table: a single
/// top-down pass can miss a grandchild whose parent entry appears later in
/// the Toolhelp snapshot than the child's own entry, since the snapshot
/// order is not guaranteed.
fn descendant_pids(root: u32) -> HashSet<u32> {
    let parent_of = snapshot_parent_map();
    let mut set = HashSet::new();
    set.insert(root);
    loop {
        let mut grew = false;
        for (&pid, &parent) in parent_of.iter() {
            if set.contains(&parent) && !set.contains(&pid) {
                set.insert(pid);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    set
}

struct EnumResult {
    hwnds: Vec<ffi::HWND>,
}

unsafe extern "system" fn enum_proc(hwnd: ffi::HWND, lparam: ffi::LPARAM) -> ffi::BOOL {
    let found = &mut *(lparam as *mut EnumResult);
    found.hwnds.push(hwnd);
    1
}

fn window_title(hwnd: ffi::HWND) -> String {
    let mut buf = [0u16; 512];
    let len = unsafe { ffi::GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

/// One EnumWindows pass: builds the descendant pid set, evaluates every
/// top-level window belonging to it, and returns the current best pick (if
/// any) without waiting.
fn try_find_window(root_pid: u32, require_visible: bool) -> Option<FoundWindow> {
    let descendants = descendant_pids(root_pid);
    let mut found = EnumResult { hwnds: Vec::new() };
    unsafe {
        ffi::EnumWindows(enum_proc, &mut found as *mut EnumResult as ffi::LPARAM);
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    // hwnd -> (pid, rect), kept only long enough to re-fetch the winner's
    // details after `pick_best` chooses - most enumerated windows are
    // rejected and never need this.
    let mut details: HashMap<isize, (u32, Rect)> = HashMap::new();

    for hwnd in found.hwnds {
        let mut pid: u32 = 0;
        unsafe {
            ffi::GetWindowThreadProcessId(hwnd, &mut pid);
        }
        if !descendants.contains(&pid) {
            continue;
        }

        let mut rect = Rect::default();
        unsafe {
            ffi::GetWindowRect(hwnd, &mut rect);
        }
        let attrs = unsafe {
            WindowAttrs {
                visible: ffi::IsWindowVisible(hwnd) != 0,
                width: rect.width(),
                height: rect.height(),
                exstyle: ffi::GetWindowLongPtrW(hwnd, ffi::GWL_EXSTYLE) as u32,
                has_owner: !ffi::GetWindow(hwnd, GW_OWNER).is_null(),
                root_owned: pid == root_pid,
            }
        };

        if let Ok(candidate) = evaluate(hwnd as isize, attrs, require_visible) {
            details.insert(hwnd as isize, (pid, rect));
            candidates.push(candidate);
        }
    }

    let best = pick_best(&candidates)?;
    let (pid, rect) = details.get(&best.hwnd).copied().unwrap_or((root_pid, Rect::default()));
    Some(FoundWindow {
        hwnd: best.hwnd,
        was_hidden: !best.visible,
        pid,
        title: window_title(best.hwnd as ffi::HWND),
        rect,
    })
}

/// Polls up to `timeout` for `root_pid` or any of its descendants' real
/// top-level main window. `require_visible` is the one deliberate
/// difference between a spawn path (just launched, poll until it paints -
/// pass `true`) and an attach path (reaching for an already-running app
/// that may be hidden to the tray - pass `false`, since that hidden window
/// is still the legitimate dock target).
pub fn find_window(
    root_pid: u32,
    timeout: Duration,
    require_visible: bool,
) -> Option<FoundWindow> {
    let start = Instant::now();
    loop {
        if let Some(w) = try_find_window(root_pid, require_visible) {
            return Some(w);
        }
        if start.elapsed() >= timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// A single `EnumWindows` pass with no retry sleep-loop, for a caller that
/// runs its own timer tick and must never block on this call - `find_window`
/// sleeps up to `timeout` between attempts, which is fine for a one-shot
/// dock request but would hold any lock the caller took across the whole
/// poll if called from a periodic reconcile pass instead.
pub fn find_window_once(root_pid: u32, require_visible: bool) -> Option<FoundWindow> {
    try_find_window(root_pid, require_visible)
}

/// Stale-handle guard: Windows recycles both PIDs and HWNDs, so any HWND a
/// caller holds onto (e.g. across a hold-position loop) must be revalidated
/// before use - acting on a recycled handle would move or reparent an
/// unrelated app's window.
pub fn is_window_alive(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    unsafe { ffi::IsWindow(hwnd as ffi::HWND) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(visible: bool, width: i32, height: i32, exstyle: u32, root_owned: bool) -> WindowAttrs {
        WindowAttrs { visible, width, height, exstyle, has_owner: false, root_owned }
    }

    #[test]
    fn toolwindow_helper_is_rejected() {
        // A helper surface carrying WS_EX_TOOLWINDOW is rejected on that flag
        // specifically, independent of the (also-failing) 16x16 size floor -
        // sized here above the floor so this test isolates that one rule.
        let a = attrs(true, 200, 100, WS_EX_TOOLWINDOW, true);
        assert_eq!(evaluate(1, a, true), Err(RejectReason::ToolWindow));

        // The literal 16x16 case Tauri/Electron apps own is rejected too,
        // just on the size rule that happens to run first.
        let tiny = attrs(true, 16, 16, WS_EX_TOOLWINDOW, true);
        assert_eq!(evaluate(2, tiny, true), Err(RejectReason::TooSmall));
    }

    #[test]
    fn hidden_compositor_window_loses_to_smaller_visible_root_owned() {
        // The hidden ~1920x1023 WebView2 helper-process compositor surface.
        let compositor = evaluate(1, attrs(false, 1920, 1023, 0, false), false).unwrap();
        // The app's real, smaller, visible, root-owned window.
        let real = evaluate(2, attrs(true, 900, 600, 0, true), false).unwrap();
        let chosen = pick_best(&[compositor, real]).unwrap();
        assert_eq!(chosen.hwnd, 2);
    }

    #[test]
    fn narrow_hud_windows_survive_the_32px_floor() {
        // 200x100 and 468x48 are both real windows seen in live testing;
        // a naive 200px floor would have rejected the second one.
        assert!(evaluate(1, attrs(true, 200, 100, 0, true), true).is_ok());
        assert!(evaluate(2, attrs(true, 468, 48, 0, true), true).is_ok());
    }

    #[test]
    fn hidden_tier_used_when_only_hidden_candidates_exist() {
        let hidden = evaluate(1, attrs(false, 300, 200, 0, true), false).unwrap();
        let chosen = pick_best(&[hidden]).unwrap();
        assert_eq!(chosen.hwnd, 1);
    }

    #[test]
    fn empty_candidate_list_returns_none() {
        assert!(pick_best(&[]).is_none());
    }

    #[test]
    fn stale_handle_zero_is_never_alive() {
        assert!(!is_window_alive(0));
    }
}
