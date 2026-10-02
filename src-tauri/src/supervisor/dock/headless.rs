//! Headless docking: the guest is embedded exactly like a dashboard-pane
//! dock, but into a borderless host window of its own that the dev never
//! sees. Agents reach it through the screenshot/input API, and the
//! dashboard shows a preview from the same capture.
//!
//! One host per guest, never shared: two guests overlapping inside one
//! host would leave which one a capture shows up to z-order.

use super::registry::{registry, Entry};
use super::on_main;
use crate::supervisor::window::{self, DockOutcome as WindowOutcome, Rect};
use crate::supervisor::Supervisor;
use crate::types::DockOutcome;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use tauri::AppHandle;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowRect, RegisterClassW, SetWindowPos,
    ShowWindow, HWND_BOTTOM, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNOACTIVATE,
    WNDCLASSW, WS_CLIPCHILDREN, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

/// Far outside any monitor layout, so the host is never on screen.
const PARK_AT: i32 = -32_000;
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

unsafe extern "system" fn host_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(h, m, w, l) }
}

/// Main thread only: a window's messages are dispatched by the thread that
/// created it, and only Tauri's main thread pumps messages.
fn create_host(width: i32, height: i32) -> Result<isize, String> {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    let class = w!("ServerSupervisorHeadlessHost");
    unsafe {
        let hinst = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        REGISTERED.get_or_init(|| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(host_proc),
                hInstance: hinst.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&wc);
        });
        let host = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("server_supervisor headless host"),
            WS_POPUP | WS_CLIPCHILDREN,
            PARK_AT,
            PARK_AT,
            width,
            height,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(|e| format!("could not create the headless host: {e}"))?;
        let _ = ShowWindow(host, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(host, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        Ok(host.0 as isize)
    }
}

/// Main thread only, for the same reason as `create_host`.
pub(super) fn destroy_host(host: isize) {
    unsafe {
        let _ = DestroyWindow(HWND(host as *mut _));
    }
}

fn screen_rect(host: isize) -> Rect {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(HWND(host as *mut _), &mut r);
    }
    Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

/// (proc id, pid) pairs that refused embedding, so the tick does not retry
/// the same process every second. A restart gets a new pid and a new try.
fn refused() -> &'static Mutex<HashSet<(String, u32)>> {
    static REFUSED: OnceLock<Mutex<HashSet<(String, u32)>>> = OnceLock::new();
    REFUSED.get_or_init(|| Mutex::new(HashSet::new()))
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

        let placed = on_main(app, move || {
            let host = create_host(size.0, size.1).map_err(HeadlessError::Other)?;
            let target = screen_rect(host);
            match window::embed(guest, host, target) {
                Ok((WindowOutcome::Embedded, original)) => Ok((host, target, original)),
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
            } else if !want && docked {
                if let Err(e) = self.undock_window(app, &id) {
                    log::warn!("headless undock of {id} failed: {e}");
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
