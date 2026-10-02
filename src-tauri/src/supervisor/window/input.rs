//! Drives a window with posted messages instead of `SendInput`. Posted input
//! goes straight into the target's message queue: the dev's real cursor
//! never moves, focus never changes, and it reaches windows that are
//! off-screen in the headless host. The tradeoff is that modifier state
//! (`GetKeyState`) is not faked, so Ctrl/Shift chords are not supported.

use serde::Deserialize;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC, VIRTUAL_KEY};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR,
    VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ChildWindowFromPointEx, GetGUIThreadInfo, GetWindowThreadProcessId, IsWindow, PostMessageW,
    CWP_SKIPDISABLED, CWP_SKIPINVISIBLE, CWP_SKIPTRANSPARENT, GUITHREADINFO,
    WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP,
};

const MK_LBUTTON: usize = 0x0001;
const MK_RBUTTON: usize = 0x0002;
const MK_MBUTTON: usize = 0x0010;
/// `ChildWindowFromPointEx` nesting depth cap; real UIs are a handful deep.
const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

/// One input action, in the docked window's own client pixels (the same
/// space as a screenshot from `capture`, so an agent can click what it
/// sees).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum InputAction {
    Click {
        x: i32,
        y: i32,
        #[serde(default)]
        button: MouseButton,
        #[serde(default)]
        double: bool,
    },
    Move { x: i32, y: i32 },
    /// `delta` in wheel notches; positive scrolls up.
    Scroll { x: i32, y: i32, delta: i32 },
    /// Literal text, delivered as `WM_CHAR` per UTF-16 unit.
    Text { text: String },
    /// A named key: Enter, Tab, Escape, Backspace, Delete, Space, ArrowUp,
    /// ArrowDown, ArrowLeft, ArrowRight, Home, End, PageUp, PageDown, F1-F12.
    Key { key: String },
}

fn makelparam(x: i32, y: i32) -> LPARAM {
    LPARAM((((y as u16 as u32) << 16) | (x as u16 as u32)) as isize)
}

fn post(hwnd: HWND, msg: u32, w: usize, l: LPARAM) -> Result<(), String> {
    unsafe { PostMessageW(Some(hwnd), msg, WPARAM(w), l) }.map_err(|e| format!("PostMessageW: {e}"))
}

/// The deepest visible, enabled, non-transparent descendant of `root` under
/// client point (`x`, `y`), with the point translated into that window's
/// own client space. Flutter, for one, takes mouse input only on its inner
/// FLUTTERVIEW child, never on the runner window.
fn target_at(root: HWND, x: i32, y: i32) -> (HWND, i32, i32) {
    let flags = CWP_SKIPINVISIBLE | CWP_SKIPDISABLED | CWP_SKIPTRANSPARENT;
    let mut screen = POINT { x, y };
    unsafe {
        let _ = ClientToScreen(root, &mut screen);
    }
    let mut cur = root;
    for _ in 0..MAX_DEPTH {
        let mut local = screen;
        unsafe {
            let _ = ScreenToClient(cur, &mut local);
        }
        let child = unsafe { ChildWindowFromPointEx(cur, local, flags) };
        if child.is_invalid() || child == cur {
            break;
        }
        cur = child;
    }
    let mut local = screen;
    unsafe {
        let _ = ScreenToClient(cur, &mut local);
    }
    (cur, local.x, local.y)
}

/// Where keyboard input should go: the window that holds keyboard focus on
/// the guest's UI thread, else the guest itself.
fn keyboard_target(root: HWND) -> HWND {
    unsafe {
        let thread = GetWindowThreadProcessId(root, None);
        let mut info = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        if GetGUIThreadInfo(thread, &mut info).is_ok() && !info.hwndFocus.is_invalid() {
            return info.hwndFocus;
        }
    }
    root
}

fn named_key(name: &str) -> Option<VIRTUAL_KEY> {
    let vk = match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => VK_RETURN,
        "tab" => VK_TAB,
        "escape" | "esc" => VK_ESCAPE,
        "backspace" => VK_BACK,
        "delete" | "del" => VK_DELETE,
        "space" => VK_SPACE,
        "arrowup" | "up" => VK_UP,
        "arrowdown" | "down" => VK_DOWN,
        "arrowleft" | "left" => VK_LEFT,
        "arrowright" | "right" => VK_RIGHT,
        "home" => VK_HOME,
        "end" => VK_END,
        "pageup" => VK_PRIOR,
        "pagedown" => VK_NEXT,
        f if f.len() >= 2 && f.starts_with('f') => {
            let n: u16 = f[1..].parse().ok()?;
            if !(1..=12).contains(&n) {
                return None;
            }
            VIRTUAL_KEY(VK_F1.0 + n - 1)
        }
        _ => return None,
    };
    Some(vk)
}

