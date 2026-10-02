//! Headless docking: the guest is embedded exactly like a dashboard-pane
//! dock, but into a borderless host window of its own that the dev never
//! sees. Agents reach it through the screenshot/input API, and the
//! dashboard shows a preview from the same capture.
//!
//! One host per guest, never shared: two guests overlapping inside one
//! host would leave which one a capture shows up to z-order.

use super::registry::{registry, Entry};
use super::on_main;
// Re-exported (not just imported): `dock.rs` still calls `headless::destroy_host`
// directly, exactly as it did when this function was defined in this file.
pub(super) use crate::supervisor::window::headless_host::destroy_host;
use crate::supervisor::window::headless_host::{create_host, host_kind_for_class, screen_rect, window_class};
use crate::supervisor::window::{self, DockOutcome as WindowOutcome};
use crate::supervisor::Supervisor;
use crate::types::DockOutcome;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use tauri::AppHandle;

/// The guest keeps its own size when it has one this big; smaller (or
/// minimised, which reports a tiny rect) falls back to the default.
const MIN_DIM: i32 = 320;
const DEFAULT_SIZE: (i32, i32) = (1280, 800);

enum HeadlessError {
    NoWindowYet,
    /// The app refused `SetParent`. Soft-docking cannot stand in here:
    /// a top-level window parked off-screen is still a window on the
    /// desktop the app can move back.
    Refused,
    Other(String),
}

/// (proc id, pid) pairs that refused embedding, so the tick does not retry
/// the same process every second. A restart gets a new pid and a new try.
fn refused() -> &'static Mutex<HashSet<(String, u32)>> {
    static REFUSED: OnceLock<Mutex<HashSet<(String, u32)>>> = OnceLock::new();
    REFUSED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Whether `(proc_id, pid)` already refused headless embedding, for
/// `dock_state_for` to surface as `DockState::Refused` instead of
/// `NotDocked`.
pub(super) fn is_refused(proc_id: &str, pid: u32) -> bool {
    refused().lock().unwrap().contains(&(proc_id.to_string(), pid))
}

/// Second, independent safety net on top of this module's own park-rect
/// override in `try_dock_headless`: if a recorded original still somehow
/// sits off every monitor (the park hook was not running yet when the
/// window first appeared, or its state was lost across a restart), moves it
/// to a sane on-screen spot instead of restoring a guest to -32000 forever.
/// A no-op for every normal pane dock, whose recorded rect is always
/// on-screen already. Called by `dock`'s own release paths, not just
/// headless ones, since a stale registry entry from before this fix could
/// in principle carry an off-screen "original" for either dock kind.
pub(super) fn ensure_restorable(original: window::OriginalState) -> window::OriginalState {
    let monitors = window::park::monitor_rects();
    let primary = window::park::primary_work_area();
    let safe = window::safe_restore_rect(original.rect(), &monitors, primary);
    original.with_rect(safe)
}

fn is_headless_docked(proc_id: &str) -> bool {
    matches!(
        registry().entries.lock().unwrap().get(proc_id),
        Some(Entry::Active { hwnd, headless_host: Some(_), .. }) if window::is_window_alive(*hwnd)
    )
}

impl Supervisor {
    /// Moves `proc_id`'s window into its own invisible host. Idempotent; a
    /// proc docked into a dashboard pane is undocked from it first.
    pub fn dock_headless(&self, app: &AppHandle, proc_id: &str) -> Result<DockOutcome, String> {
        match self.try_dock_headless(app, proc_id) {
            Ok(()) => Ok(DockOutcome::Headless),
            Err(HeadlessError::NoWindowYet) => Err(format!("process {proc_id} has no window yet")),
            Err(HeadlessError::Refused) => {
                Err("this app refuses to be embedded, so it cannot run headless".to_string())
            }
            Err(HeadlessError::Other(e)) => Err(e),
        }
    }

    fn try_dock_headless(&self, app: &AppHandle, proc_id: &str) -> Result<(), HeadlessError> {
        if is_headless_docked(proc_id) {
            return Ok(());
        }
        // Pane dock, stale entry or window-lost marker: start clean.
        self.undock_window(app, proc_id).map_err(HeadlessError::Other)?;
        registry().entries.lock().unwrap().remove(proc_id);

        let pid = self.pid_for(proc_id).map_err(HeadlessError::Other)?;
        let found = window::find_window_once(pid, false).ok_or(HeadlessError::NoWindowYet)?;
        let guest = found.hwnd;
        let (w, h) = (found.rect.width(), found.rect.height());
        let size = if w >= MIN_DIM && h >= MIN_DIM { (w, h) } else { DEFAULT_SIZE };
        // Checked now, before the window is possibly moved further by
        // `embed` itself - the class never changes for a window's lifetime,
        // so reading it off-thread here (rather than inside `on_main`) is
        // safe and keeps that closure's body unchanged otherwise.
        let kind = host_kind_for_class(&window_class(guest));

        let placed = on_main(app, move || {
            let host = create_host(kind, size.0, size.1).map_err(HeadlessError::Other)?;
            let target = screen_rect(host);
            match window::embed(guest, host, target) {
                Ok((WindowOutcome::Embedded, original)) => {
                    // `embed`'s own snapshot runs AFTER `window::park`'s hook
                    // may already have moved this window to -32000 (it parks
                    // the instant the pid is in its set, well before this
                    // tick's `find_window_once` call ever sees the window) -
                    // if park recorded the true pre-park position, that is
                    // the real "original" an undock must restore to.
                    let original = match window::park::original_rect_of(guest) {
                        Some(rect) => original.with_rect(rect),
                        None => original,
                    };
                    Ok((host, target, original))
                }
                Ok((WindowOutcome::SoftDocked, original)) => {
                    let _ = window::release(guest, &original);
                    destroy_host(host);
                    Err(HeadlessError::Refused)
                }
                Err(_) => {
                    destroy_host(host);
                    Err(HeadlessError::Other("window disappeared before it could be docked".to_string()))
                }
            }
        })
        .map_err(HeadlessError::Other)?;

        let (host, target, original) = match placed {
            Ok(p) => p,
            Err(HeadlessError::Refused) => {
                refused().lock().unwrap().insert((proc_id.to_string(), pid));
                return Err(HeadlessError::Refused);
            }
            Err(e) => return Err(e),
        };
        registry().entries.lock().unwrap().insert(
            proc_id.to_string(),
            Entry::Active { hwnd: guest, original, outcome: WindowOutcome::Embedded, target, headless_host: Some(host) },
        );
        Ok(())
    }

