//! Post-hoc focus-steal suppression for the one spawn site that launches an
//! arbitrary, unbounded GUI-capable command (`proc/spawn.rs`'s `cmd /C
//! <cmd_str>`). Per `docs/research/focus-stealing-spec.md`, there is no Win32
//! API to deny a child the foreground in advance - `AllowSetForegroundWindow`
//! is grant-only, `LockSetForegroundWindow` can only be called by the current
//! foreground process, and `ForegroundLockTimeout` is a system-wide OS
//! setting. The only mechanism that actually works is reactive: capture
//! whichever window holds the foreground right before spawn, wait for the
//! new process's own window to appear (via the already-shipped `find_window`
//! finder this module shares with docking), mark it non-activating, then
//! hand the foreground back. A brief visible flash is the documented,
//! unavoidable cost of this approach.
//!
//! Deliberately does NOT `SetWindowPos(HWND_BOTTOM, ...)` the new window, the
//! third leg of the todo's original approach. Pushing a freshly launched
//! window to the bottom of the z-order hides an app the dev deliberately
//! just started behind every other window, which is a second, unrequested
//! behaviour change and the one most likely to read as a bug - the dev's
//! complaint is focus theft, not screen coverage (screen coverage is what
//! docking, `super::place`, already addresses for opted-in commands).
//! Restoring the captured foreground window already fixes what was asked.

use super::ffi::{self, SW_SHOWNOACTIVATE};
use super::find::find_window;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Bounded wait for the child's window. Most supervised commands are
/// headless dev servers with no window at all, so this must time out
/// quietly rather than spin - `find_window` already polls internally and
/// returns `None` once `timeout` elapses.
const FIND_WINDOW_TIMEOUT: Duration = Duration::from_secs(3);

/// Defaults to true: the dev's own complaint is what triggered this feature,
/// so a toggle defaulting off would leave it unfixed until he happened to
/// find the setting. Kept as a plain static `AtomicBool` (no `OnceLock`
/// needed) since a default value is all a fresh process needs before
/// `set_keep_focus_on_launch` runs the real settings value in at startup.
static ENABLED: AtomicBool = AtomicBool::new(true);

/// Mirrors the `Settings::keep_focus_on_launch` toggle into this process-wide
/// flag. Called once at startup (after `settings::load`) and again on every
/// `save_settings` (mirroring how `sync_autostart` re-syncs the OS startup
/// entry) - `proc::spawn::start` has no `AppHandle` to read `Settings` from
/// directly, and threading one through `Supervisor`/`ManagedProc` for a
/// single boolean would be a much larger change than this toggle needs.
pub fn set_keep_focus_on_launch(enabled: bool) {
    ENABLED.store(enabled, Ordering::SeqCst);
}

fn is_enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// The window holding the foreground right now, or `0` if there is none (a
/// legitimate Win32 state per the `SetForegroundWindow` docs: "There is
/// currently no foreground window"). Cheap enough to call unconditionally
/// from the spawn path; the gate that matters is in `guard_focus`.
pub fn capture_foreground() -> isize {
    unsafe { ffi::GetForegroundWindow() as isize }
}

/// Spawns a detached background thread that waits for `pid`'s window to
/// appear, shows it without activating, then restores `previous_foreground`.
/// No-ops immediately (no thread spawned) when the setting is off.
///
/// Never blocks the caller and never touches any lock: `proc::spawn::start`
/// must return as soon as the child process handle exists, not once this
/// guard finishes - which can take up to `FIND_WINDOW_TIMEOUT`, or (for a
/// headless dev server, the common case) run out that whole timeout finding
/// nothing. This repo already had a bug from a blocking poll under a shared
/// lock, fixed in the docking work; running detached is how this avoids
/// reintroducing it.
pub fn guard_focus(pid: u32, previous_foreground: isize) {
    if !is_enabled() {
        return;
    }
    std::thread::spawn(move || {
        // `require_visible: true` - same contract `find.rs` documents for the
        // spawn path: poll until the just-launched app paints, rather than
        // the attach path's "hidden is still a legitimate target".
        let Some(found) = find_window(pid, FIND_WINDOW_TIMEOUT, true) else {
            return;
        };

        unsafe {
            ffi::ShowWindow(found.hwnd as ffi::HWND, SW_SHOWNOACTIVATE);
        }

        if previous_foreground == 0 {
            return;
        }
        // A refusal here is an expected, non-fatal Windows outcome (see the
        // `SetForegroundWindow` doc comment on the FFI declaration), not a
        // guard failure - the launched child keeps running either way.
        let restored =
            unsafe { ffi::SetForegroundWindow(previous_foreground as ffi::HWND) };
        if restored == 0 {
            log::debug!(
                "supervisor::window::focus: could not restore foreground window \
                 {previous_foreground:#x} after launching pid {pid}; Windows refused \
                 the SetForegroundWindow call, which is expected and non-fatal"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // Win32 calls themselves are not unit-testable without a live desktop
    // (no HWND, no real GetForegroundWindow to assert against); what IS
    // testable without one is the enabled/disabled decision `guard_focus`
    // gates on, which is exercised directly here.
    #[test]
    fn disabling_the_setting_is_observed_before_any_window_work() {
        set_keep_focus_on_launch(false);
        assert!(!is_enabled());
        set_keep_focus_on_launch(true);
        assert!(is_enabled());
    }
}