/// `WM_KEYDOWN`/`WM_KEYUP` lParam: repeat count 1, the scan code, and for
/// key-up the previous-state and transition bits (30, 31).
fn key_lparam(vk: VIRTUAL_KEY, up: bool) -> LPARAM {
    let scan = unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) } & 0xFF;
    let mut l = 1u32 | (scan << 16);
    if up {
        l |= 0xC000_0000;
    }
    LPARAM(l as i32 as isize)
}

pub fn send(hwnd: isize, action: &InputAction) -> Result<(), String> {
    let root = HWND(hwnd as *mut _);
    if !unsafe { IsWindow(Some(root)) }.as_bool() {
        return Err("window is gone".to_string());
    }
    match action {
        InputAction::Click { x, y, button, double } => {
            let (target, cx, cy) = target_at(root, *x, *y);
            let at = makelparam(cx, cy);
            let (down, up, mk) = match button {
                MouseButton::Left => (WM_LBUTTONDOWN, WM_LBUTTONUP, MK_LBUTTON),
                MouseButton::Right => (WM_RBUTTONDOWN, WM_RBUTTONUP, MK_RBUTTON),
                MouseButton::Middle => (WM_MBUTTONDOWN, WM_MBUTTONUP, MK_MBUTTON),
            };
            post(target, WM_MOUSEMOVE, 0, at)?;
            post(target, down, mk, at)?;
            post(target, up, 0, at)?;
            if *double {
                // A real double click is down/up/DBLCLK/up; only the left
                // button gets the DBLCLK message most frameworks look for.
                let dbl = if *button == MouseButton::Left { WM_LBUTTONDBLCLK } else { down };
                post(target, dbl, mk, at)?;
                post(target, up, 0, at)?;
            }
            Ok(())
        }
        InputAction::Move { x, y } => {
            let (target, cx, cy) = target_at(root, *x, *y);
            post(target, WM_MOUSEMOVE, 0, makelparam(cx, cy))
        }
        InputAction::Scroll { x, y, delta } => {
            let (target, _, _) = target_at(root, *x, *y);
            // WM_MOUSEWHEEL carries SCREEN coordinates, unlike the button
            // messages, and the delta in the high word of wParam.
            let mut screen = POINT { x: *x, y: *y };
            unsafe {
                let _ = ClientToScreen(root, &mut screen);
            }
            let wheel = ((*delta).clamp(-100, 100) * 120) as i16 as u16 as usize;
            post(target, WM_MOUSEWHEEL, wheel << 16, makelparam(screen.x, screen.y))
        }
        InputAction::Text { text } => {
            let target = keyboard_target(root);
            for unit in text.encode_utf16() {
                post(target, WM_CHAR, unit as usize, LPARAM(1))?;
            }
            Ok(())
        }
        InputAction::Key { key } => {
            let vk = named_key(key).ok_or_else(|| format!("unknown key: {key}"))?;
            let target = keyboard_target(root);
            post(target, WM_KEYDOWN, vk.0 as usize, key_lparam(vk, false))?;
            post(target, WM_KEYUP, vk.0 as usize, key_lparam(vk, true))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_resolve_case_insensitively() {
        assert_eq!(named_key("Enter"), Some(VK_RETURN));
        assert_eq!(named_key("ARROWLEFT"), Some(VK_LEFT));
        assert_eq!(named_key("f5"), Some(VIRTUAL_KEY(VK_F1.0 + 4)));
        assert_eq!(named_key("f13"), None);
        assert_eq!(named_key("ctrl+c"), None);
    }

    #[test]
    fn lparam_packs_low_x_high_y() {
        assert_eq!(makelparam(10, 20).0, (20 << 16) | 10);
    }

    #[test]
    fn actions_deserialize_from_agent_json() {
        let click: InputAction = serde_json::from_str(r#"{"type":"click","x":5,"y":6}"#).unwrap();
        assert_eq!(click, InputAction::Click { x: 5, y: 6, button: MouseButton::Left, double: false });
        let key: InputAction = serde_json::from_str(r#"{"type":"key","key":"Enter"}"#).unwrap();
        assert_eq!(key, InputAction::Key { key: "Enter".into() });
    }

    #[test]
    fn stale_handle_is_rejected() {
        assert!(send(0, &InputAction::Text { text: "x".into() }).is_err());
    }
}
