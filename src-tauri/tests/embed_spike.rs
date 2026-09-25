// SPIKE, not a feature test. Answers one question with machine-checked
// evidence: can a running app's top-level window be SetParent-reparented
// into another process's window on Windows 11 and keep rendering, when that
// app is a GPU-composited Chromium/WebView2 app (the eventual target being
// server_supervisor docking a launched dev app inside its own dashboard)?
//
// Every function below is hand-declared via `extern "system"` instead of
// pulling in the `windows` crate. `windows` would be the right long-term
// dependency for real embedding code, but it is a heavy compile-time cost
// (codegen-heavy metadata crate) for a throwaway spike that this repo may
// delete tomorrow depending on the verdict, and the ~25 functions used here
// are simple enough that hand-declaring them is cheaper than paying that
// build cost just to find out if the idea works at all.
//
// This whole file only makes sense on Windows (Win32 APIs, GUI processes,
// an interactive desktop) so it is gated on `#[cfg(windows)]` end to end,
// and every test is `#[ignore]` because it spawns real GUI applications and
// needs a real desktop session - it must never run under the default
// `cargo test` floor. Run it explicitly:
//   cargo test --test embed_spike -- --ignored --nocapture --test-threads=1

#![cfg(windows)]
// These names deliberately mirror the real Win32 type names (HWND, RECT,
// MSG, ...) so this file reads against MSDN docs instead of a translated
// dialect - the same choice the `windows-sys` crate itself makes.
#![allow(clippy::upper_case_acronyms)]

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::rc::Rc;
use std::time::{Duration, Instant};
use std::{ptr, thread};

// ---------------------------------------------------------------------
// Win32 primitives, hand-declared. Only what this spike needs.
// ---------------------------------------------------------------------

type HWND = *mut c_void;
type HANDLE = *mut c_void;
type HINSTANCE = *mut c_void;
type HICON = *mut c_void;
type HCURSOR = *mut c_void;
type HBRUSH = *mut c_void;
type HMENU = *mut c_void;
type HDC = *mut c_void;
type HGDIOBJ = *mut c_void;
type HBITMAP = *mut c_void;
type BOOL = i32;
type LPARAM = isize;
type WPARAM = usize;
type LRESULT = isize;

type WNDPROC = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
struct WNDCLASSEXW {
    cb_size: u32,
    style: u32,
    lpfn_wnd_proc: WNDPROC,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: HINSTANCE,
    h_icon: HICON,
    h_cursor: HCURSOR,
    hbr_background: HBRUSH,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
    h_icon_sm: HICON,
}

#[repr(C)]
struct MSG {
    hwnd: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
    time: u32,
    pt: POINT,
}

// Field order/types here happen to need zero repr(C) padding (two u16s sit
// on a 4-byte boundary), so this struct's memory layout matches the real
// Win32 BITMAPINFOHEADER exactly without `packed`.
#[repr(C)]
struct BitmapInfoHeader {
    bi_size: u32,
    bi_width: i32,
    bi_height: i32,
    bi_planes: u16,
    bi_bit_count: u16,
    bi_compression: u32,
    bi_size_image: u32,
    bi_x_pels_per_meter: i32,
    bi_y_pels_per_meter: i32,
    bi_clr_used: u32,
    bi_clr_important: u32,
}

#[repr(C)]
struct Processentry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; 260],
}

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF0000;
const WS_CLIPCHILDREN: u32 = 0x0200_0000;
const WS_CHILD: u32 = 0x4000_0000;
const WS_POPUP: u32 = 0x8000_0000;
const WS_EX_LEFT: u32 = 0;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_TRANSPARENT: u32 = 0x0000_0020;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
const SW_SHOW: i32 = 5;
const SW_HIDE: i32 = 0;
const GWL_STYLE: i32 = -16;
const GWL_EXSTYLE: i32 = -20;
const GW_OWNER: u32 = 4;
// A real application main window is never this small - but this floor is
// deliberately lower than a first draft (200px either dimension) because
// live testing falsified that draft: the dev's own pomodoro-overlay and
// windows-taskbar-widgets are legitimately narrow HUD bars (200x100 and
// 468x48), and rejecting either would have thrown out the real window
// this pass exists to find. 32 sits well above the 16x16 helper surface
// that caused the original bug and well below the smallest real window
// dimension seen (48px), so it separates the two without re-admitting
// the helper.
const MIN_MAIN_WINDOW_DIM: i32 = 32;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_FRAMECHANGED: u32 = 0x0020;
const SWP_SHOWWINDOW: u32 = 0x0040;
const PM_REMOVE: u32 = 1;
const WM_DESTROY: u32 = 0x0002;
const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;
const DIB_RGB_COLORS: u32 = 0;
const BI_RGB: u32 = 0;
const SRCCOPY: u32 = 0x00CC_0020;
const STILL_ACTIVE: u32 = 259;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
// DPI_HOSTING_BEHAVIOR_MIXED. Since Windows 10 1607, SetParent fails
// outright when the parent and child threads have different DPI awareness
// contexts unless the parent's thread opts into mixed-mode hosting first.
// This is the single most likely cause of failure in this whole spike.
const DPI_HOSTING_BEHAVIOR_MIXED: i32 = 1;

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(lpwcx: *const WNDCLASSEXW) -> u16;
    fn UnregisterClassW(lp_class_name: *const u16, h_instance: HINSTANCE) -> BOOL;
    fn CreateWindowExW(
        dw_ex_style: u32,
        lp_class_name: *const u16,
        lp_window_name: *const u16,
        dw_style: u32,
        x: i32,
        y: i32,
        n_width: i32,
        n_height: i32,
        h_wnd_parent: HWND,
        h_menu: HMENU,
        h_instance: HINSTANCE,
        lp_param: *mut c_void,
    ) -> HWND;
    fn DestroyWindow(hwnd: HWND) -> BOOL;
    fn ShowWindow(hwnd: HWND, n_cmd_show: i32) -> BOOL;
    fn DefWindowProcW(hwnd: HWND, msg: u32, w_param: WPARAM, l_param: LPARAM) -> LRESULT;
    fn PostQuitMessage(n_exit_code: i32);
    fn PeekMessageW(
        lp_msg: *mut MSG,
        h_wnd: HWND,
        w_msg_filter_min: u32,
        w_msg_filter_max: u32,
        w_remove_msg: u32,
    ) -> BOOL;
    fn TranslateMessage(lp_msg: *const MSG) -> BOOL;
    fn DispatchMessageW(lp_msg: *const MSG) -> LRESULT;
    fn EnumWindows(
        lp_enum_func: unsafe extern "system" fn(HWND, LPARAM) -> BOOL,
        l_param: LPARAM,
    ) -> BOOL;
    fn GetWindowThreadProcessId(hwnd: HWND, lpdw_process_id: *mut u32) -> u32;
    fn IsWindowVisible(hwnd: HWND) -> BOOL;
    fn IsWindow(hwnd: HWND) -> BOOL;
    fn GetWindowRect(hwnd: HWND, lp_rect: *mut RECT) -> BOOL;
    fn GetClientRect(hwnd: HWND, lp_rect: *mut RECT) -> BOOL;
    fn ClientToScreen(hwnd: HWND, lp_point: *mut POINT) -> BOOL;
    fn GetWindow(hwnd: HWND, u_cmd: u32) -> HWND;
    fn GetParent(hwnd: HWND) -> HWND;
    fn SetParent(h_wnd_child: HWND, h_wnd_new_parent: HWND) -> HWND;
    fn GetWindowLongPtrW(hwnd: HWND, n_index: i32) -> isize;
    fn SetWindowLongPtrW(hwnd: HWND, n_index: i32, dw_new_long: isize) -> isize;
    fn SetWindowPos(
        hwnd: HWND,
        h_wnd_insert_after: HWND,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        u_flags: u32,
    ) -> BOOL;
    fn GetDC(hwnd: HWND) -> HDC;
    fn ReleaseDC(hwnd: HWND, hdc: HDC) -> i32;
    fn PrintWindow(hwnd: HWND, hdc_blt: HDC, n_flags: u32) -> BOOL;
    fn SetForegroundWindow(hwnd: HWND) -> BOOL;
    fn BringWindowToTop(hwnd: HWND) -> BOOL;
    fn LoadCursorW(h_instance: HINSTANCE, lpcursor_name: *const u16) -> HCURSOR;
    fn GetWindowDpiAwarenessContext(hwnd: HWND) -> HANDLE;
    fn SetThreadDpiHostingBehavior(value: i32) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateSolidBrush(color: u32) -> HBRUSH;
    fn DeleteObject(ho: HGDIOBJ) -> BOOL;
    fn CreateCompatibleDC(hdc: HDC) -> HDC;
    fn DeleteDC(hdc: HDC) -> BOOL;
    fn CreateDIBSection(
        hdc: HDC,
        pbmi: *const BitmapInfoHeader,
        usage: u32,
        ppv_bits: *mut *mut c_void,
        h_section: HANDLE,
        dw_offset: u32,
    ) -> HBITMAP;
    fn SelectObject(hdc: HDC, h: HGDIOBJ) -> HGDIOBJ;
    fn BitBlt(
        hdc_dest: HDC,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        hdc_src: HDC,
        x1: i32,
        y1: i32,
        rop: u32,
    ) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(lp_module_name: *const u16) -> HINSTANCE;
    fn GetLastError() -> u32;
    fn CloseHandle(h_object: HANDLE) -> BOOL;
    fn OpenProcess(dw_desired_access: u32, b_inherit_handle: BOOL, dw_process_id: u32) -> HANDLE;
    fn GetExitCodeProcess(h_process: HANDLE, lp_exit_code: *mut u32) -> BOOL;
    fn CreateToolhelp32Snapshot(dw_flags: u32, th32_process_id: u32) -> HANDLE;
    fn Process32FirstW(h_snapshot: HANDLE, lppe: *mut Processentry32W) -> BOOL;
    fn Process32NextW(h_snapshot: HANDLE, lppe: *mut Processentry32W) -> BOOL;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// Drains and dispatches every pending message on this thread's queue. A
