//! Pure "did the dock hold" check, plus the live-rect helper the dock
//! module's post-embed recheck reads it with. Split out as a sibling of
//! `place.rs` rather than grown there - `place.rs` was already 323 lines,
//! past the project's ~330 split bar.
//!
//! `windows-taskbar-widgets` measured re-pinning itself ~200ms after a
//! successful `SetParent` + `SetWindowPos`: live rect `(70,1473,538,1521)`
//! against a requested pane rect of `(98,121,1202,822)` (commit `38c75c9`'s
//! diagnostic trace, recorded in todo 0052). Both rects are screen
//! coordinates: the caller's `target` is the pane rect it already computed
//! in screen space (see `place::embed`'s doc), and `GetWindowRect` always
//! reads screen coordinates regardless of parentage - unlike `SetWindowPos`
//! on a `WS_CHILD`, which `to_placement_coords` has to convert separately -
//! so `holds_target` compares the two directly, no conversion needed.

use super::ffi::{self, Rect};
use super::find::is_window_alive;

/// Rounding/frame slop tolerated at the target rect's edges, so a window
/// that is merely off by a DPI-rounded pixel or two does not read as
/// refused. Small next to the measured re-pin, which missed on both axes
/// by hundreds of pixels, so it can never mask a real drift.
const EDGE_TOLERANCE: i32 = 4;

/// Whether `actual` (a window's just-measured live rect) still sits where
/// `target` asked for it, judged by `actual`'s centre point rather than an
/// exact match. Both rects must already be in screen coordinates (see
/// module doc).
pub(crate) fn holds_target(actual: Rect, target: Rect) -> bool {
    let centre_x = (actual.left + actual.right) / 2;
    let centre_y = (actual.top + actual.bottom) / 2;
    centre_x >= target.left - EDGE_TOLERANCE
        && centre_x <= target.right + EDGE_TOLERANCE
        && centre_y >= target.top - EDGE_TOLERANCE
        && centre_y <= target.bottom + EDGE_TOLERANCE
}

/// Re-reads `guest`'s current screen rect, for a caller re-checking a dock
/// a moment after `embed` placed it. `None` once the handle has already
/// gone stale - Windows recycles both PIDs and HWNDs, so a dead handle is
/// routine, never a bug on its own.
pub(crate) fn live_rect(guest: isize) -> Option<Rect> {
    if !is_window_alive(guest) {
        return None;
    }
    let mut rect = Rect::default();
    unsafe {
        ffi::GetWindowRect(guest as ffi::HWND, &mut rect);
    }
    Some(rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measured_widget_repin_does_not_hold() {
        let actual = Rect { left: 70, top: 1473, right: 538, bottom: 1521 };
        let target = Rect { left: 98, top: 121, right: 1202, bottom: 822 };
        assert!(!holds_target(actual, target));
    }

    #[test]
    fn exact_match_holds() {
        let target = Rect { left: 98, top: 121, right: 1202, bottom: 822 };
        assert!(holds_target(target, target));
    }

    #[test]
    fn small_dpi_rounding_offset_still_holds() {
        let target = Rect { left: 98, top: 121, right: 1202, bottom: 822 };
        let actual = Rect {
            left: target.left + 2,
            top: target.top - 1,
            right: target.right + 2,
            bottom: target.bottom - 1,
        };
        assert!(holds_target(actual, target));
    }

    #[test]
    fn centre_just_outside_tolerance_does_not_hold() {
        let target = Rect { left: 0, top: 0, right: 100, bottom: 100 };
        // Centre at (50,50); shift the whole rect right so its centre sits
        // at x=105, 5px past target.right (100) plus the 4px tolerance.
        let actual = Rect { left: 55, top: 0, right: 155, bottom: 100 };
        assert!(!holds_target(actual, target));
    }
}
