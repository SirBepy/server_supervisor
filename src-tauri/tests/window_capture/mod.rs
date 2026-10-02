//! Shared by `window_spike.rs`: frame capture/comparison, the test HTML
//! page, locating msedge, and the end-to-end per-host-kind run that spawns
//! Edge, embeds it, captures frames, and posts keyboard/mouse input through
//! the production `supervisor::window::input` module exactly as the HTTP
//! API drives it. Split out of the former `tests/media_spike.rs` (todo 0060).
#![allow(dead_code)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::spike_common::kill_tree;
use crate::window_host::{create_host, embed, find_window, pump_for, HostKind, HOST_H, HOST_W};

struct Frame {
    w: i32,
    h: i32,
    bgra: Vec<u8>,
}

impl Frame {
    fn distinct(&self) -> usize {
        self.bgra.chunks_exact(4).map(|p| u32::from_le_bytes([p[0], p[1], p[2], 0])).collect::<HashSet<_>>().len()
    }
    /// Share of pixels with the same channel pattern as (r, g, b), tolerant
    /// of the page being dimmed: a full channel must be bright relative to
    /// the pixel's max, an empty one dark.
    fn fraction(&self, r: u8, g: u8, b: u8) -> f32 {
        let want = [b, g, r];
        let n = self
            .bgra
            .chunks_exact(4)
            .filter(|p| {
                let max = p[0].max(p[1]).max(p[2]) as f32;
                max > 40.0
                    && (0..3).all(|i| {
                        let v = p[i] as f32 / max;
                        if want[i] == 255 { v > 0.8 } else { v < 0.3 }
                    })
            })
            .count();
        n as f32 / (self.w * self.h).max(1) as f32
    }
    fn same_fraction(&self, other: &Frame) -> f32 {
        if self.bgra.len() != other.bgra.len() {
            return 0.0;
        }
        let n = self
            .bgra
            .chunks_exact(4)
            .zip(other.bgra.chunks_exact(4))
            .filter(|(a, b)| a[..3].iter().zip(&b[..3]).all(|(x, y)| x.abs_diff(*y) < 8))
            .count();
        n as f32 / (self.w * self.h).max(1) as f32
    }
    fn save(&self, name: &str) {
        let Some(dir) = std::env::var_os("SPIKE_OUT").map(PathBuf::from) else { return };
        let _ = std::fs::create_dir_all(&dir);
        let file = std::fs::File::create(dir.join(format!("{name}.png"))).expect("png file");
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), self.w as u32, self.h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let rgba: Vec<u8> = self.bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect();
        enc.write_header().unwrap().write_image_data(&rgba).unwrap();
    }
}

/// `PrintWindow(PW_RENDERFULLCONTENT)` when `hwnd` is Some, else a BitBlt
/// of the real screen at `screen_rect` (what the dev would actually see).
fn grab(hwnd: Option<HWND>, screen_rect: RECT) -> Frame {
    unsafe {
        let (w, h) = match hwnd {
            Some(hw) => {
                let mut r = RECT::default();
                let _ = GetWindowRect(hw, &mut r);
                (r.right - r.left, r.bottom - r.top)
            }
            None => (screen_rect.right - screen_rect.left, screen_rect.bottom - screen_rect.top),
        };
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).expect("dib");
        let old = SelectObject(mem, bmp.into());
        match hwnd {
            Some(hw) => {
                let _ = PrintWindow(hw, mem, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT));
            }
            None => {
                let _ = BitBlt(mem, 0, 0, w, h, Some(screen), screen_rect.left, screen_rect.top, SRCCOPY);
            }
        }
        let bgra = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        Frame { w, h, bgra }
    }
}

fn post(hwnd: HWND, msg: u32, w: usize, l: LPARAM) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), msg, WPARAM(w), l);
    }
}

