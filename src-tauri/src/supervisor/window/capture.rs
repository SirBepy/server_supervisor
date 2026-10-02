//! Screenshots a window through `PrintWindow(PW_RENDERFULLCONTENT)`, which
//! asks DWM for the window's own composed content instead of copying screen
//! pixels. That is what makes it work on a window that is covered, embedded,
//! or parked in the off-screen headless host where nothing is on screen to
//! copy. Without the flag, DirectComposition apps (Chromium, WebView2,
//! Flutter) capture as solid black.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsWindow, PW_RENDERFULLCONTENT};

/// Larger than any real monitor; guards the bitmap allocation against a
/// garbage rect from a window mid-destruction.
const MAX_DIM: i32 = 16_384;

pub struct Capture {
    pub width: u32,
    pub height: u32,
    /// Top-down RGBA, 4 bytes per pixel.
    pub rgba: Vec<u8>,
}

impl Capture {
    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, self.width, self.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| format!("png header: {e}"))?;
        writer.write_image_data(&self.rgba).map_err(|e| format!("png encode: {e}"))?;
        writer.finish().map_err(|e| format!("png finish: {e}"))?;
        Ok(out)
    }
}

pub fn capture(hwnd: isize) -> Result<Capture, String> {
    let hwnd = HWND(hwnd as *mut _);
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return Err("window is gone".to_string());
        }
        let mut r = RECT::default();
        GetWindowRect(hwnd, &mut r).map_err(|e| format!("GetWindowRect: {e}"))?;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w <= 0 || h <= 0 || w > MAX_DIM || h > MAX_DIM {
            return Err(format!("window has an unusable size {w}x{h}"));
        }
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                // Negative height = top-down rows, matching PNG order.
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let result = match CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bmp) => {
                let old = SelectObject(mem, bmp.into());
                let printed = PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool();
                let out = if printed && !bits.is_null() {
                    let bgra = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
                    let rgba = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect();
                    Ok(Capture { width: w as u32, height: h as u32, rgba })
                } else {
                    Err("PrintWindow failed".to_string())
                };
                SelectObject(mem, old);
                let _ = DeleteObject(bmp.into());
                out
            }
            Err(e) => Err(format!("CreateDIBSection: {e}")),
        };
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_handle_is_rejected() {
        assert!(capture(0).is_err());
    }

    #[test]
    fn png_round_trips_dimensions() {
        let c = Capture { width: 3, height: 2, rgba: vec![255; 3 * 2 * 4] };
        let png = c.to_png().unwrap();
        assert_eq!(&png[1..4], b"PNG");
        // IHDR width/height live at bytes 16..24.
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 3);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 2);
    }
}