// host window that never pumps messages deadlocks any cross-process
// SetParent/SetWindowPos against it, so every wait loop below calls this
// instead of a bare `thread::sleep`.
fn pump_messages() {
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn pump_for(duration: Duration) {
    let start = Instant::now();
    while start.elapsed() < duration {
        pump_messages();
        thread::sleep(Duration::from_millis(25));
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if msg == WM_DESTROY {
        PostQuitMessage(0);
        return 0;
    }
    DefWindowProcW(hwnd, msg, w_param, l_param)
}

// ---------------------------------------------------------------------
// Process tree walk (kernel32 toolhelp), so we can find a guest window
// owned by a child/grandchild process. Launchers and Chromium/Tauri
// routinely put the real UI window in a descendant process, not the PID
// std::process::Child hands back.
// ---------------------------------------------------------------------

fn snapshot_parent_map() -> HashMap<u32, u32> {
    let mut map = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() {
            return map;
        }
        let mut entry: Processentry32W = std::mem::zeroed();
        entry.dw_size = std::mem::size_of::<Processentry32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                map.insert(entry.th32_process_id, entry.th32_parent_process_id);
                entry.dw_size = std::mem::size_of::<Processentry32W>() as u32;
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    map
}

// Fixed-point iteration over the whole (pid, parent_pid) table: a single
// top-down pass can miss a grandchild whose parent entry appears later in
// the snapshot than the child's own entry, since Toolhelp does not
// guarantee any ordering.
fn descendant_pids(root: u32) -> std::collections::HashSet<u32> {
    let parent_of = snapshot_parent_map();
    let mut set = std::collections::HashSet::new();
    set.insert(root);
    loop {
        let mut grew = false;
        for (&pid, &parent) in parent_of.iter() {
            if set.contains(&parent) && !set.contains(&pid) {
                set.insert(pid);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    set
}

struct FoundWindows {
    hwnds: Vec<HWND>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let found = &mut *(lparam as *mut FoundWindows);
    found.hwnds.push(hwnd);
    1
}

/// A window that survived every rejection filter below and is eligible to
/// be picked as the guest's real main window.
struct GuestCandidate {
    hwnd: HWND,
    visible: bool,
    // pid == root_pid, i.e. owned by the app's own process rather than a
    // descendant. Chromium/WebView2 helper subprocesses (gpu-process,
    // renderer, audio service) reliably own a hidden, monitor-sized
    // compositor surface window that carries none of the TOOLWINDOW /
    // TRANSPARENT / NOACTIVATE flags and would otherwise out-area the
    // app's real window under a plain largest-wins rule - confirmed live
    // via Get-Process on the owning pid for all three attach targets.
    root_owned: bool,
    area: i64,
}

/// Narrows `candidates` to the single tier the final pick should come from:
/// visible beats hidden, and within a visibility tier, root_pid-owned beats
/// descendant-owned. Falling back to the descendant tier only when no
/// root-owned survivor exists at that visibility level keeps the spawn
/// path's legitimate case intact (a launcher/browser process handing the
/// real window to a child - see this function's own doc comment on
/// `require_visible`), while stopping a same-tier helper-process surface
/// from beating the app's own window purely on area.
fn best_tier(candidates: &[GuestCandidate]) -> Vec<&GuestCandidate> {
    for want_visible in [true, false] {
        let visible_tier: Vec<&GuestCandidate> = candidates
            .iter()
            .filter(|c| c.visible == want_visible)
            .collect();
        if visible_tier.is_empty() {
            continue;
        }
        let root_tier: Vec<&GuestCandidate> =
            visible_tier.iter().filter(|c| c.root_owned).copied().collect();
        return if root_tier.is_empty() {
            visible_tier
        } else {
            root_tier
        };
    }
    Vec::new()
}

/// Polls up to `timeout` for the guest's real top-level main window,
/// belonging to `root_pid` or any of its descendants. Shared by both the
/// spawn path (`run_spike`) and the attach path (`run_attach_spike`) so a
/// filter fix here never has to be made twice.
///
/// `require_visible` is the one deliberate difference between the two
/// callers: the spawn path just launched the process and can keep polling
/// until a window appears on screen, so it passes `true`. The attach path
/// is reaching for an app the dev already has running, which may be hidden
/// to the tray - that hidden window is still the legitimate embed target,
/// so it passes `false` and accepts a non-visible candidate.
///
/// Every candidate considered is logged with why it was rejected or kept,
/// and the caller learns whether the window it got back was hidden
/// (`was_hidden` in the returned tuple) since the wrong-window bug this
/// replaces was a silent wrong pick.
fn find_guest_window(root_pid: u32, timeout: Duration, require_visible: bool) -> Option<(HWND, bool)> {
    let start = Instant::now();
    loop {
        pump_messages();
        let descendants = descendant_pids(root_pid);
        let mut found = FoundWindows { hwnds: Vec::new() };
        unsafe {
            EnumWindows(enum_proc, &mut found as *mut FoundWindows as LPARAM);
        }

        let mut candidates: Vec<GuestCandidate> = Vec::new();
        for hwnd in found.hwnds {
            unsafe {
                let mut pid: u32 = 0;
                GetWindowThreadProcessId(hwnd, &mut pid);
                // Windows belonging to an unrelated process are not this
                // guest's helper surfaces or main window - skip silently,
                // logging every one of them would drown the real output in
                // noise from the rest of the desktop.
                if !descendants.contains(&pid) {
                    continue;
                }

                let visible = IsWindowVisible(hwnd) != 0;
                let mut rect = RECT::default();
                GetWindowRect(hwnd, &mut rect);
                let width = rect.right - rect.left;
                let height = rect.bottom - rect.top;
                let exstyle = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
                let has_owner = !GetWindow(hwnd, GW_OWNER).is_null();

                let mut reasons: Vec<&str> = Vec::new();
                if require_visible && !visible {
                    reasons.push("not visible");
                }
                if has_owner {
                    reasons.push("has GW_OWNER");
                }
                if width < MIN_MAIN_WINDOW_DIM || height < MIN_MAIN_WINDOW_DIM {
                    reasons.push("too small");
                }
                if exstyle & WS_EX_TOOLWINDOW != 0 {
                    reasons.push("WS_EX_TOOLWINDOW");
                }
                if exstyle & WS_EX_TRANSPARENT != 0 {
                    reasons.push("WS_EX_TRANSPARENT");
                }
                if exstyle & WS_EX_NOACTIVATE != 0 {
                    reasons.push("WS_EX_NOACTIVATE");
                }

                if !reasons.is_empty() {
                    println!(
                        "[finder] pid={pid} hwnd={} {width}x{height} ex=0x{exstyle:08X} REJECTED ({})",
                        hwnd as usize,
                        reasons.join(", ")
                    );
                    continue;
                }

                println!(
                    "[finder] pid={pid} hwnd={} {width}x{height} ex=0x{exstyle:08X} CANDIDATE{}",
                    hwnd as usize,
                    if visible { "" } else { " (hidden)" }
                );
                candidates.push(GuestCandidate {
                    hwnd,
                    visible,
                    root_owned: pid == root_pid,
                    area: (width as i64) * (height as i64),
                });
            }
        }

        // Largest-by-area alone is not enough: a WebView2/Chromium helper
        // subprocess can own a full-screen invisible compositor surface
        // (confirmed live via Get-Process on the owning pid, across all
        // three attach targets) that carries none of the reject flags above
        // and outsizes the app's real window, which would otherwise win
        // outright under a plain largest-wins rule. `best_tier` picks
        // visible-and-root-owned first and only relaxes one axis at a time;
        // "largest by area" then only breaks ties inside that tier.
        let pool = best_tier(&candidates);
        if let Some(best) = pool.iter().max_by_key(|c| c.area) {
            println!(
                "[finder] chosen hwnd={} (largest of {} {}{}candidate{})",
                best.hwnd as usize,
                pool.len(),
                if best.visible { "visible " } else { "hidden " },
                if best.root_owned { "root-owned " } else { "descendant " },
                if pool.len() == 1 { "" } else { "s" }
            );
            return Some((best.hwnd, !best.visible));
        }

        if start.elapsed() >= timeout {
            return None;
        }
        thread::sleep(Duration::from_millis(150));
    }
}

// ---------------------------------------------------------------------
// BMP writer (hand-rolled: BITMAPFILEHEADER + BITMAPINFOHEADER + bottom-up
// BGRA rows). No image crate - this is a throwaway spike artifact.
// ---------------------------------------------------------------------

fn write_bmp(path: &Path, width: i32, height: i32, top_down_bgra: &[u8]) -> std::io::Result<()> {
    let row_bytes = (width as usize) * 4;
    let data_len = row_bytes * (height as usize);
    let file_header_len = 14u32;
    let info_header_len = 40u32;

    let mut buf = Vec::with_capacity((file_header_len + info_header_len) as usize + data_len);
    // BITMAPFILEHEADER
    buf.extend_from_slice(b"BM");
    buf.extend_from_slice(&(file_header_len + info_header_len + data_len as u32).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes()); // reserved1
    buf.extend_from_slice(&0u16.to_le_bytes()); // reserved2
    buf.extend_from_slice(&(file_header_len + info_header_len).to_le_bytes()); // bfOffBits

    // BITMAPINFOHEADER. Positive height marks this as bottom-up, the
    // standard BMP-on-disk convention, even though our in-memory capture
    // buffer (from a top-down CreateDIBSection) is top-down - the row loop
    // below does the flip while writing.
    buf.extend_from_slice(&info_header_len.to_le_bytes());
    buf.extend_from_slice(&width.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // planes
    buf.extend_from_slice(&32u16.to_le_bytes()); // bit count
    buf.extend_from_slice(&BI_RGB.to_le_bytes());
    buf.extend_from_slice(&(data_len as u32).to_le_bytes());
    buf.extend_from_slice(&0i32.to_le_bytes());
    buf.extend_from_slice(&0i32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());

    for y in (0..height as usize).rev() {
        let start = y * row_bytes;
        buf.extend_from_slice(&top_down_bgra[start..start + row_bytes]);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(&buf)
}

// ---------------------------------------------------------------------
// The verdict, and the shared routine that produces one.
// ---------------------------------------------------------------------

#[derive(Default)]
struct Verdict {
    setparent_ok: bool,
    setparent_last_error: u32,
    parent_matches: bool,
    guest_alive: bool,
    // Only meaningful on the attach path: the spawn path requires
    // visibility as a finder precondition, so it is always false there.
    guest_was_hidden: bool,
    rect_inside_host: bool,
    capture_method: String,
    distinct_colours: usize,
    dominant_colour_pct: f64,
    pixel_content_ok: bool,
    release_restored_ok: bool,
    host_dpi_context: isize,
    guest_dpi_context: isize,
    dpi_hosting_behavior_set: bool,
    bitmap: Option<PathBuf>,
    // Set when a host or guest window vanished mid-test for a reason that
    // has nothing to do with SetParent/rendering - most likely someone at
    // the keyboard closing one of the real windows this spike pops up.
    // This must never collapse into a plain FAIL: a stray click producing
    // "guest window destroyed" would otherwise read as damning evidence
    // against the embedding mechanism when it is really just a closed
    // window, so it gets its own INVALID outcome and an automatic retry.
    invalid_reason: Option<String>,
}

impl Verdict {
    fn overall_pass(&self) -> bool {
        self.setparent_ok
            && self.parent_matches
            && self.guest_alive
            && self.rect_inside_host
            && self.pixel_content_ok
            && self.release_restored_ok
    }

    fn print(&self, target_name: &str) {
        println!("===== EMBED SPIKE VERDICT: {target_name} =====");
        println!("setparent_ok:            {}", self.setparent_ok);
        println!("setparent_last_error:    {}", self.setparent_last_error);
        println!("parent_matches:          {}", self.parent_matches);
        println!("guest_alive:             {}", self.guest_alive);
        println!("guest_was_hidden:        {}", self.guest_was_hidden);
        println!("rect_inside_host:        {}", self.rect_inside_host);
        println!("capture_method:          {}", self.capture_method);
        println!("distinct_colours:        {}", self.distinct_colours);
        println!("dominant_colour_pct:     {:.1}", self.dominant_colour_pct);
        println!("pixel_content_ok:        {}", self.pixel_content_ok);
        println!("release_restored_ok:     {}", self.release_restored_ok);
        println!("host_dpi_context:        {}", self.host_dpi_context);
        println!("guest_dpi_context:       {}", self.guest_dpi_context);
        println!(
            "dpi_hosting_behavior_set: {}",
            self.dpi_hosting_behavior_set
        );
        match &self.bitmap {
            Some(p) => println!("bitmap:                  {}", p.display()),
            None => println!("bitmap:                  (none captured)"),
        }
        let overall = match &self.invalid_reason {
            Some(reason) => format!("INVALID ({reason})"),
            None => (if self.overall_pass() { "PASS" } else { "FAIL" }).to_string(),
        };
        println!("OVERALL:                 {overall}");
        println!("=====================================================");
    }
}

/// True if both windows are still alive. If not, records a distinct
/// INVALID verdict naming which one vanished, rather than letting the
/// caller's next assertion (GetParent, IsWindow, pixel capture) fail for a
/// reason unrelated to the embedding mechanism under test.
fn still_alive_or_invalidate(verdict: &mut Verdict, host: HWND, guest: HWND, stage: &str) -> bool {
    let host_ok = unsafe { IsWindow(host) } != 0;
    let guest_ok = unsafe { IsWindow(guest) } != 0;
    if host_ok && guest_ok {
        return true;
    }
    let which = match (host_ok, guest_ok) {
        (false, false) => "both the host and guest windows",
        (false, true) => "the host window",
        (true, false) => "the guest window",
        (true, true) => unreachable!("checked above"),
    };
    verdict.invalid_reason = Some(format!(
        "{which} vanished externally during {stage} - not a mechanism result, needs a rerun"
    ));
    false
}

/// Guarantees the guest process tree is killed and the host window/class
/// are torn down even if an assertion panics partway through - leaking a
/// GUI process here is the exact failure mode this whole project exists to
/// prevent.
struct Teardown {
    host: HWND,
    class_name: Vec<u16>,
    h_instance: HINSTANCE,
    guest_pid: Option<u32>,
}

impl Drop for Teardown {
    fn drop(&mut self) {
        if let Some(pid) = self.guest_pid.take() {
            let _ = Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .output();
        }
        unsafe {
            if !self.host.is_null() {
                DestroyWindow(self.host);
            }
            UnregisterClassW(self.class_name.as_ptr(), self.h_instance);
        }
    }
}

fn screen_dimensions_ok(rect: &RECT) -> bool {
    rect.right - rect.left > 0 && rect.bottom - rect.top > 0
}

/// One capture attempt: either `PrintWindow(PW_RENDERFULLCONTENT)` (the
/// flag that exists specifically to pull DirectComposition-rendered
/// content - the older flag returns black for it) or a screen `BitBlt` of
/// the host's rect. Returns a top-down 32bpp BGRA buffer sized
/// `width * height * 4`.
fn capture_frame(
    host: HWND,
    host_rect: RECT,
    width: i32,
    height: i32,
    use_printwindow: bool,
) -> Option<Vec<u8>> {
    unsafe {
        let screen_dc = GetDC(ptr::null_mut());
        if screen_dc.is_null() {
            return None;
        }
        let mem_dc = CreateCompatibleDC(screen_dc);
        if mem_dc.is_null() {
            ReleaseDC(ptr::null_mut(), screen_dc);
            return None;
        }

        let bmi = BitmapInfoHeader {
            bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
            bi_width: width,
            // Negative height: a top-down DIB, so pixel row 0 in memory is
            // the top row of the window - convenient for sub-rect slicing
            // later. Flipped back to bottom-up only when written to disk.
            bi_height: -height,
            bi_planes: 1,
            bi_bit_count: 32,
            bi_compression: BI_RGB,
            bi_size_image: 0,
            bi_x_pels_per_meter: 0,
            bi_y_pels_per_meter: 0,
            bi_clr_used: 0,
            bi_clr_important: 0,
        };
        let mut bits: *mut c_void = ptr::null_mut();
        let dib = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
        if dib.is_null() || bits.is_null() {
            DeleteDC(mem_dc);
            ReleaseDC(ptr::null_mut(), screen_dc);
            return None;
        }
        let old = SelectObject(mem_dc, dib);

        let ok = if use_printwindow {
            PrintWindow(host, mem_dc, PW_RENDERFULLCONTENT) != 0
        } else {
            // The host is visible and near the top-left, so a raw screen
            // BitBlt works as long as nothing occludes it - bring it to
            // the foreground first to make that true.
            SetForegroundWindow(host);
            BringWindowToTop(host);
            pump_for(Duration::from_millis(300));
            BitBlt(
                mem_dc,
                0,
                0,
                width,
                height,
                screen_dc,
                host_rect.left,
                host_rect.top,
                SRCCOPY,
            ) != 0
        };

        let result = if ok {
            let len = (width as usize) * (height as usize) * 4;
            Some(std::slice::from_raw_parts(bits as *const u8, len).to_vec())
        } else {
            None
        };

        SelectObject(mem_dc, old);
        DeleteObject(dib);
        DeleteDC(mem_dc);
        ReleaseDC(ptr::null_mut(), screen_dc);
        result
    }
}

/// Tries PrintWindow first, falling back to a screen BitBlt if the API
/// call itself fails outright (not yet checking for a blank-but-successful
/// capture - that uniformity check happens in the caller, since it needs
/// to inspect only the guest's sub-rectangle, not the whole host).
fn capture_host(host: HWND, host_rect: RECT) -> Option<(String, i32, i32, Vec<u8>)> {
    let width = host_rect.right - host_rect.left;
    let height = host_rect.bottom - host_rect.top;
    if width <= 0 || height <= 0 {
        return None;
    }
    if let Some(pixels) = capture_frame(host, host_rect, width, height, true) {
        return Some((
            "PrintWindow(PW_RENDERFULLCONTENT)".to_string(),
            width,
            height,
            pixels,
        ));
    }
    capture_frame(host, host_rect, width, height, false)
        .map(|pixels| ("BitBlt(screen)".to_string(), width, height, pixels))
}

/// Counts distinct BGR colours (alpha ignored - DIB alpha is meaningless
/// for GDI-rendered content) within `sub` (host-window-relative pixel
/// coordinates) and returns (distinct_count, dominant_count, total_count).
fn analyze_region(pixels: &[u8], stride_width: i32, sub: RECT) -> (usize, u32, u32) {
    let mut counts: HashMap<u32, u32> = HashMap::new();
    let mut total = 0u32;
    for y in sub.top..sub.bottom {
        for x in sub.left..sub.right {
            let idx = ((y as usize) * (stride_width as usize) + (x as usize)) * 4;
            if idx + 3 >= pixels.len() {
                continue;
            }
            let b = pixels[idx] as u32;
            let g = pixels[idx + 1] as u32;
            let r = pixels[idx + 2] as u32;
            let colour = (r << 16) | (g << 8) | b;
            *counts.entry(colour).or_insert(0) += 1;
            total += 1;
        }
    }
    let dominant = counts.values().copied().max().unwrap_or(0);
    (counts.len(), dominant, total)
}

/// The whole spike: create a host window, spawn the guest via `spawn`,
/// find its top-level window (including descendant processes), reparent
/// it into the host, verify, capture a bitmap, then restore and tear down.
/// Every assertion is recorded rather than early-returned on, since a
/// partial result (e.g. SetParent works but the round-trip restore does
/// not) is itself a useful answer.
fn run_spike<F>(target_name: &str, screenshot_dir: &Path, mut spawn: F) -> Verdict
where
    F: FnMut() -> std::io::Result<Child>,
{
    let mut verdict = Verdict::default();

    unsafe {
        verdict.dpi_hosting_behavior_set =
            SetThreadDpiHostingBehavior(DPI_HOSTING_BEHAVIOR_MIXED) != -1;
    }
    if !verdict.dpi_hosting_behavior_set {
        println!(
            "[{target_name}] WARNING: SetThreadDpiHostingBehavior(MIXED) did not report success; \
             continuing anyway, but this is the first place to look if SetParent fails."
        );
    }

    let class_name = wide("EmbedSpikeHostWindowClass");
    let h_instance = unsafe { GetModuleHandleW(ptr::null()) };
    let brush = unsafe { CreateSolidBrush(0x00FF00FF) }; // magenta, BGR 0x00BBGGRR

    let wc = WNDCLASSEXW {
        cb_size: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfn_wnd_proc: wndproc,
        cb_cls_extra: 0,
        cb_wnd_extra: 0,
        h_instance,
        h_icon: ptr::null_mut(),
        h_cursor: unsafe { LoadCursorW(ptr::null_mut(), 32512usize as *const u16) }, // IDC_ARROW
        hbr_background: brush,
        lpsz_menu_name: ptr::null(),
        lpsz_class_name: class_name.as_ptr(),
        h_icon_sm: ptr::null_mut(),
    };
    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        println!(
            "[{target_name}] RegisterClassExW failed, GetLastError={}",
            unsafe { GetLastError() }
        );
        return verdict;
    }

    let title = wide("Embed Spike Host");
    let host = unsafe {
        CreateWindowExW(
            WS_EX_LEFT,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            50,
            50,
            1200,
            820,
            ptr::null_mut(),
            ptr::null_mut(),
            h_instance,
            ptr::null_mut(),
        )
    };

    let mut teardown = Teardown {
        host,
        class_name: class_name.clone(),
        h_instance,
        guest_pid: None,
    };

    if host.is_null() {
        println!(
            "[{target_name}] CreateWindowExW(host) failed, GetLastError={}",
            unsafe { GetLastError() }
        );
        return verdict;
    }
    unsafe {
        ShowWindow(host, SW_SHOW);
    }
    pump_for(Duration::from_millis(200));

    verdict.host_dpi_context = unsafe { GetWindowDpiAwarenessContext(host) } as isize;

    // Step 2: launch the guest, then poll (across descendant processes,
    // since launchers/Chromium/Tauri commonly hand the real window to a
    // child) for its main window.
    let child = match spawn() {
        Ok(c) => c,
        Err(e) => {
            println!("[{target_name}] failed to spawn guest process: {e}");
            return verdict;
        }
    };
    let root_pid = child.id();
    teardown.guest_pid = Some(root_pid);

    // The spawn path just launched this process, so it requires visibility
    // (`require_visible: true`) rather than accepting a not-yet-shown
    // window - see `find_guest_window`'s doc comment for why the attach
    // path below differs.
    let guest = match find_guest_window(root_pid, Duration::from_secs(45), true) {
        Some((h, _was_hidden)) => h,
        None => {
            println!(
                "[{target_name}] no visible, unowned, non-zero-area top-level window found for \
                 pid {root_pid} or its descendants within 45s"
            );
            return verdict;
        }
    };

    let mut guest_pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(guest, &mut guest_pid);
    }
    verdict.guest_dpi_context = unsafe { GetWindowDpiAwarenessContext(guest) } as isize;

    if !still_alive_or_invalidate(&mut verdict, host, guest, "window discovery") {
        return verdict;
    }

    // Step 3: record original state so it can be restored later.
    let orig_style = unsafe { GetWindowLongPtrW(guest, GWL_STYLE) };
    let orig_exstyle = unsafe { GetWindowLongPtrW(guest, GWL_EXSTYLE) };
    let orig_parent = unsafe { GetParent(guest) };
    let mut orig_rect = RECT::default();
    unsafe {
        GetWindowRect(guest, &mut orig_rect);
    }

    // Step 4: embed. Checked immediately before SetParent, since a host
    // destroyed a moment earlier (e.g. a stray click) makes SetParent fail
    // with ERROR_INVALID_WINDOW_HANDLE - a misleading result that has
    // nothing to do with the DPI-hosting mechanism this call is testing.
    if !still_alive_or_invalidate(&mut verdict, host, guest, "just before SetParent") {
        return verdict;
    }
    let new_style = ((orig_style as u32) & !WS_OVERLAPPEDWINDOW & !WS_POPUP) | WS_CHILD;
    unsafe {
        SetWindowLongPtrW(guest, GWL_STYLE, new_style as isize);
    }
    let set_parent_result = unsafe { SetParent(guest, host) };
    verdict.setparent_ok = !set_parent_result.is_null();
    if !verdict.setparent_ok {
        verdict.setparent_last_error = unsafe { GetLastError() };
    }

    // Inset rect: a visible margin of host background on all sides proves
    // the guest is genuinely a child inside the host, not just overlapping
    // it - the bitmap would look identical either way without this margin.
    let margin = 40;
    let mut host_client = RECT::default();
    unsafe {
        GetClientRect(host, &mut host_client);
    }
    let inset_x = margin;
    let inset_y = margin;
    let inset_w = (host_client.right - host_client.left - 2 * margin).max(1);
    let inset_h = (host_client.bottom - host_client.top - 2 * margin).max(1);

    unsafe {
        SetWindowPos(
            guest,
            ptr::null_mut(),
            inset_x,
            inset_y,
            inset_w,
            inset_h,
            SWP_FRAMECHANGED | SWP_SHOWWINDOW | SWP_NOZORDER,
        );
    }
    pump_for(Duration::from_secs(3));

    // The main checkpoint: this is the point in the coordinator's report
    // where a hand-closed window would otherwise surface as a plain,
    // misleading FAIL on parent_matches/guest_alive/rect_inside_host.
    if !still_alive_or_invalidate(&mut verdict, host, guest, "post-embed settle") {
        return verdict;
    }

    verdict.parent_matches = unsafe { GetParent(guest) } == host;
    let is_window_after_embed = unsafe { IsWindow(guest) } != 0;

    // Guest-alive check via OpenProcess/GetExitCodeProcess on the PID that
    // actually owns the window (may differ from root_pid - see the
    // descendant-process comment above find_guest_window).
    verdict.guest_alive = unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, guest_pid);
        if h.is_null() {
            false
        } else {
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE
        }
    } && is_window_after_embed;

    // rect_inside_host: compare guest's screen rect against the host's
    // client rect converted to screen coordinates.
    let mut host_client_screen_tl = POINT {
        x: host_client.left,
        y: host_client.top,
    };
    let mut host_client_screen_br = POINT {
        x: host_client.right,
        y: host_client.bottom,
    };
    unsafe {
        ClientToScreen(host, &mut host_client_screen_tl);
        ClientToScreen(host, &mut host_client_screen_br);
    }
    let mut guest_rect_after = RECT::default();
    unsafe {
        GetWindowRect(guest, &mut guest_rect_after);
    }
    verdict.rect_inside_host = guest_rect_after.left >= host_client_screen_tl.x
        && guest_rect_after.top >= host_client_screen_tl.y
        && guest_rect_after.right <= host_client_screen_br.x
        && guest_rect_after.bottom <= host_client_screen_br.y
        && screen_dimensions_ok(&guest_rect_after);

    if !still_alive_or_invalidate(&mut verdict, host, guest, "just before pixel capture") {
        return verdict;
    }

    // Step 5's load-bearing check: capture the host's pixels and inspect
    // the sub-region the guest occupies.
    let mut host_rect_screen = RECT::default();
    unsafe {
        GetWindowRect(host, &mut host_rect_screen);
    }
    if let Some((method, width, height, pixels)) = capture_host(host, host_rect_screen) {
        // Guest sub-rect in host-window-relative pixel coordinates (the
        // bitmap is 1:1 with the host's full window rect, frame included,
        // since PrintWindow/BitBlt both capture at that origin).
        let sub = RECT {
            left: (guest_rect_after.left - host_rect_screen.left).max(0),
            top: (guest_rect_after.top - host_rect_screen.top).max(0),
            right: (guest_rect_after.right - host_rect_screen.left).min(width),
            bottom: (guest_rect_after.bottom - host_rect_screen.top).min(height),
        };
        let (distinct, dominant, total) = analyze_region(&pixels, width, sub);
        verdict.capture_method = method.clone();
        verdict.distinct_colours = distinct;
        verdict.dominant_colour_pct = if total > 0 {
            (dominant as f64 / total as f64) * 100.0
        } else {
            100.0
        };

        // If PrintWindow's capture of the guest sub-region came back
        // uniform, retry with a screen BitBlt before giving up - a blank
        // PrintWindow surface is a known DWM/composition timing issue
        // distinct from "the guest truly isn't rendering".
        let mut final_pixels = pixels;
        if method.starts_with("PrintWindow") && distinct <= 1 {
            if let Some(retry_pixels) = capture_frame(host, host_rect_screen, width, height, false)
            {
                let (d2, dom2, tot2) = analyze_region(&retry_pixels, width, sub);
                verdict.capture_method = "BitBlt(screen)".to_string();
                verdict.distinct_colours = d2;
                verdict.dominant_colour_pct = if tot2 > 0 {
                    (dom2 as f64 / tot2 as f64) * 100.0
                } else {
                    100.0
                };
                final_pixels = retry_pixels;
            }
        }

        let bmp_path = screenshot_dir.join(format!("embed-spike-{target_name}.bmp"));
        if write_bmp(&bmp_path, width, height, &final_pixels).is_ok() {
            println!("[{target_name}] bitmap written to {}", bmp_path.display());
            verdict.bitmap = Some(bmp_path);
        }

        // PASS threshold as specified: more than 16 distinct colours and
        // the dominant colour under 95% of the region. A uniform region
        // (black or white) means the content did not render at all.
        verdict.pixel_content_ok =
            verdict.distinct_colours > 16 && verdict.dominant_colour_pct < 95.0;
    } else {
        verdict.capture_method = "capture failed (both PrintWindow and BitBlt)".to_string();
    }

    if !still_alive_or_invalidate(&mut verdict, host, guest, "just before restore") {
        return verdict;
    }

    // Step 6: release and restore.
    unsafe {
        SetWindowLongPtrW(guest, GWL_STYLE, orig_style);
        SetWindowLongPtrW(guest, GWL_EXSTYLE, orig_exstyle);
        SetParent(guest, orig_parent);
        SetWindowPos(
            guest,
            ptr::null_mut(),
            orig_rect.left,
            orig_rect.top,
            orig_rect.right - orig_rect.left,
            orig_rect.bottom - orig_rect.top,
            SWP_FRAMECHANGED,
        );
    }
    pump_for(Duration::from_millis(500));
    let parent_after_restore = unsafe { GetParent(guest) };
    let still_window_after_restore = unsafe { IsWindow(guest) } != 0;
    verdict.release_restored_ok = parent_after_restore == orig_parent && still_window_after_restore;

    verdict
}

/// Runs the spike for one target up to 3 attempts (1 + 2 retries). An
/// INVALID result (a window vanished for a reason unrelated to the
/// embedding mechanism, e.g. someone closing it by hand mid-test) is
/// retried automatically; a PASS or a genuine FAIL is returned immediately.
/// `spawn` must be repeatable (FnMut), since a retry launches a fresh
/// guest process.
fn run_spike_with_retries<F>(target_name: &str, screenshot_dir: &Path, mut spawn: F) -> Verdict
where
    F: FnMut() -> std::io::Result<Child>,
{
    let mut last = Verdict::default();
    for attempt in 1..=3 {
        let verdict = run_spike(target_name, screenshot_dir, &mut spawn);
        match &verdict.invalid_reason {
            Some(reason) => {
                println!("[{target_name}] attempt {attempt}/3 was INVALID: {reason}");
                last = verdict;
            }
            None => return verdict,
        }
    }
    last
}

fn screenshot_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent()
        .expect("src-tauri has a parent (the repo root)")
        .join(".for_bepy")
        .join("screenshots")
        .join("34376-134348148554153839")
}

// ---------------------------------------------------------------------
// Target 1: notepad.exe. Plain GDI window, no compositor tricks. If this
// fails, the harness itself is broken - debug this one first, always.
// ---------------------------------------------------------------------

#[test]
#[ignore]
fn spike_notepad() {
    let dir = screenshot_dir();
    let verdict = run_spike_with_retries("notepad", &dir, || Command::new("notepad.exe").spawn());
    verdict.print("notepad");
}

// ---------------------------------------------------------------------
// Target 2: Edge in app mode. Same Chromium/DirectComposition compositor
// architecture as WebView2, reachable with no build step.
// ---------------------------------------------------------------------

fn find_msedge() -> Option<PathBuf> {
    for candidate in [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ] {
        let p = PathBuf::from(candidate);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[test]
#[ignore]
fn spike_edge_app_window() {
    let msedge = match find_msedge() {
        Some(p) => p,
        None => {
            println!(
                "SKIPPED spike_edge_app_window: msedge.exe not found at either usual install path"
            );
            return;
        }
    };

    let html_dir = std::env::temp_dir().join("embed_spike_edge_html");
    let _ = std::fs::create_dir_all(&html_dir);
    let html_path = html_dir.join("spike.html");
    let html = r#"<!doctype html><html><body style="margin:0">
<div style="display:flex;height:100vh;width:100vw">
  <div style="flex:1;background:#e6194b"></div>
  <div style="flex:1;background:#3cb44b"></div>
  <div style="flex:1;background:#4363d8"></div>
  <div style="flex:1;background:#ffe119"></div>
</div>
<div style="position:absolute;top:40%;left:10%;font-size:64px;font-family:sans-serif;color:white">
EMBED SPIKE
</div>
</body></html>"#;
    std::fs::write(&html_path, html).expect("write temp html for edge spike");

    // A fresh --user-data-dir per run is required: without one, this
    // launch hands off to any already-running Edge process and our
    // spawned PID exits immediately with no window of its own.
    let user_data_dir =
        std::env::temp_dir().join(format!("embed_spike_edge_profile_{}", std::process::id()));

    let dir = screenshot_dir();
    let html_url = format!("file:///{}", html_path.to_string_lossy().replace('\\', "/"));
    let verdict = run_spike_with_retries("edge_app_window", &dir, || {
        Command::new(&msedge)
            .arg(format!("--app={html_url}"))
            .arg(format!("--user-data-dir={}", user_data_dir.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--window-size=900,600")
            .spawn()
    });
    verdict.print("edge_app_window");
}

// ---------------------------------------------------------------------
// Target 3: this repo's own Tauri binary - the real target, since
// server_supervisor is itself a Tauri + WebView2 app.
// ---------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent (the repo root)")
        .to_path_buf()
}

/// Polls a spawned build command until it exits or `timeout` elapses,
/// killing it on timeout. `Child::wait` has no built-in deadline, so this
/// is the only way to bound an `npm`/`cargo` subprocess inside a test.
fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<bool, String> {
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    return Err(format!("timed out after {timeout:?}"));
                }
                thread::sleep(Duration::from_millis(500));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn any_process_named(exe_name_lower: &str) -> bool {
    let parent_of = snapshot_parent_map();
    // snapshot_parent_map only records pid->parent; re-walk the snapshot
    // for exe names directly since that is what we actually need here.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() {
            return false;
        }
        let mut entry: Processentry32W = std::mem::zeroed();
        entry.dw_size = std::mem::size_of::<Processentry32W>() as u32;
        let mut found = false;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let name = String::from_utf16_lossy(
                    &entry.sz_exe_file[..entry
                        .sz_exe_file
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.sz_exe_file.len())],
                );
                if name.to_lowercase() == exe_name_lower {
                    found = true;
                    break;
                }
                entry.dw_size = std::mem::size_of::<Processentry32W>() as u32;
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        let _ = parent_of;
        found
    }
}

fn cargo_target_directory(root: &Path) -> Option<PathBuf> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--manifest-path",
            "src-tauri/Cargo.toml",
            "--format-version",
            "1",
        ])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // `cargo metadata` output is already valid JSON, and serde_json is
    // already a dev-dependency of this crate (used elsewhere for API
    // tests) - parsing a field out of it here adds no new dependency.
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("target_directory")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
}

