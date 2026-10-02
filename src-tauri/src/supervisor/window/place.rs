//! Embed, soft-dock, release and reassert a guest window, promoted from
//! `tests/embed_spike.rs`'s `run_spike` embed/restore steps and its
//! `AttachTeardown` restore guard.
//!
//! Everything here takes and returns plain data (an `isize` HWND, `Rect` as
//! four `i32`s) rather than holding a raw pointer in long-lived state - a
//! docked window's HWND is only good until the caller next needs it, and
//! Windows recycles both PIDs and HWNDs, so every function revalidates its
//! handle with `IsWindow` before acting on it.

use super::ffi::{
    self, DPI_HOSTING_BEHAVIOR_MIXED, GWL_EXSTYLE, GWL_STYLE, Rect, SW_HIDE, SW_SHOW,
    SWP_FRAMECHANGED, SWP_NOZORDER, SWP_SHOWWINDOW, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_POPUP,
};
use super::find::is_window_alive;

/// How a dock attempt actually landed. The UI renders these differently
/// (an embedded window has no title bar of its own to fight with; a
/// soft-docked one still does), so this stays a real enum rather than
/// collapsing into a bool the caller would have to re-derive meaning from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockOutcome {
    /// `SetParent` succeeded: the guest is a true child window of the host.
    Embedded,
    /// `SetParent` failed (real apps that stay always-on-top return
    /// `ERROR_ACCESS_DENIED` here) and the fallback ran instead: the guest
    /// stays a top-level window, just moved and sized to sit over the
    /// host's pane.
    SoftDocked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceError {
    /// `IsWindow` failed on a handle the caller passed in. Windows recycles
    /// both PIDs and HWNDs, so this is a normal, expected outcome (the
    /// guest process died, or closed its window) and never a bug on its
    /// own - callers must treat it as "gone", not retry blindly.
    WindowGone,
}

/// Everything needed to put a guest window back exactly as `embed` found
/// it, including show-state: `SetWindowLongPtrW(GWL_STYLE, ...)` does not
/// reliably toggle `WS_VISIBLE` on its own (confirmed in the spike's
/// `AttachTeardown` restore), so `release` also needs the original
/// visibility to issue an explicit `ShowWindow`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OriginalState {
    style: isize,
    exstyle: isize,
    parent: isize,
    rect: Rect,
    visible: bool,
}

impl OriginalState {
    /// The recorded rect, for a caller deciding whether it is still a sane
    /// place to restore to (see `safe_restore_rect`).
    pub(crate) fn rect(&self) -> Rect {
        self.rect
    }

    /// Overrides just the recorded rect, keeping style/exstyle/parent/visible
    /// as `embed`'s live snapshot found them. `supervisor::window::park` may
    /// have already moved a headless guest off-screen before `embed`'s own
    /// snapshot ran (the park hook fires the instant the window is created,
    /// well before `dock::headless`'s poll tick finds it), so without this
    /// override `release` would restore the guest to -32000 forever instead
    /// of back to the desktop.
    pub(crate) fn with_rect(mut self, rect: Rect) -> Self {
        self.rect = rect;
        self
    }
}

