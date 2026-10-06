//! Re-checks a freshly embedded pane dock a moment after `embed` placed it.
//! `windows-taskbar-widgets` measured live (commit `38c75c9`, todo 0052):
//! `SetParent` succeeds and the window lands in the pane rect, then within
//! ~200ms it snaps back to its own native rect near the taskbar and holds
//! that forever, with `parent_is_host` still reporting true throughout. The
//! embed call itself can't see that - it only knows `SetParent` succeeded -
//! so `dock::dock_window`'s fresh-embed path calls `verify_holds` once,
//! after a short wait, and releases the dock if the window didn't stay put.
//!
//! Split out of `dock.rs` (already at its own ~300-line split bar) rather
//! than grown there.

use super::on_main;
use crate::supervisor::window::{self, OriginalState, Rect};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::AppHandle;

/// Measured past the ~200ms snap `windows-taskbar-widgets` showed live:
/// long enough that a real re-pin has already happened, short enough a
/// user ticking the dock toggle won't feel the wait.
const HOLD_CHECK_DELAY: Duration = Duration::from_millis(300);

/// (proc id, pid) pairs whose fresh pane embed did not hold its rect. Kept
/// separate from `headless::refused()`: that set is cleared every tick by
/// `headless_tick`'s own `!want` branch for any proc not currently asking
/// to run headless - which is every pane-docked proc - so sharing it would
/// erase a pane refusal within one tick of recording it. A restart gets a
/// new pid and a new attempt, same as the headless set.
fn pane_refused() -> &'static Mutex<HashSet<(String, u32)>> {
    static PANE_REFUSED: OnceLock<Mutex<HashSet<(String, u32)>>> = OnceLock::new();
    PANE_REFUSED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Whether `(proc_id, pid)` already failed to hold a pane dock, for
/// `dock_state_for` to report `DockState::Refused` instead of `NotDocked`.
pub(super) fn is_pane_refused(proc_id: &str, pid: u32) -> bool {
    pane_refused().lock().unwrap().contains(&(proc_id.to_string(), pid))
}

/// Confirms a just-embedded guest is still where `target` asked for it,
/// `HOLD_CHECK_DELAY` after `embed` placed it - past the measured re-pin
/// window. Sleeps on the CALLING thread (the `dock_window` worker thread
/// invoking this), never on the Tauri main thread the two `on_main` calls
/// here marshal onto, so the wait never blocks the message pump.
///
/// On a hold failure, releases the dock exactly as `undock_window` would
/// (restore style/ex-style/parent/rect/show-state) and records the
/// refusal. Returns `Ok(())` unchanged when the dock held, or when the
/// handle already went stale (a dead window is the caller's own liveness
/// check's problem, not a "won't hold" refusal).
pub(super) fn verify_holds(
    app: &AppHandle,
    proc_id: &str,
    pid: u32,
    guest: isize,
    target: Rect,
    original: OriginalState,
) -> Result<(), String> {
    std::thread::sleep(HOLD_CHECK_DELAY);

    let still_holds = on_main(app, move || match window::live_rect(guest) {
        Some(actual) => window::holds_target(actual, target),
        None => true,
    })
    .map_err(|e| format!("hold recheck on main thread failed: {e}"))?;

    if still_holds {
        return Ok(());
    }

    log::warn!(
        "supervisor::dock::pane_hold: {proc_id} (pid {pid}) did not hold its docked rect \
         (re-pinned within {HOLD_CHECK_DELAY:?}); releasing it and reporting it as refused"
    );
    on_main(app, move || {
        if window::is_window_alive(guest) {
            let _ = window::release(guest, &original);
        }
    })
    .map_err(|e| format!("release after a failed hold check failed: {e}"))?;

    pane_refused().lock().unwrap().insert((proc_id.to_string(), pid));
    Err(format!("{proc_id} would not hold its docked position, so it was released"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::proc::ManagedProc;
    use crate::supervisor::Supervisor;
    use crate::types::{ProcKind, ProcSpec};

    #[test]
    fn is_pane_refused_reflects_a_recorded_refusal() {
        let id = "test-pane-hold-refused";
        let pid = 987_654;
        assert!(!is_pane_refused(id, pid));
        pane_refused().lock().unwrap().insert((id.to_string(), pid));
        assert!(is_pane_refused(id, pid));
        pane_refused().lock().unwrap().remove(&(id.to_string(), pid));
    }

    fn fast_exit_spec(id: &str) -> ProcSpec {
        ProcSpec {
            id: id.to_string(),
            project: "p".to_string(),
            name: "c".to_string(),
            cmd: "cmd /C exit 0".to_string(),
            cwd: ".".to_string(),
            kind: ProcKind::Generic,
            autostart: false,
            use_dynamic_port: false,
            fixed_port: None,
            env: String::new(),
            dock_window: false,
            play_sound: false,
            dock_headless: false,
        }
    }

    // Exercises `dock_state_for`'s wiring directly: without its
    // `pane_hold::is_pane_refused(...)` check, a recorded pane refusal with
    // no registry entry would read as `None` (untracked), not `Refused`.
    // No live window involved, same as `headless.rs`'s own
    // `dock_state_for_reports_refused_after_a_refusal`.
    #[test]
    fn dock_state_for_reports_refused_after_a_pane_hold_failure() {
        let dir = tempfile::tempdir().unwrap();
        let ports = std::sync::Arc::new(crate::ports::PortRegistry::new(dir.path().to_path_buf()));
        let sup = Supervisor::new(dir.path().to_path_buf(), ports);
        let id = "pane-hold-refused-test:cmd";
        let pid = 456_789;
        {
            let mut map = sup.procs.lock().unwrap();
            let mut p = ManagedProc::new(fast_exit_spec(id));
            p.pid = Some(pid);
            map.insert(id.to_string(), p);
        }
        pane_refused().lock().unwrap().insert((id.to_string(), pid));

        let state = sup.dock_state_for(id);

        pane_refused().lock().unwrap().remove(&(id.to_string(), pid));
        assert_eq!(state, Some(crate::types::DockState::Refused));
    }
}