#[test]
#[ignore]
fn spike_tauri_webview2() {
    if any_process_named("server_supervisor.exe") {
        println!(
            "SKIPPED spike_tauri_webview2: an instance of server_supervisor is already running \
             (tauri-plugin-single-instance would hand off to it and this spawn would exit \
             immediately with no window). Close the running supervisor first and rerun."
        );
        return;
    }

    let root = repo_root();

    if !root.join("node_modules").is_dir() {
        println!("spike_tauri_webview2: node_modules missing, running npm install");
        match run_with_timeout(
            Command::new("npm.cmd").arg("install").current_dir(&root),
            Duration::from_secs(600),
        ) {
            Ok(true) => {}
            other => {
                println!(
                    "SKIPPED spike_tauri_webview2: npm install failed or timed out: {other:?}"
                );
                return;
            }
        }
    }

    println!("spike_tauri_webview2: running npm run build");
    match run_with_timeout(
        Command::new("npm.cmd")
            .args(["run", "build"])
            .current_dir(&root),
        Duration::from_secs(600),
    ) {
        Ok(true) => {}
        other => {
            println!("SKIPPED spike_tauri_webview2: npm run build failed or timed out: {other:?}");
            return;
        }
    }

    println!("spike_tauri_webview2: running cargo build --manifest-path src-tauri/Cargo.toml");
    match run_with_timeout(
        Command::new("cargo")
            .args(["build", "--manifest-path", "src-tauri/Cargo.toml"])
            .current_dir(&root),
        Duration::from_secs(600),
    ) {
        Ok(true) => {}
        other => {
            println!("SKIPPED spike_tauri_webview2: cargo build failed or timed out: {other:?}");
            return;
        }
    }

    let target_dir = match cargo_target_directory(&root) {
        Some(d) => d,
        None => {
            println!("SKIPPED spike_tauri_webview2: could not resolve cargo target_directory via `cargo metadata`");
            return;
        }
    };
    let candidates = [
        target_dir.join("debug").join("server_supervisor.exe"),
        root.join("src-tauri")
            .join("target")
            .join("debug")
            .join("server_supervisor.exe"),
    ];
    let exe = match candidates.iter().find(|p| p.is_file()) {
        Some(p) => p.clone(),
        None => {
            println!(
                "SKIPPED spike_tauri_webview2: built exe not found in any of {:?}",
                candidates
            );
            return;
        }
    };

    let dir = screenshot_dir();
    let verdict = run_spike_with_retries("tauri_webview2", &dir, || Command::new(&exe).spawn());
    verdict.print("tauri_webview2");
}

