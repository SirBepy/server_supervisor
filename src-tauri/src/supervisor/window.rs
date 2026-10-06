//! Docks a supervised GUI process's window into a pane inside this app's
//! own dashboard, instead of it cluttering the dev's desktop as a separate
//! top-level window. The mechanism (`SetParent` reparenting, with a
//! `SetWindowPos`-only soft-dock fallback for windows that refuse it) was
//! proven working, including against a real Tauri/WebView2 target, by
//! `tests/embed_spike.rs` before any of this module was written; nothing
//! here is a new technique, only that spike's proven parts promoted out of
//! a throwaway test harness.
//!
//! The one behaviour every caller must design around: `tests/embed_spike.rs`'s
//! `spike_host_force_kill` proved that force-killing the host process
//! destroys the embedded guest's window while the guest's own process
//! survives untouched. A docked guest can therefore end up in a state of
//! "process alive, no window" at any time the host was not shut down
//! cleanly - callers must treat that as a real, expected state (not an
//! error to surface as a bug) and must release the dock on clean host
//! shutdown rather than relying on the guest to notice on its own.
//!
//! This module only holds the mechanism. Nothing here wires it into
//! `ProcSpec`, IPC, or the HTTP API - that is later, separate work.

pub mod capture;
mod ffi;
mod find;
mod find_windows;
mod focus;
pub mod headless_host;
mod hold;
pub mod input;
pub mod park;
mod place;

pub(crate) use hold::{holds_target, live_rect};
pub use find::{FoundWindow, find_window, find_window_once, is_window_alive};
pub(crate) use find::{descendant_pids, snapshot_parent_map};
pub use find_windows::{WindowSummary, list_windows_of};
pub(crate) use find_windows::hwnd_pid;
pub use focus::{capture_foreground, guard_focus, set_keep_focus_on_launch};
pub use place::{DockOutcome, OriginalState, PlaceError, embed, reassert, release};
pub(crate) use place::safe_restore_rect;
// Only reachable from the dock registry's own test fixtures; the real
// constructor path (`embed`'s live snapshot) stays private outside tests.
#[cfg(test)]
pub(crate) use place::test_original_state;

pub use ffi::Rect;