    /// Reconciles every proc with its `dock_headless` flag. Runs on a timer
    /// from `lib.rs` rather than from the dashboard, because a headless app
    /// must leave the dev's screen whether or not the dashboard is open.
    pub fn headless_tick(&self, app: &AppHandle) {
        self.sweep_dead_headless_hosts(app);
        let procs: Vec<(String, u32, bool)> = self
            .procs
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(id, p)| p.pid.map(|pid| (id.clone(), pid, p.spec.dock_headless)))
            .collect();

        // One replace per tick, from the union of every currently
        // headless-wanted proc's full process tree - `window::park`'s hook
        // parks a new top-level window the instant its pid is in this set,
        // which is what keeps a headless app's startup window and its
        // popups/dialogs off the dev's screen without waiting on this tick
        // to find and embed anything first.
        let mut parked_pids: HashSet<u32> = HashSet::new();
        for (_, pid, want) in &procs {
            if *want {
                parked_pids.extend(window::descendant_pids(*pid));
            }
        }
        window::park::set_parked_pids(parked_pids);

        for (id, pid, want) in procs {
            let docked = is_headless_docked(&id);
            if want && !docked {
                if refused().lock().unwrap().contains(&(id.clone(), pid)) {
                    continue;
                }
                match self.try_dock_headless(app, &id) {
                    Ok(()) | Err(HeadlessError::NoWindowYet) => {}
                    Err(HeadlessError::Refused) => log::warn!("{id} refused embedding; it stays a normal window"),
                    Err(HeadlessError::Other(e)) => log::warn!("headless dock of {id} failed: {e}"),
                }
            } else if !want {
                // Forgetting the refusal lets `dock_state_for` stop reporting
                // `Refused` for a window that is now meant to be normal, and
                // gives a later re-toggle one fresh attempt.
                refused().lock().unwrap().remove(&(id.clone(), pid));
                // A popup parked off-screen while this proc was headless must
                // come back, whether or not the main window itself was ever
                // successfully docked.
                window::park::unpark_all_of(&window::descendant_pids(pid));
                if docked {
                    if let Err(e) = self.undock_window(app, &id) {
                        log::warn!("headless undock of {id} failed: {e}");
                    }
                }
            }
        }
    }

    /// Destroys hosts whose guest window is gone (the app exited or closed
    /// its window), which nothing else would ever clean up.
    fn sweep_dead_headless_hosts(&self, app: &AppHandle) {
        let dead: Vec<isize> = {
            let mut guard = registry().entries.lock().unwrap();
            let ids: Vec<String> = guard
                .iter()
                .filter(|(_, e)| {
                    matches!(e, Entry::Active { hwnd, headless_host: Some(_), .. } if !window::is_window_alive(*hwnd))
                })
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter()
                .filter_map(|id| match guard.remove(id) {
                    Some(Entry::Active { headless_host: Some(host), .. }) => Some(host),
                    _ => None,
                })
                .collect()
        };
        if !dead.is_empty() {
            let _ = on_main(app, move || dead.into_iter().for_each(destroy_host));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::proc::ManagedProc;
    use crate::types::{DockState, ProcKind, ProcSpec};

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

    // No live window involved: `dock_state_for`'s `Refused` branch only
    // consults the `refused()` set and the proc's pid, both reachable
    // without touching Win32, unlike the rest of this module.
    #[test]
    fn dock_state_for_reports_refused_after_a_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let ports = std::sync::Arc::new(crate::ports::PortRegistry::new(dir.path().to_path_buf()));
        let sup = Supervisor::new(dir.path().to_path_buf(), ports);
        let id = "headless-refused-test:cmd";
        let pid = 123_456;
        {
            let mut map = sup.procs.lock().unwrap();
            let mut p = ManagedProc::new(fast_exit_spec(id));
            p.pid = Some(pid);
            map.insert(id.to_string(), p);
        }
        refused().lock().unwrap().insert((id.to_string(), pid));

        let state = sup.dock_state_for(id);

        refused().lock().unwrap().remove(&(id.to_string(), pid));
        assert_eq!(state, Some(DockState::Refused));
    }
}