// ---------------------------------------------------------------------
// Target 4: an already-running Tauri app, attached rather than spawned.
// spike_tauri_webview2 above never actually exercised a real Tauri window
// because launching this repo's own binary hands off to the dev's running
// instance via tauri-plugin-single-instance and exits with no window of
// its own - an artifact of using this project as its own guinea pig, not
// a statement about the embedding mechanism. This test sidesteps that by
// reaching for a Tauri app the dev already has open.
//
// Everything here is the dev's live application, not a process this test
// owns, so the rules are different from every target above: nothing may
// ever taskkill it, and restoring its window ranks above producing a
// verdict at all - see `AttachTeardown` below.
// ---------------------------------------------------------------------

/// Returns the PID of the first running process matching `exe_name_lower`
/// (e.g. "pomodoro-overlay.exe"), or None if it is not running. Unlike
/// `any_process_named`, callers here need the PID itself to hand to
/// `find_guest_window`, not just a yes/no.
fn find_pid_by_name(exe_name_lower: &str) -> Option<u32> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() {
            return None;
        }
        let mut entry: Processentry32W = std::mem::zeroed();
        entry.dw_size = std::mem::size_of::<Processentry32W>() as u32;
        let mut result = None;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let name = String::from_utf16_lossy(
                    &entry.sz_exe_file[..entry
                        .sz_exe_file
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.sz_exe_file.len())],
                );
                if name.to_lowercase() == exe_name_lower {
                    result = Some(entry.th32_process_id);
                    break;
                }
                entry.dw_size = std::mem::size_of::<Processentry32W>() as u32;
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        result
    }
}

