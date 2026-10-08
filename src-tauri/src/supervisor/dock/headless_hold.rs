//! Re-checks a freshly embedded headless dock a moment after
//! `headless::try_dock_headless` placed it, mirroring `pane_hold`'s recheck
//! for the dashboard-pane path (see that module's doc comment for the
//! `windows-taskbar-widgets` measurement this is modeled on: `SetParent`
//! succeeds, the window lands in the target rect, then within ~200ms it
//! snaps back to its own native rect and holds that forever).
//! `try_dock_headless`'s own `SetParent` failure check cannot see a
//! successful `SetParent` followed by a later re-pin, so after a fresh
//! headless embed it calls `verify_headless_holds` once, after a short
//! wait, and releases + marks the proc refused if the window didn't stay
//! put. Before this module, a re-pinning app run headless stayed marked
//! `Embedded` forever, showing a blank or stale capture instead of
//! `Refused` (todo 0065).
//!
//! Split out of `headless.rs` (already near the project's ~300-line split
//! bar) rather than grown there, same reasoning `pane_hold.rs` used when it
//! split out of `dock.rs`.

use super::headless::{destroy_host, refused};
use super::on_main;
use super::registry::{registry, Entry};
use crate::supervisor::window::{self, OriginalState, Rect};
use std::time::Duration;
use tauri::AppHandle;

/// Same measured delay `pane_hold::HOLD_CHECK_DELAY` uses - the re-pin this
/// guards against is the same window behavior, just embedded into a
/// headless host instead of a dashboard pane.
const HOLD_CHECK_DELAY: Duration = Duration::from_millis(300);

/// Confirms a just-embedded headless guest is still where `target` (the
/// host's screen rect) asked for it, `HOLD_CHECK_DELAY` after `embed` placed
/// it. Sleeps on the calling thread - `headless_tick`'s own background
/// thread, never the Tauri main thread the two `on_main` calls here
/// marshal onto - so the wait never blocks the message pump.
///
/// The target is read from the registry AFTER the wait, never captured
/// before it, so a dock that was undocked (or re-embedded into a new host)
/// meanwhile is left alone: comparing against a stale rect would refuse a
/// window that is exactly where it was last asked to be.
///
/// On a hold failure, takes the registry entry (only if it is still this
/// guest's, in this exact host), releases it and destroys the host exactly
/// as `undock_window` would, and records the refusal in `headless::refused`
/// so `dock_state_for` reports `Refused` and `headless_tick` stops retrying
/// this pid. Returns `Ok(())` when the dock held, when it was undocked or
/// re-embedded meanwhile, or when the handle already went stale (a dead
/// window is the caller's own liveness check's problem, not a "won't hold"
/// refusal).
pub(super) fn verify_headless_holds(
    app: &AppHandle,
    proc_id: &str,
    pid: u32,
    guest: isize,
    host: isize,
    original: OriginalState,
) -> Result<(), String> {
    std::thread::sleep(HOLD_CHECK_DELAY);

    let Some(target) = current_target(proc_id, guest, host) else {
        return Ok(());
    };
    let actual = on_main(app, move || window::live_rect(guest))
        .map_err(|e| format!("headless hold recheck on main thread failed: {e}"))?;

    if !hold_failed(actual, target) {
        return Ok(());
    }

    {
        let mut entries = registry().entries.lock().unwrap();
        let still_ours = matches!(
            entries.get(proc_id),
            Some(Entry::Active { hwnd, headless_host: Some(h), .. }) if *hwnd == guest && *h == host
        );
        if !still_ours {
            return Ok(());
        }
        entries.remove(proc_id);
    }
    log::warn!(
        "supervisor::dock::headless_hold: {proc_id} (pid {pid}) did not hold its headless dock \
         (re-pinned within {HOLD_CHECK_DELAY:?}); releasing it and reporting it as refused"
    );
    // Recorded before the release so a failed release still stops
    // headless_tick from building a second host for this pid.
    refused().lock().unwrap().insert((proc_id.to_string(), pid));
    on_main(app, move || {
        if window::is_window_alive(guest) {
            let _ = window::release(guest, &original);
        }
        destroy_host(host);
    })
    .map_err(|e| format!("release after a failed headless hold check failed: {e}"))?;

    Err(format!("{proc_id} would not hold its headless dock, so it was released"))
}

/// The registry's current target for `proc_id`, if it is still a headless
/// dock of this exact `guest` into this exact `host`.
fn current_target(proc_id: &str, guest: isize, host: isize) -> Option<Rect> {
    match registry().entries.lock().unwrap().get(proc_id) {
        Some(Entry::Active { hwnd, target, headless_host: Some(h), .. }) if *hwnd == guest && *h == host => {
            Some(*target)
        }
        _ => None,
    }
}

/// Pure decision: does `actual` (the guest's just-measured live rect, `None`
/// once its handle has gone stale) count as a failed hold against `target`
/// (the headless host's screen rect)? A dead handle is never a refusal - the
/// caller's own liveness check is what notices that, same as `pane_hold`'s
/// inline `match live_rect(guest) { ... None => true }`.
fn hold_failed(actual: Option<Rect>, target: Rect) -> bool {
    match actual {
        Some(rect) => !window::holds_target(rect, target),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::window::OriginalState;

    fn rect(l: i32) -> Rect {
        Rect { left: l, top: 0, right: l + 100, bottom: 100 }
    }

    // Without `hold_failed`, nothing distinguished a re-pinned headless
    // guest from one that genuinely held its rect; this is the decision
    // `verify_headless_holds` turns into a release + refusal.
    #[test]
    fn hold_failed_true_when_actual_drifts_from_target() {
        let target = rect(0);
        let actual = Rect { left: 500, top: 500, right: 600, bottom: 600 };
        assert!(hold_failed(Some(actual), target));
    }

    #[test]
    fn hold_failed_false_when_actual_matches_target() {
        let target = rect(0);
        assert!(!hold_failed(Some(target), target));
    }

    // A dead handle is the liveness check's problem, not a refusal - same
    // contract `pane_hold::verify_holds` relies on.
    #[test]
    fn hold_failed_false_when_window_already_gone() {
        assert!(!hold_failed(None, rect(0)));
    }

    // Mirrors `pane_hold`'s `current_target_follows_the_live_registry_entry`:
    // the recheck must read the registry's live state after its wait, not a
    // rect captured before it, so a re-embed or undock mid-wait is not
    // mistaken for a hold failure against the original target.
    #[test]
    fn current_target_follows_the_live_registry_entry() {
        let id = "test-headless-hold-current-target";
        let guest = 7373;
        let host = 9191;
        let active = |hwnd, host, target| Entry::Active {
            hwnd,
            original: OriginalState::for_test(),
            outcome: window::DockOutcome::Embedded,
            target,
            headless_host: Some(host),
        };
        let entries = || registry().entries.lock().unwrap();

        entries().insert(id.to_string(), active(guest, host, rect(0)));
        assert_eq!(current_target(id, guest, host), Some(rect(0)));

        entries().insert(id.to_string(), active(guest, host, rect(500)));
        assert_eq!(current_target(id, guest, host), Some(rect(500)), "a re-embed's new rect wins");

        assert_eq!(current_target(id, guest, host + 1), None, "another host's dock is not ours");
        assert_eq!(current_target(id, guest + 1, host), None, "another window's dock is not ours");

        entries().insert(id.to_string(), active(guest, host, rect(0)));
        entries().remove(id);
        assert_eq!(current_target(id, guest, host), None, "undocked meanwhile: nothing to verify");
    }
}
