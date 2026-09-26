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
        unsafe {
            ffi::SetWindowPos(
                guest_hwnd,
                std::ptr::null_mut(),
                target.left,
                target.top,
                target.width(),
                target.height(),
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
    unsafe {
        ffi::SetWindowPos(
            guest as ffi::HWND,
            std::ptr::null_mut(),
            target.left,
            target.top,
            target.width(),
            target.height(),
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
}