/// Picks the attach target: `$EMBED_SPIKE_ATTACH` if set, else the first
/// running match from the dev's usual Tauri apps, in priority order.
fn resolve_attach_target() -> Option<(String, u32)> {
    if let Ok(name) = std::env::var("EMBED_SPIKE_ATTACH") {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            let base = trimmed.trim_end_matches(".exe").to_string();
            let exe_lower = format!("{}.exe", base.to_lowercase());
            return find_pid_by_name(&exe_lower).map(|pid| (base, pid));
        }
    }
    for name in [
        "pomodoro-overlay",
        "windows-taskbar-widgets",
        "claude-conductor",
    ] {
        let exe_lower = format!("{name}.exe");
        if let Some(pid) = find_pid_by_name(&exe_lower) {
            return Some((name.to_string(), pid));
        }
    }
    None
}

/// What the `AttachTeardown` guard found when it tried to put the guest
/// window back exactly as it found it. `attempted` stays false only if the
/// guard never ran at all (impossible short of `mem::forget`), so a
/// default value reads as a failed restore rather than a silent pass.
#[derive(Clone, Copy, Default)]
struct RestoreOutcome {
    attempted: bool,
    parent_null: bool,
    // Whether IsWindowVisible after restore matches the show-state the
    // guard captured before the spike touched anything - not a bare
    // "is visible", since a window that started hidden to the tray must
    // end hidden, not end up shown.
    visible_matches_original: bool,
    rect_matches: bool,
}

