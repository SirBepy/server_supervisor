//! The process-wide dock bookkeeping `dock.rs`'s command surface reads and
//! writes: which proc has a live embedded/soft-docked window, and which one
//! is known window-lost. Split out of `dock.rs` once that file passed the
//! project's ~300-production-line split bar; the command surface stays
//! there since it is what every other caller imports.

use super::super::window::{DockOutcome as WindowOutcome, OriginalState, Rect};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// One proc's dock bookkeeping. `WindowLost` deliberately holds no hwnd: the
/// window that used to exist (or was expected to exist) is gone, so there is
/// nothing left to revalidate against - only a fresh `dock_window` call that
/// actually finds a window clears this back to `Active`.
pub(super) enum Entry {
    Active {
        hwnd: isize,
        original: OriginalState,
        outcome: WindowOutcome,
        target: Rect,
        /// The invisible host window this dock owns when it is headless;
        /// destroyed on undock. `None` for a dashboard-pane dock.
        headless_host: Option<isize>,
    },
    WindowLost,
}

pub(super) struct DockRegistry {
    pub(super) entries: Mutex<HashMap<String, Entry>>,
}

impl DockRegistry {
    fn new() -> Self {
        Self { entries: Mutex::new(HashMap::new()) }
    }
}

pub(super) fn registry() -> &'static DockRegistry {
    static REGISTRY: OnceLock<DockRegistry> = OnceLock::new();
    REGISTRY.get_or_init(DockRegistry::new)
}

/// Marks `proc_id` window-lost directly, bypassing the `AppHandle`-marshaled
/// entry points. Called from the re-adopt liveness poll
/// (`proc::spawn::refresh`), which runs on the reaper thread on a timer and
/// has no `SetParent`/`SetWindowPos` to marshal onto the main thread here -
/// finding no window needs no Win32 mutation, only a registry write.
///
/// A no-op when `proc_id` already holds an `Active` entry: a successful
/// embed reparents the guest to `WS_CHILD` (see `window::place::embed`),
/// which makes it invisible to any caller still probing for it with a
/// top-level `EnumWindows` scan. Overwriting `Active` with `WindowLost` on
/// that false negative would report a docked-and-fine window as lost and
/// never recover, since nothing re-attempts docking from `WindowLost`. Once
/// an embed lands, only `dock_window`/`undock_window`/`dock_state_for`'s own
/// `is_window_alive` check may decide the entry has actually gone stale.
pub(crate) fn note_window_lost(proc_id: &str) {
    let mut guard = registry().entries.lock().unwrap();
    if matches!(guard.get(proc_id), Some(Entry::Active { .. })) {
        return;
    }
    guard.insert(proc_id.to_string(), Entry::WindowLost);
}

/// The hwnd tracked by `proc_id`'s `Active` dock entry, if any, with no
/// liveness check of its own - callers that need to know whether it is
/// still real call `window::is_window_alive` on the result themselves.
/// Lets the re-adopt liveness poll (`proc::spawn::refresh`) validate a
/// docked window directly via `IsWindow` instead of an `EnumWindows` scan
/// that structurally cannot see it once it is `WS_CHILD`.
pub(crate) fn active_dock_hwnd(proc_id: &str) -> Option<isize> {
    match registry().entries.lock().unwrap().get(proc_id) {
        Some(Entry::Active { hwnd, .. }) => Some(*hwnd),
        _ => None,
    }
}

/// Clears a `WindowLost` marker once the same re-adopt liveness poll finds
/// the window again. Only removes an untouched `WindowLost` entry, never an
/// `Active` one, so this can never undo a real embed.
pub(crate) fn clear_window_lost(proc_id: &str) {
    let mut guard = registry().entries.lock().unwrap();
    if matches!(guard.get(proc_id), Some(Entry::WindowLost)) {
        guard.remove(proc_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::super::window::test_original_state;

    fn insert_active(proc_id: &str, hwnd: isize) {
        registry().entries.lock().unwrap().insert(
            proc_id.to_string(),
            Entry::Active {
                hwnd,
                original: test_original_state(),
                outcome: WindowOutcome::Embedded,
                target: Rect::default(),
                headless_host: None,
            },
        );
    }

    /// The RAII guard every test here uses to undo its own registry insert.
    /// These are the first tests in this codebase to mutate the process-wide
    /// `OnceLock<DockRegistry>`, so nothing else enforces cleanup - leaving
    /// stray entries is harmless only until a test on `release_all_docks`
    /// enumerates the whole registry and trips over another test's leftover
    /// id.
    struct RemoveOnDrop<'a>(&'a str);

    impl Drop for RemoveOnDrop<'_> {
        fn drop(&mut self) {
            registry().entries.lock().unwrap().remove(self.0);
        }
    }

    #[test]
    fn note_window_lost_does_not_clobber_an_active_entry() {
        let id = "test-dock-note-lost-active";
        let _guard = RemoveOnDrop(id);
        insert_active(id, 777);
        note_window_lost(id);
        let guard = registry().entries.lock().unwrap();
        assert!(matches!(guard.get(id), Some(Entry::Active { hwnd: 777, .. })));
    }

    #[test]
    fn note_window_lost_sets_window_lost_when_untracked() {
        let id = "test-dock-note-lost-untracked";
        let _guard = RemoveOnDrop(id);
        registry().entries.lock().unwrap().remove(id);
        note_window_lost(id);
        let guard = registry().entries.lock().unwrap();
        assert!(matches!(guard.get(id), Some(Entry::WindowLost)));
    }

    #[test]
    fn remove_on_drop_actually_removes_the_entry() {
        let id = "test-dock-remove-on-drop-proves-itself";
        {
            let _guard = RemoveOnDrop(id);
            insert_active(id, 1);
            assert!(registry().entries.lock().unwrap().contains_key(id));
        }
        assert!(!registry().entries.lock().unwrap().contains_key(id));
    }
}