/// Whether two rects overlap at all (a touching edge, zero-area overlap,
/// does not count).
fn rects_intersect(a: &Rect, b: &Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// Where a guest should actually land on release: `recorded` unchanged when
/// it still overlaps a real monitor, else centered on `primary_work_area`.
/// The fallback path matters when no `park`-recorded original exists AND the
/// only rect on hand is itself off every monitor - e.g. headless was turned
/// on for a window the park hook never saw get created (hook not running
/// yet, or state lost across a restart) - so this is a second, independent
/// safety net on top of `OriginalState::with_rect`'s park-rect override, not
/// a replacement for it.
pub(crate) fn safe_restore_rect(recorded: Rect, monitors: &[Rect], primary_work_area: Rect) -> Rect {
    if monitors.iter().any(|m| rects_intersect(m, &recorded)) {
        return recorded;
    }
    let w = recorded.width().max(1);
    let h = recorded.height().max(1);
    let left = primary_work_area.left + (primary_work_area.width() - w) / 2;
    let top = primary_work_area.top + (primary_work_area.height() - h) / 2;
    Rect { left, top, right: left + w, bottom: top + h }
}

fn snapshot(guest: ffi::HWND) -> OriginalState {
    let mut rect = Rect::default();
    unsafe {
        ffi::GetWindowRect(guest, &mut rect);
    }
    OriginalState {
        style: unsafe { ffi::GetWindowLongPtrW(guest, GWL_STYLE) },
        exstyle: unsafe { ffi::GetWindowLongPtrW(guest, GWL_EXSTYLE) },
        parent: unsafe { ffi::GetParent(guest) } as isize,
        rect,
        visible: unsafe { ffi::IsWindowVisible(guest) } != 0,
    }
}

/// `SetWindowPos` places a `WS_CHILD` relative to its parent's client
/// area, never the screen, so an embedded guest handed the caller's screen
/// rect would land offset by the host's own screen position. A top-level
/// (soft-docked) guest keeps screen coordinates. The style check matters:
/// `GetParent` on a top-level popup returns its OWNER, not a parent.
fn to_placement_coords(guest: ffi::HWND, target: Rect) -> Rect {
    unsafe {
        let style = ffi::GetWindowLongPtrW(guest, GWL_STYLE) as u32;
        let parent = ffi::GetParent(guest);
        if style & WS_CHILD == 0 || parent.is_null() {
            return target;
        }
        let mut r = target;
        // A RECT is two POINTs back to back, which is what MapWindowPoints
        // converts in place.
        ffi::MapWindowPoints(std::ptr::null_mut(), parent, &mut r, 2);
        r
    }
}

/// Embeds `guest` into `host`'s pane at `target` (screen coordinates,
/// already computed by the caller - this module does not know about panes
/// or layout). Must run on the host's own thread: `SetThreadDpiHostingBehavior`
/// only affects the calling thread, and skipping it makes `SetParent` fail
/// outright on mismatched DPI awareness between host and guest.
///
/// If `SetParent` returns null, this does not fail - it restores the style
/// bits it had just changed and soft-docks instead (`SetWindowPos` only, no
/// reparenting). The spike found real always-on-top apps that refuse
/// `SetParent` with `ERROR_ACCESS_DENIED`, so this branch is taken in
/// practice, not just in theory.
pub fn embed(
    guest: isize,
    host: isize,
    target: Rect,
) -> Result<(DockOutcome, OriginalState), PlaceError> {
    let guest_hwnd = guest as ffi::HWND;
    let host_hwnd = host as ffi::HWND;
    if !is_window_alive(guest) || unsafe { ffi::IsWindow(host_hwnd) } == 0 {
        return Err(PlaceError::WindowGone);
    }

    let original = snapshot(guest_hwnd);

    let ok = unsafe { ffi::SetThreadDpiHostingBehavior(DPI_HOSTING_BEHAVIOR_MIXED) };
    if ok == -1 {
        log::warn!(
            "supervisor::window::place: SetThreadDpiHostingBehavior(MIXED) did not report \
             success; continuing anyway, but this is the first place to look if SetParent \
             fails next"
        );
    }

    let new_style = ((original.style as u32) & !WS_OVERLAPPEDWINDOW & !WS_POPUP) | WS_CHILD;
    unsafe {
        ffi::SetWindowLongPtrW(guest_hwnd, GWL_STYLE, new_style as isize);
    }

    let set_parent_result = unsafe { ffi::SetParent(guest_hwnd, host_hwnd) };
    let outcome = if set_parent_result.is_null() {
        let last_error = unsafe { ffi::GetLastError() };
        log::warn!(
            "supervisor::window::place: SetParent failed (GetLastError={last_error}), \
             falling back to soft-dock"
        );
        unsafe {
            ffi::SetWindowLongPtrW(guest_hwnd, GWL_STYLE, original.style);
        }
        unsafe {
            ffi::SetWindowPos(
                guest_hwnd,
                std::ptr::null_mut(),
                target.left,
                target.top,
                target.width(),
                target.height(),
                SWP_NOZORDER,
            );
        }
        DockOutcome::SoftDocked
    } else {
        let at = to_placement_coords(guest_hwnd, target);
        unsafe {
            ffi::SetWindowPos(
                guest_hwnd,
                std::ptr::null_mut(),
                at.left,
                at.top,
                at.width(),
                at.height(),
                SWP_FRAMECHANGED | SWP_SHOWWINDOW,
            );
        }
        DockOutcome::Embedded
    };

    if !original.visible {
        unsafe {
            ffi::ShowWindow(guest_hwnd, SW_SHOW);
        }
    }

    Ok((outcome, original))
}

/// Restores style, ex-style, parent, rect and show-state to what `embed`
/// (or `snapshot`) recorded. Works uniformly for both `DockOutcome`s: a
/// soft-docked window's style/parent were never changed, so re-applying the
/// same values back is a no-op for those fields and only the rect (and
/// show-state, if `embed` had to reveal a hidden window) actually moves.
pub fn release(guest: isize, original: &OriginalState) -> Result<(), PlaceError> {
    if !is_window_alive(guest) {
        return Err(PlaceError::WindowGone);
    }
    let guest_hwnd = guest as ffi::HWND;
    unsafe {
        ffi::SetWindowLongPtrW(guest_hwnd, GWL_STYLE, original.style);
        ffi::SetWindowLongPtrW(guest_hwnd, GWL_EXSTYLE, original.exstyle);
        ffi::SetParent(guest_hwnd, original.parent as ffi::HWND);
        ffi::SetWindowPos(
            guest_hwnd,
            std::ptr::null_mut(),
            original.rect.left,
            original.rect.top,
            original.rect.width(),
            original.rect.height(),
            SWP_FRAMECHANGED,
        );
        // GWL_STYLE alone does not reliably toggle WS_VISIBLE (MSDN: use
        // ShowWindow for that bit) - a window that started hidden to the
        // tray must end hidden again, not merely styled as if it were.
        ffi::ShowWindow(guest_hwnd, if original.visible { SW_SHOW } else { SW_HIDE });
    }
    Ok(())
}

/// Idempotently re-places an already-docked window into `target`. Exists
/// for a caller's hold-position loop: some apps move or resize themselves
/// back after being docked, and this is how the loop corrects that without
/// re-running the embed/soft-dock decision.
pub fn reassert(guest: isize, target: Rect) -> Result<(), PlaceError> {
    if !is_window_alive(guest) {
        return Err(PlaceError::WindowGone);
    }
    let at = to_placement_coords(guest as ffi::HWND, target);
    unsafe {
        ffi::SetWindowPos(
            guest as ffi::HWND,
            std::ptr::null_mut(),
            at.left,
            at.top,
            at.width(),
            at.height(),
            SWP_NOZORDER,
        );
    }
    Ok(())
}

/// Builds a placeholder `OriginalState` for tests elsewhere in the crate
/// (the dock registry's fixtures, notably) that need one but have no live
/// window to `snapshot`. Goes through real field assignment rather than a
/// zeroed-memory cast, so a field added to `OriginalState` later becomes a
/// compile error for those callers instead of silent UB from a bit pattern
/// nobody checked still applies.
#[cfg(test)]
pub(crate) fn test_original_state() -> OriginalState {
    OriginalState { style: 0, exstyle: 0, parent: 0, rect: Rect::default(), visible: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_handle_is_rejected_by_every_entry_point() {
        let zero_rect = Rect::default();
        assert_eq!(embed(0, 0, zero_rect), Err(PlaceError::WindowGone));
        let original = OriginalState {
            style: 0,
            exstyle: 0,
            parent: 0,
            rect: zero_rect,
            visible: true,
        };
        assert_eq!(release(0, &original), Err(PlaceError::WindowGone));
        assert_eq!(reassert(0, zero_rect), Err(PlaceError::WindowGone));
    }

    #[test]
    fn safe_restore_rect_keeps_a_recorded_rect_that_overlaps_a_monitor() {
        let monitors = [Rect { left: 0, top: 0, right: 1920, bottom: 1080 }];
        let recorded = Rect { left: 100, top: 100, right: 900, bottom: 700 };
        assert_eq!(safe_restore_rect(recorded, &monitors, monitors[0]), recorded);
    }

    #[test]
    fn safe_restore_rect_centers_an_off_screen_rect_on_the_primary_work_area() {
        let monitors = [Rect { left: 0, top: 0, right: 1920, bottom: 1080 }];
        let primary_work_area = Rect { left: 0, top: 0, right: 1920, bottom: 1040 };
        // Parked at -32000: no monitor overlaps it.
        let recorded = Rect { left: -32_000, top: -32_000, right: -31_200, bottom: -31_200 };
        let restored = safe_restore_rect(recorded, &monitors, primary_work_area);
        assert_eq!(restored.width(), 800);
        assert_eq!(restored.height(), 800);
        // Centered: equal margin on both sides of the 1920-wide work area.
        assert_eq!(restored.left, (1920 - 800) / 2);
        assert_eq!(restored.top, (1040 - 800) / 2);
    }
}