impl RestoreOutcome {
    fn ok(&self) -> bool {
        self.attempted && self.parent_null && self.visible_matches_original && self.rect_matches
    }
}

/// Teardown for the attach case. Unlike `Teardown`, this struct has no PID
/// field and no taskkill call anywhere in it - there is structurally
/// nothing here that can terminate the dev's own live application, even on
/// a panicking unwind, because the guard never took a process handle to
/// begin with. Its only job is putting the guest window back and proving
/// it, which is why the restore runs unconditionally in `Drop` instead of
/// at the end of `run_attach_spike` - a panic or an early return still
/// reaches this.
struct AttachTeardown {
    host: HWND,
    class_name: Vec<u16>,
    h_instance: HINSTANCE,
    guest: HWND,
    orig_style: isize,
    orig_exstyle: isize,
    orig_parent: HWND,
    orig_rect: RECT,
    // Show-state as found, before this guard's SetParent/SetWindowPos ever
    // ran. Restoring style/parent/rect is not enough to restore this - see
    // the comment on the ShowWindow call below.
    orig_visible: bool,
    app_name: String,
    outcome: Rc<Cell<RestoreOutcome>>,
}

impl Drop for AttachTeardown {
    fn drop(&mut self) {
        let still_a_window = unsafe { IsWindow(self.guest) } != 0;
        let outcome = if still_a_window {
            unsafe {
                SetWindowLongPtrW(self.guest, GWL_STYLE, self.orig_style);
                SetWindowLongPtrW(self.guest, GWL_EXSTYLE, self.orig_exstyle);
                SetParent(self.guest, self.orig_parent);
                SetWindowPos(
                    self.guest,
                    ptr::null_mut(),
                    self.orig_rect.left,
                    self.orig_rect.top,
                    self.orig_rect.right - self.orig_rect.left,
                    self.orig_rect.bottom - self.orig_rect.top,
                    SWP_FRAMECHANGED,
                );
                // SetWindowLongPtrW on GWL_STYLE does not reliably toggle
                // WS_VISIBLE by itself (MSDN: use ShowWindow/EnableWindow
                // for that bit, not SetWindowLongPtr) - an explicit
                // ShowWindow call is the only way to guarantee a window
                // that started hidden to the tray ends hidden again.
                ShowWindow(
                    self.guest,
                    if self.orig_visible { SW_SHOW } else { SW_HIDE },
                );
            }
            pump_for(Duration::from_millis(500));
            let mut o = RestoreOutcome {
                attempted: true,
                ..Default::default()
            };
            unsafe {
                o.parent_null = GetParent(self.guest).is_null();
                o.visible_matches_original =
                    (IsWindowVisible(self.guest) != 0) == self.orig_visible;
                let mut rect = RECT::default();
                GetWindowRect(self.guest, &mut rect);
                o.rect_matches = rect.left == self.orig_rect.left
                    && rect.top == self.orig_rect.top
                    && rect.right == self.orig_rect.right
                    && rect.bottom == self.orig_rect.bottom;
            }
            if !o.ok() {
                println!(
                    "!!! RESTORE FAILED - {} window may be invisible, kill and relaunch it !!!",
                    self.app_name
                );
            }
            o
        } else {
            // The handle is already gone (e.g. the dev closed the app
            // mid-test). Hammering a dead HWND with SetParent/SetWindowPos
            // would only manufacture a false "restore failed" for a state
            // this guard did not cause - the INVALID verdict above already
            // explains why.
            println!(
                "[{}] guest window no longer exists at teardown time, nothing to restore",
                self.app_name
            );
            RestoreOutcome {
                attempted: true,
                parent_null: true,
                visible_matches_original: true,
                rect_matches: true,
            }
        };
        self.outcome.set(outcome);

        unsafe {
            if !self.host.is_null() {
                DestroyWindow(self.host);
            }
            UnregisterClassW(self.class_name.as_ptr(), self.h_instance);
        }
    }
}

