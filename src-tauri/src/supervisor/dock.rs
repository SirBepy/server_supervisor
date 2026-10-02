//! Owns dock state: which supervised processes have their window embedded or
//! soft-docked into the dashboard, and the "process alive, window gone"
//! state a force-killed host session leaves behind (see
//! `supervisor::window`'s module docs for why that state exists and why it
//! is expected, not a bug).
//!
//! This is the only module besides `supervisor::window` itself that calls
//! into it. Every `embed`/`release`/`reassert` crosses through `on_main`, the
//! one helper below, because those three issue `SetParent`/`SetWindowPos`
//! and MUST run on the thread pumping window messages - Tauri's main thread -
//! never the worker thread a `#[tauri::command]` handler runs on; doing it
//! straight from a command deadlocks the cross-process reparent (proven live
//! while building `tests/embed_spike.rs`).
//!
//! Dock state is process-wide (one dashboard, one set of embedded panes), so
//! it lives in a private singleton in `dock::registry` rather than as a new
//! field on `Supervisor` - that would mean threading a dock-aware default
//! through every `Supervisor::new()` call site, including the many test
//! helpers across the supervisor module that construct one directly and
//! have nothing to do with docking. The public surface is still a set of
//! `impl Supervisor` methods (inherent impls don't have to live next to the
//! struct definition), so callers reach it the same way as everything else:
//! `sup.dock_window(...)`.

mod headless;
mod registry;

use super::window::{self, DockOutcome as WindowOutcome, PlaceError, Rect};
use super::Supervisor;
use crate::types::{DockOutcome, DockRect, DockState};
use registry::{registry, Entry};
use std::time::Duration;
use tauri::{AppHandle, Manager};

// `proc::spawn::refresh`'s liveness poll calls straight into the registry
// bookkeeping; re-exported here so that caller's `super::super::dock::*`
// path keeps resolving after the registry moved into its own submodule.
pub(crate) use registry::{active_dock_hwnd, clear_window_lost, note_window_lost};

/// How long `dock_window` polls for the guest's window before giving up and
/// recording the proc as window-lost. Generous because a freshly-started or
/// freshly-adopted process may still be mid-launch.
const FIND_WINDOW_TIMEOUT: Duration = Duration::from_secs(5);

fn to_window_rect(r: DockRect) -> Rect {
    Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

fn to_dock_outcome(o: WindowOutcome) -> DockOutcome {
    match o {
        WindowOutcome::Embedded => DockOutcome::Embedded,
        WindowOutcome::SoftDocked => DockOutcome::SoftDocked,
    }
}

/// Marshals a closure onto the Tauri main thread and blocks the calling
/// thread until it completes, handing back its result.
/// `AppHandle::run_on_main_thread` only takes an `FnOnce() + Send + 'static`
/// and itself returns `tauri::Result<()>` - whether the closure was
/// scheduled, not what it computed - so a channel is what gets that value
/// back out to the command/worker thread that asked for it.
fn on_main<T, F>(app: &AppHandle, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel::<T>();
    app.run_on_main_thread(move || {
        // A send error only means the caller stopped waiting; nothing to do
        // about it from inside the main-thread closure.
        let _ = tx.send(f());
    })
    .map_err(|e| format!("could not schedule work on the main thread: {e}"))?;
    rx.recv()
        .map_err(|_| "main thread closure dropped its sender without a result".to_string())
}

/// The dashboard's own top-level window, as a raw hwnd `embed` can reparent
/// a guest into. Reads a handle the windowing backend already has cached
/// (not itself a message-pump operation), so unlike `embed`/`release`/
/// `reassert` this is safe to call off the main thread.
fn host_hwnd(app: &AppHandle) -> Result<isize, String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "no main window to dock into".to_string())?;
    let hwnd = window.hwnd().map_err(|e| format!("could not read host hwnd: {e}"))?;
    Ok(hwnd.0 as isize)
}

impl Supervisor {
    /// Resolves `proc_id` to its live pid, or an error the caller can surface
    /// as-is (unknown id, or the proc simply has no running pid right now).
    pub(crate) fn pid_for(&self, proc_id: &str) -> Result<u32, String> {
        let guard = self.procs.lock().unwrap();
        let proc = guard.get(proc_id).ok_or_else(|| format!("unknown process id: {proc_id}"))?;
        proc.pid.ok_or_else(|| format!("process {proc_id} is not running"))
    }