pub(crate) fn find_msedge() -> Option<PathBuf> {
    [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

pub(crate) const PAGE: &str = r#"<!doctype html><html><body style="margin:0;font-family:sans-serif;background:#fff">
<canvas id=c width=900 height=260 style="display:block"></canvas>
<input id=t autofocus style="font-size:40px;width:80%;margin:10px">
<script>
const c=document.getElementById('c').getContext('2d');let f=0;
function tick(){f++;c.fillStyle=`hsl(${f*7%360},90%,50%)`;c.fillRect(0,0,900,260);
c.fillStyle='#000';c.font='60px sans-serif';c.fillText('frame '+f,20,150);requestAnimationFrame(tick)}tick();
document.getElementById('t').addEventListener('input',()=>{document.body.style.background='#00ffff'});
document.addEventListener('mousedown',e=>{if(e.target.id!=='t')document.body.style.background='#ff00ff'});
</script></body></html>"#;

pub(crate) fn run_host_case(kind: HostKind, msedge: &PathBuf, html_url: &str) {
    println!("===== host {kind:?} =====");
    let profile = std::env::temp_dir().join(format!("media_spike_edge_{}_{kind:?}", std::process::id()));
    let child = Command::new(msedge)
        .arg(format!("--app={html_url}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        // --guest: a fresh profile otherwise auto-signs into the Windows
        // account and opens a sync dialog that steals focus and dims the page.
        .args(["--guest", "--no-first-run", "--no-default-browser-check", "--window-size=900,600"])
        .spawn()
        .expect("spawn edge");
    let pid = child.id();
    let host = create_host(kind);
    let Some(guest) = find_window(pid, Duration::from_secs(15)) else {
        println!("no guest window found");
        kill_tree(pid);
        unsafe {
            let _ = DestroyWindow(host);
        }
        return;
    };
    let embedded = embed(guest, host);
    println!("SetParent ok: {embedded}");
    if matches!(kind, HostKind::Visible) {
        // place.rs hands SetWindowPos SCREEN coords for an embedded child;
        // this shows where a child actually lands when given them.
        unsafe {
            let mut hr = RECT::default();
            let _ = GetWindowRect(host, &mut hr);
            let _ = SetWindowPos(guest, None, hr.left, hr.top, HOST_W, HOST_H, SWP_NOZORDER | SWP_NOACTIVATE);
            let mut gr = RECT::default();
            let _ = GetWindowRect(guest, &mut gr);
            println!(
                "child given screen coords ({},{}) landed at screen ({},{}); host at ({},{})",
                hr.left, hr.top, gr.left, gr.top, hr.left, hr.top
            );
            let _ = SetWindowPos(guest, None, 0, 0, HOST_W, HOST_H, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
    pump_for(Duration::from_millis(2500));

    let a = grab(Some(guest), RECT::default());
    pump_for(Duration::from_millis(800));
    let b = grab(Some(guest), RECT::default());
    println!(
        "capture {}x{} distinct={} live(frames differ)={}",
        a.w,
        a.h,
        a.distinct(),
        a.same_fraction(&b) < 0.98
    );
    a.save(&format!("{kind:?}_a"));

    let mut hr = RECT::default();
    unsafe {
        let _ = GetWindowRect(host, &mut hr);
    }
    if hr.left > -10_000 {
        let screen = grab(None, hr);
        println!("screen-vs-capture same-pixel fraction (1.0 = fully visible to dev): {:.3}", screen.same_fraction(&b));
        screen.save(&format!("{kind:?}_screen"));
    }

    // Through the production module, exactly as the HTTP API drives it.
    use server_supervisor_lib::supervisor::window::input::{send, InputAction};
    let g = guest.0 as isize;
    // Keyboard first: the autofocused input should take posted WM_CHAR.
    let typed = send(g, &InputAction::Text { text: "hi".into() });
    pump_for(Duration::from_millis(700));
    let k = grab(Some(guest), RECT::default());
    println!("after text {typed:?}: cyan fraction {:.3}", k.fraction(0, 255, 255));
    k.save(&format!("{kind:?}_keys"));
    if k.fraction(0, 255, 255) < 0.1 {
        // autofocus never fires in a window that was never active, so do
        // what an agent would: click the field, then type.
        let _ = send(g, &InputAction::Click { x: 300, y: 325, button: Default::default(), double: false });
        pump_for(Duration::from_millis(300));
        let _ = send(g, &InputAction::Text { text: "hi".into() });
        pump_for(Duration::from_millis(700));
        let k2 = grab(Some(guest), RECT::default());
        println!("after click-into-field + text: cyan fraction {:.3}", k2.fraction(0, 255, 255));
        k2.save(&format!("{kind:?}_keys_clicked"));
        if k2.fraction(0, 255, 255) < 0.1 {
            // Chromium drops keys for a widget it believes is inactive.
            post(guest, WM_ACTIVATE, 1, LPARAM(0));
            post(guest, WM_SETFOCUS, 0, LPARAM(0));
            pump_for(Duration::from_millis(200));
            let _ = send(g, &InputAction::Click { x: 300, y: 325, button: Default::default(), double: false });
            pump_for(Duration::from_millis(300));
            let _ = send(g, &InputAction::Text { text: "hi".into() });
            pump_for(Duration::from_millis(700));
            let k3 = grab(Some(guest), RECT::default());
            println!("after WM_ACTIVATE+WM_SETFOCUS, click, text: cyan fraction {:.3}", k3.fraction(0, 255, 255));
            k3.save(&format!("{kind:?}_keys_activated"));
        }
    }

    // Mouse: press on the body below the input.
    let clicked = send(g, &InputAction::Click { x: 450, y: 520, button: Default::default(), double: false });
    println!("click sent: {clicked:?}");
    pump_for(Duration::from_millis(700));
    let m = grab(Some(guest), RECT::default());
    println!("after posted click: magenta fraction {:.3}", m.fraction(255, 0, 255));
    m.save(&format!("{kind:?}_click"));

    kill_tree(pid);
    unsafe {
        let _ = DestroyWindow(host);
    }
    let _ = std::fs::remove_dir_all(&profile);
}