/// Mirrors `run_spike`'s embed/measure steps for a window this test did not
/// launch. Returns the verdict plus whether the restore actually verified,
/// which the caller asserts on separately, since restoring the dev's
/// window outranks producing an embedding verdict at all.
fn run_attach_spike(app_name: &str, screenshot_dir: &Path, guest: HWND) -> (Verdict, bool) {
    let mut verdict = Verdict::default();

    if unsafe { IsWindow(guest) } == 0 {
        verdict.invalid_reason = Some(format!(
            "{app_name} window vanished before the spike could start"
        ));
        return (verdict, true); // nothing was ever touched, so nothing to restore
    }

    // Highest priority in this test, done before anything else touches the
    // guest: snapshot exactly what a restore needs, so the Drop guard
    // below can reach it on every exit path, including a panic.
    let orig_style = unsafe { GetWindowLongPtrW(guest, GWL_STYLE) };
    let orig_exstyle = unsafe { GetWindowLongPtrW(guest, GWL_EXSTYLE) };
    let orig_parent = unsafe { GetParent(guest) };
    let orig_visible = unsafe { IsWindowVisible(guest) } != 0;
    let mut orig_rect = RECT::default();
    unsafe {
        GetWindowRect(guest, &mut orig_rect);
    }
    // A hidden guest (e.g. an app minimised to the tray) is still a
    // legitimate embed target - the finder's `require_visible: false` on
    // this path is what let it through - but nothing would render until
    // it is explicitly shown below.
    verdict.guest_was_hidden = !orig_visible;

    unsafe {
        verdict.dpi_hosting_behavior_set =
            SetThreadDpiHostingBehavior(DPI_HOSTING_BEHAVIOR_MIXED) != -1;
    }
    if !verdict.dpi_hosting_behavior_set {
        println!(
            "[{app_name}] WARNING: SetThreadDpiHostingBehavior(MIXED) did not report success; \
             continuing anyway, but this is the first place to look if SetParent fails."
        );
    }

    let class_name = wide("EmbedSpikeAttachHostWindowClass");
    let h_instance = unsafe { GetModuleHandleW(ptr::null()) };
    let brush = unsafe { CreateSolidBrush(0x00FF00FF) };
    let wc = WNDCLASSEXW {
        cb_size: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfn_wnd_proc: wndproc,
        cb_cls_extra: 0,
        cb_wnd_extra: 0,
        h_instance,
        h_icon: ptr::null_mut(),
        h_cursor: unsafe { LoadCursorW(ptr::null_mut(), 32512usize as *const u16) }, // IDC_ARROW
        hbr_background: brush,
        lpsz_menu_name: ptr::null(),
        lpsz_class_name: class_name.as_ptr(),
        h_icon_sm: ptr::null_mut(),
    };
    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        println!(
            "[{app_name}] RegisterClassExW failed, GetLastError={}",
            unsafe { GetLastError() }
        );
        return (verdict, true); // guest never touched
    }

    let title = wide("Embed Spike Attach Host");
    let host = unsafe {
        CreateWindowExW(
            WS_EX_LEFT,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            50,
            50,
            1200,
            820,
            ptr::null_mut(),
            ptr::null_mut(),
            h_instance,
            ptr::null_mut(),
        )
    };
    if host.is_null() {
        println!(
            "[{app_name}] CreateWindowExW(host) failed, GetLastError={}",
            unsafe { GetLastError() }
        );
        unsafe {
            UnregisterClassW(class_name.as_ptr(), h_instance);
        }
        return (verdict, true); // guest never touched
    }
    unsafe {
        ShowWindow(host, SW_SHOW);
    }
    pump_for(Duration::from_millis(200));
    verdict.host_dpi_context = unsafe { GetWindowDpiAwarenessContext(host) } as isize;
    verdict.guest_dpi_context = unsafe { GetWindowDpiAwarenessContext(guest) } as isize;

    // From this point on, `guard` is the only thing allowed to touch the
    // guest's parent/style/rect. It is constructed before the first
    // mutation below, so any early return or panic on the remaining lines
    // still restores the dev's window instead of leaving a hidden
    // WS_CHILD orphan behind.
    let outcome_cell = Rc::new(Cell::new(RestoreOutcome::default()));
    let guard = AttachTeardown {
        host,
        class_name: class_name.clone(),
        h_instance,
        guest,
        orig_style,
        orig_exstyle,
        orig_parent,
        orig_rect,
        orig_visible,
        app_name: app_name.to_string(),
        outcome: outcome_cell.clone(),
    };

    if !still_alive_or_invalidate(&mut verdict, host, guest, "just before SetParent") {
        drop(guard);
        return (verdict, outcome_cell.get().ok());
    }

    let new_style = ((orig_style as u32) & !WS_OVERLAPPEDWINDOW & !WS_POPUP) | WS_CHILD;
    unsafe {
        SetWindowLongPtrW(guest, GWL_STYLE, new_style as isize);
    }
    let set_parent_result = unsafe { SetParent(guest, host) };
    verdict.setparent_ok = !set_parent_result.is_null();
    if !verdict.setparent_ok {
        verdict.setparent_last_error = unsafe { GetLastError() };
    }

    let margin = 40;
    let mut host_client = RECT::default();
    unsafe {
        GetClientRect(host, &mut host_client);
    }
    let inset_x = margin;
    let inset_y = margin;
    let inset_w = (host_client.right - host_client.left - 2 * margin).max(1);
    let inset_h = (host_client.bottom - host_client.top - 2 * margin).max(1);
    unsafe {
        SetWindowPos(
            guest,
            ptr::null_mut(),
            inset_x,
            inset_y,
            inset_w,
            inset_h,
            SWP_FRAMECHANGED | SWP_SHOWWINDOW | SWP_NOZORDER,
        );
        // The window this path selected is allowed to have started hidden
        // (tray-minimised) - SWP_SHOWWINDOW alone does not reliably flip
        // WS_VISIBLE (see the matching comment on AttachTeardown's
        // restore), so a hidden guest needs its own explicit ShowWindow or
        // there is nothing to capture.
        if verdict.guest_was_hidden {
            ShowWindow(guest, SW_SHOW);
        }
    }
    // Short on purpose: this is someone's live app, not a process this
    // spike owns, so it should exist as an embedded child only as long as
    // it takes to repaint once and capture a frame.
    pump_for(Duration::from_secs(2));

    if !still_alive_or_invalidate(&mut verdict, host, guest, "post-embed settle") {
        drop(guard);
        return (verdict, outcome_cell.get().ok());
    }

    verdict.parent_matches = unsafe { GetParent(guest) } == host;
    let is_window_after_embed = unsafe { IsWindow(guest) } != 0;

    let mut guest_pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(guest, &mut guest_pid);
    }
    verdict.guest_alive = unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, guest_pid);
        if h.is_null() {
            false
        } else {
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE
        }
    } && is_window_after_embed;

    let mut host_client_screen_tl = POINT {
        x: host_client.left,
        y: host_client.top,
    };
    let mut host_client_screen_br = POINT {
        x: host_client.right,
        y: host_client.bottom,
    };
    unsafe {
        ClientToScreen(host, &mut host_client_screen_tl);
        ClientToScreen(host, &mut host_client_screen_br);
    }
    let mut guest_rect_after = RECT::default();
    unsafe {
        GetWindowRect(guest, &mut guest_rect_after);
    }
    verdict.rect_inside_host = guest_rect_after.left >= host_client_screen_tl.x
        && guest_rect_after.top >= host_client_screen_tl.y
        && guest_rect_after.right <= host_client_screen_br.x
        && guest_rect_after.bottom <= host_client_screen_br.y
        && screen_dimensions_ok(&guest_rect_after);

    if !still_alive_or_invalidate(&mut verdict, host, guest, "just before pixel capture") {
        drop(guard);
        return (verdict, outcome_cell.get().ok());
    }

    let mut host_rect_screen = RECT::default();
    unsafe {
        GetWindowRect(host, &mut host_rect_screen);
    }
    if let Some((method, width, height, pixels)) = capture_host(host, host_rect_screen) {
        let sub = RECT {
            left: (guest_rect_after.left - host_rect_screen.left).max(0),
            top: (guest_rect_after.top - host_rect_screen.top).max(0),
            right: (guest_rect_after.right - host_rect_screen.left).min(width),
            bottom: (guest_rect_after.bottom - host_rect_screen.top).min(height),
        };
        let (distinct, dominant, total) = analyze_region(&pixels, width, sub);
        verdict.capture_method = method.clone();
        verdict.distinct_colours = distinct;
        verdict.dominant_colour_pct = if total > 0 {
            (dominant as f64 / total as f64) * 100.0
        } else {
            100.0
        };

        let mut final_pixels = pixels;
        if method.starts_with("PrintWindow") && distinct <= 1 {
            if let Some(retry_pixels) = capture_frame(host, host_rect_screen, width, height, false)
            {
                let (d2, dom2, tot2) = analyze_region(&retry_pixels, width, sub);
                verdict.capture_method = "BitBlt(screen)".to_string();
                verdict.distinct_colours = d2;
                verdict.dominant_colour_pct = if tot2 > 0 {
                    (dom2 as f64 / tot2 as f64) * 100.0
                } else {
                    100.0
                };
                final_pixels = retry_pixels;
            }
        }

        let bmp_path = screenshot_dir.join(format!("embed-spike-{app_name}.bmp"));
        if write_bmp(&bmp_path, width, height, &final_pixels).is_ok() {
            println!("[{app_name}] bitmap written to {}", bmp_path.display());
            verdict.bitmap = Some(bmp_path);
        }

        verdict.pixel_content_ok =
            verdict.distinct_colours > 16 && verdict.dominant_colour_pct < 95.0;
    } else {
        verdict.capture_method = "capture failed (both PrintWindow and BitBlt)".to_string();
    }

    // Dropping the guard here, rather than waiting for scope exit, makes
    // the restore happen at a known point right after the capture instead
    // of racing whatever the test does next - the Drop impl still covers
    // every early return above for the panic/early-exit paths.
    drop(guard);
    let outcome = outcome_cell.get();
    verdict.release_restored_ok = outcome.ok();
    (verdict, outcome.ok())
}