    /// The window an agent should screenshot or drive for `proc_id`: the
    /// docked window when there is one (an embedded `WS_CHILD` is invisible
    /// to the top-level scan), else the app's own top-level window.
    pub fn window_for(&self, proc_id: &str) -> Result<isize, String> {
        if let Some(hwnd) = registry::active_dock_hwnd(proc_id) {
            if window::is_window_alive(hwnd) {
                return Ok(hwnd);
            }
        }
        let pid = self.pid_for(proc_id)?;
        window::find_window_once(pid, false)
            .map(|w| w.hwnd)
            .ok_or_else(|| format!("process {proc_id} has no window"))
    }

    /// Docks `proc_id`'s window into `target` (screen coordinates the
    /// caller's pane already computed - this module knows nothing about
    /// layout). Idempotent: a proc that is already actively docked is just
    /// reasserted into the new rect rather than re-embedded.
    pub fn dock_window(
        &self,
        app: &AppHandle,
        proc_id: &str,
        target: DockRect,
    ) -> Result<DockOutcome, String> {
        let pid = self.pid_for(proc_id)?;
        let target_rect = to_window_rect(target);
        let reg = registry();

        // Idempotent path: an existing dock whose handle is still alive just
        // gets reasserted into the new rect.
        let existing_alive = {
            let guard = reg.entries.lock().unwrap();
            match guard.get(proc_id) {
                Some(Entry::Active { hwnd, outcome, headless_host: None, .. })
                    if window::is_window_alive(*hwnd) =>
                {
                    Some((*hwnd, *outcome))
                }
                _ => None,
            }
        };
        // A headless dock lives in its own host; bring it back out before
        // embedding it into this pane.
        if matches!(reg.entries.lock().unwrap().get(proc_id), Some(Entry::Active { headless_host: Some(_), .. })) {
            self.undock_window(app, proc_id)?;
        }
        if let Some((hwnd, outcome)) = existing_alive {
            on_main(app, move || window::reassert(hwnd, target_rect))
                .map_err(|e| format!("reassert on main thread failed: {e}"))?
                .map_err(|e| format!("reassert failed: {e:?}"))?;
            if let Some(Entry::Active { target, .. }) = reg.entries.lock().unwrap().get_mut(proc_id) {
                *target = target_rect;
            }
            return Ok(to_dock_outcome(outcome));
        }
        // Either untracked, window-lost, or a stale handle (Windows recycles
        // both PIDs and HWNDs - never act on an old one). Drop whatever was
        // there and attempt a fresh dock.
        reg.entries.lock().unwrap().remove(proc_id);

        // `require_visible: false` - a tray-hidden app is still a legitimate
        // dock target, matching the attach path's own contract in `find.rs`.
        let found = window::find_window(pid, FIND_WINDOW_TIMEOUT, false);
        let found = match found {
            Some(f) => f,
            None => {
                reg.entries.lock().unwrap().insert(proc_id.to_string(), Entry::WindowLost);
                return Err(format!("no window found for pid {pid} within the poll window"));
            }
        };

        let host = host_hwnd(app)?;
        let guest = found.hwnd;
        let embed_result = on_main(app, move || window::embed(guest, host, target_rect))
            .map_err(|e| format!("embed on main thread failed: {e}"))?;

        match embed_result {
            Ok((outcome, original)) => {
                reg.entries.lock().unwrap().insert(
                    proc_id.to_string(),
                    Entry::Active { hwnd: guest, original, outcome, target: target_rect, headless_host: None },
                );
                Ok(to_dock_outcome(outcome))
            }
            Err(PlaceError::WindowGone) => {
                reg.entries.lock().unwrap().insert(proc_id.to_string(), Entry::WindowLost);
                Err("window disappeared before it could be docked".to_string())
            }
        }
    }