#[test]
#[ignore]
fn spike_running_tauri_app() {
    let (app_name, root_pid) = match resolve_attach_target() {
        Some(t) => t,
        None => {
            println!(
                "SKIPPED spike_running_tauri_app: none of pomodoro-overlay, \
                 windows-taskbar-widgets, claude-conductor (or $EMBED_SPIKE_ATTACH) is currently \
                 running"
            );
            return;
        }
    };

    // require_visible: false - an app hidden to the tray still owns its
    // real window, and attaching to that hidden window is a legitimate
    // thing to want (see find_guest_window's doc comment).
    let guest = match find_guest_window(root_pid, Duration::from_secs(5), false) {
        Some((h, _was_hidden)) => h,
        None => {
            println!(
                "SKIPPED spike_running_tauri_app: {app_name} (pid {root_pid}) is running but no \
                 unowned, non-zero-area main window was found for it or its descendants"
            );
            return;
        }
    };

    let dir = screenshot_dir();
    let (verdict, restore_verified) = run_attach_spike(&app_name, &dir, guest);
    verdict.print(&app_name);
    println!("restore_verified:        {restore_verified}");
    assert!(
        restore_verified,
        "!!! RESTORE FAILED - {app_name} window may be invisible, kill and relaunch it !!!"
    );
}