    /// Releases `proc_id`'s dock and restores its original window state. A
    /// no-op (not an error) when the proc isn't docked, or its handle already
    /// went stale - both are routine outcomes, not bugs, per the stale-handle
    /// discipline every entry point here follows.
    pub fn undock_window(&self, app: &AppHandle, proc_id: &str) -> Result<(), String> {
        let entry = registry().entries.lock().unwrap().remove(proc_id);
        let Some(Entry::Active { hwnd, original, headless_host, .. }) = entry else {
            return Ok(());
        };
        on_main(app, move || {
            // Release before destroying the host: destroying a parent
            // destroys its children, which would take the app's window
            // down with it.
            let released = if window::is_window_alive(hwnd) { window::release(hwnd, &original) } else { Ok(()) };
            if let Some(host) = headless_host {
                headless::destroy_host(host);
            }
            released
        })
        .map_err(|e| format!("release on main thread failed: {e}"))?
        .map_err(|e| format!("release failed: {e:?}"))
    }

    /// Re-places an already-docked proc's window into `target`, for a
    /// caller's hold-position tick (some apps move or resize themselves back
    /// after being docked). A no-op when the proc isn't actively docked -
    /// nothing to correct.
    pub fn reassert_dock(&self, app: &AppHandle, proc_id: &str, target: DockRect) -> Result<(), String> {
        let reg = registry();
        let hwnd = {
            let guard = reg.entries.lock().unwrap();
            match guard.get(proc_id) {
                Some(Entry::Active { hwnd, headless_host: None, .. }) if window::is_window_alive(*hwnd) => *hwnd,
                // Not docked, or headless: a pane rect means nothing to a
                // window living in its own off-screen host.
                _ => return Ok(()),
            }
        };
        let target_rect = to_window_rect(target);
        on_main(app, move || window::reassert(hwnd, target_rect))
            .map_err(|e| format!("reassert on main thread failed: {e}"))?
            .map_err(|e| format!("reassert failed: {e:?}"))?;
        if let Some(Entry::Active { target, .. }) = reg.entries.lock().unwrap().get_mut(proc_id) {
            *target = target_rect;
        }
        Ok(())
    }

    /// Releases every current dock. Meant for a caller already running on a
    /// worker thread (an IPC command, say) that needs the `on_main` channel
    /// marshal - see `release_all_docks_on_main_thread` for the one caller
    /// that must NOT go through that marshal.
    pub fn release_all_docks(&self, app: &AppHandle) {
        let ids: Vec<String> = registry().entries.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let _ = self.undock_window(app, &id);
        }
    }

    /// Releases every current dock directly on the calling thread, with no
    /// `AppHandle` and no `on_main` channel marshal. Callers must already be
    /// on Tauri's main thread - the same constraint every `SetParent`/
    /// `SetWindowPos` call in this module has (see the module doc). Exists
    /// for `RunEvent::ExitRequested`: that handler already runs on the main
    /// thread, so scheduling through `on_main` there would deadlock - the
    /// scheduled closure can only run once this handler returns control to
    /// the event loop, which it can't do while blocked on `on_main`'s
    /// channel waiting for that same closure.
    pub fn release_all_docks_on_main_thread(&self) {
        let ids: Vec<String> = registry().entries.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let entry = registry().entries.lock().unwrap().remove(&id);
            if let Some(Entry::Active { hwnd, original, headless_host, .. }) = entry {
                if window::is_window_alive(hwnd) {
                    let _ = window::release(hwnd, &original);
                }
                if let Some(host) = headless_host {
                    headless::destroy_host(host);
                }
            }
        }
    }

    /// Current dock state for `proc_id`, or `None` when nothing has ever
    /// been attempted for it (never docked, never window-lost) - callers
    /// that need a concrete `DockState` regardless (e.g. an IPC command)
    /// default that to `DockState::NotDocked`.
    pub fn dock_state_for(&self, proc_id: &str) -> Option<DockState> {
        match registry().entries.lock().unwrap().get(proc_id) {
            Some(Entry::Active { hwnd, headless_host: Some(_), .. }) if window::is_window_alive(*hwnd) => {
                Some(DockState::Docked { mode: DockOutcome::Headless })
            }
            Some(Entry::Active { hwnd, outcome, .. }) if window::is_window_alive(*hwnd) => {
                Some(DockState::Docked { mode: to_dock_outcome(*outcome) })
            }
            // Stale handle not yet swept by an entry point: reads as
            // untracked rather than falsely still-docked.
            Some(Entry::Active { .. }) => None,
            Some(Entry::WindowLost) => Some(DockState::WindowLost),
            None => None,
        }
    }
}
