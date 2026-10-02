//! Spike for headless docking, per-process audio, window capture and posted
//! input. Answers, against real processes, the unknowns the production
//! `supervisor::audio` / `supervisor::window::capture` modules are built on:
//!
//! - Does a muted audio session still produce samples for process-loopback
//!   capture, and does its session peak meter still move?
//! - Which invisible host window keeps a docked Chromium guest rendering live
//!   (offscreen, DWM-cloaked, near-transparent layered)?
//! - Does a Chromium guest accept mouse/keyboard input posted as window
//!   messages, with no real cursor or focus change?
//!
//! Every test is `#[ignore]`: they spawn real GUI apps and play real sound.
//! Run with:
//!   cargo test --test media_spike -- --ignored --nocapture --test-threads=1
//! Set `SPIKE_OUT` to a directory to also write PNG evidence.

#![cfg(windows)]

use std::collections::{HashMap, HashSet};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, Ref, BOOL, HRESULT};
use windows::Win32::Foundation::{CloseHandle, COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{
    eRender, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, IAudioSessionControl2, IChannelAudioVolume, IAudioSessionManager2,
    IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    DEVICE_STATE_ACTIVE, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IAgileObject, IAgileObject_Impl, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::UI::HiDpi::{SetThreadDpiHostingBehavior, DPI_HOSTING_BEHAVIOR_MIXED};
use windows::Win32::UI::WindowsAndMessaging::*;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const VT_BLOB: u16 = 65;
const BUFFERFLAGS_SILENT: u32 = 2;
const HOST_W: i32 = 900;
const HOST_H: i32 = 600;

// ---------------------------------------------------------------------
// Process tree
// ---------------------------------------------------------------------

fn process_tree(root: u32) -> HashSet<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).expect("toolhelp snapshot");
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                children.entry(e.th32ParentProcessID).or_default().push(e.th32ProcessID);
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    let mut out = HashSet::from([root]);
    let mut stack = vec![root];
    while let Some(p) = stack.pop() {
        for &c in children.get(&p).into_iter().flatten() {
            if c != p && out.insert(c) {
                stack.push(c);
            }
        }
    }
    out
}

fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

// ---------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------

struct Session {
    pid: u32,
    volume: ISimpleAudioVolume,
    meter: IAudioMeterInformation,
}

fn render_sessions(enumerator: &IMMDeviceEnumerator) -> Vec<Session> {
    let mut out = Vec::new();
    unsafe {
        let Ok(devices) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else { return out };
        for i in 0..devices.GetCount().unwrap_or(0) {
            let Ok(dev) = devices.Item(i) else { continue };
            let Ok(mgr) = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
            let Ok(list) = mgr.GetSessionEnumerator() else { continue };
            for j in 0..list.GetCount().unwrap_or(0) {
                let Ok(ctl) = list.GetSession(j) else { continue };
                let (Ok(ctl2), Ok(volume), Ok(meter)) = (
                    ctl.cast::<IAudioSessionControl2>(),
                    ctl.cast::<ISimpleAudioVolume>(),
                    ctl.cast::<IAudioMeterInformation>(),
                ) else {
                    continue;
                };
                let pid = ctl2.GetProcessId().unwrap_or(0);
                out.push(Session { pid, volume, meter });
            }
        }
    }
    out
}

#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct ActivateDone(mpsc::Sender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivateDone_Impl {
    fn ActivateCompleted(&self, _op: Ref<IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}

impl IAgileObject_Impl for ActivateDone_Impl {}

/// `PROPVARIANT` holding a `VT_BLOB`, laid out by hand: x64 puts `vt` and
/// three reserved words in the first 8 bytes, then the BLOB's `cbSize` and
/// (8-aligned) `pBlobData`.
#[repr(C)]
struct BlobVariant {
    vt: u16,
    r1: u16,
    r2: u16,
    r3: u16,
    cb: u32,
    ptr: *const u8,
}

struct CaptureStats {
    frames: u64,
    nonsilent_packets: u64,
    peak: f32,
    rms: f32,
}

fn capture_process_tree(pid: u32, duration: Duration) -> windows::core::Result<CaptureStats> {
    unsafe {
        let params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };
        let pv = BlobVariant {
            vt: VT_BLOB,
            r1: 0,
            r2: 0,
            r3: 0,
            cb: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            ptr: &params as *const _ as *const u8,
        };
        let (tx, rx) = mpsc::channel();
        let handler: IActivateAudioInterfaceCompletionHandler = ActivateDone(tx).into();
        let op = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&pv as *const BlobVariant as *const PROPVARIANT),
            &handler,
        )?;
        rx.recv_timeout(Duration::from_secs(5)).expect("activation never completed");
        let mut hr = HRESULT(0);
        let mut unk = None;
        op.GetActivateResult(&mut hr, &mut unk)?;
        hr.ok()?;
        let client: IAudioClient = unk.expect("activated interface").cast()?;

        let fmt = WAVEFORMATEX {
            wFormatTag: 1, // WAVE_FORMAT_PCM
            nChannels: 2,
            nSamplesPerSec: 44_100,
            nAvgBytesPerSec: 44_100 * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            2_000_000,
            0,
            &fmt,
            None,
        )?;
        let ev = CreateEventW(None, false, false, None)?;
        client.SetEventHandle(ev)?;
        let cap: IAudioCaptureClient = client.GetService()?;
        client.Start()?;

        let mut stats = CaptureStats { frames: 0, nonsilent_packets: 0, peak: 0.0, rms: 0.0 };
        let mut sum_sq = 0f64;
        let mut samples = 0u64;
        let end = Instant::now() + duration;
        while Instant::now() < end {
            WaitForSingleObject(ev, 100);
            loop {
                let n = cap.GetNextPacketSize()?;
                if n == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                stats.frames += frames as u64;
                if flags & BUFFERFLAGS_SILENT == 0 && !data.is_null() {
                    stats.nonsilent_packets += 1;
                    let s = std::slice::from_raw_parts(data as *const i16, frames as usize * 2);
                    for &v in s {
                        let f = v as f32 / 32768.0;
                        stats.peak = stats.peak.max(f.abs());
                        sum_sq += (f as f64) * (f as f64);
                    }
                    samples += s.len() as u64;
                }
                cap.ReleaseBuffer(frames)?;
            }
        }
        let _ = client.Stop();
        let _ = CloseHandle(ev);
        if samples > 0 {
            stats.rms = (sum_sq / samples as f64).sqrt() as f32;
        }
        Ok(stats)
    }
}

#[test]
#[ignore]
fn spike_audio_mute_then_capture() {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().expect("CoInitializeEx");
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).expect("device enumerator");

        let wav = r"C:\Windows\Media\Alarm01.wav";
        let script = format!(
            "$p = New-Object System.Media.SoundPlayer '{wav}'; $p.PlayLooping(); Start-Sleep -Seconds 10"
        );
        let started = Instant::now();
        let child = Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("spawn powershell player");
        let pid = child.id();

        // Mute as fast as the session appears; this delay is the leak window
        // a default-muted production watcher would also have.
        let mut muted_at = None;
        let mut peak_before_mute = 0f32;
        while started.elapsed() < Duration::from_secs(8) && muted_at.is_none() {
            let tree = process_tree(pid);
            for s in render_sessions(&enumerator) {
                if tree.contains(&s.pid) {
                    peak_before_mute = s.meter.GetPeakValue().unwrap_or(-1.0);
                    s.volume.SetMute(true, std::ptr::null()).expect("SetMute");
                    muted_at = Some(started.elapsed());
                }
            }
            if muted_at.is_none() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        println!("session muted at {muted_at:?} after spawn, meter peak at that moment {peak_before_mute}");
        let Some(_) = muted_at else {
            kill_tree(pid);
            panic!("no audio session ever appeared for the player");
        };

        // Session meter while muted: is the level pre- or post-mute?
        let mut meter_max = 0f32;
        let meter_end = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < meter_end {
            for s in render_sessions(&enumerator) {
                if process_tree(pid).contains(&s.pid) {
                    let muted = s.volume.GetMute().map(|b| b.as_bool()).unwrap_or(false);
                    assert!(muted, "session should still be muted");
                    meter_max = meter_max.max(s.meter.GetPeakValue().unwrap_or(0.0));
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        println!("session meter max while muted: {meter_max}");

        match capture_process_tree(pid, Duration::from_millis(2000)) {
            Ok(st) => println!(
                "process loopback while muted: frames={} nonsilent_packets={} peak={:.4} rms={:.4}",
                st.frames, st.nonsilent_packets, st.peak, st.rms
            ),
            Err(e) => println!("process loopback FAILED: {e:?}"),
        }
        kill_tree(pid);

        // Does Windows remember the mute for the next process of the same
        // exe? Probed with a silent WAV so nothing is audible either way.
        let silent = write_silent_wav();
        let script = format!(
            "$p = New-Object System.Media.SoundPlayer '{}'; $p.PlayLooping(); Start-Sleep -Seconds 6",
            silent.display()
        );
        let child = Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("spawn silent player");
        let pid2 = child.id();
        let started = Instant::now();
        let mut initial = None;
        while started.elapsed() < Duration::from_secs(5) && initial.is_none() {
            let tree = process_tree(pid2);
            for s in render_sessions(&enumerator) {
                if tree.contains(&s.pid) {
                    initial = Some(s.volume.GetMute().map(|b| b.as_bool()).unwrap_or(false));
                    // Leave the exe unmuted for whatever Windows persists.
                    let _ = s.volume.SetMute(false, std::ptr::null());
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        println!("next powershell session started muted (mute persisted per exe): {initial:?}");
        kill_tree(pid2);
    }
}

fn spawn_player(wav: &str, secs: u32) -> u32 {
    let script = format!("$p = New-Object System.Media.SoundPlayer '{wav}'; $p.PlayLooping(); Start-Sleep -Seconds {secs}");
    Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("spawn player")
        .id()
}

fn wait_session(enumerator: &IMMDeviceEnumerator, pid: u32) -> Option<(IChannelAudioVolume, ISimpleAudioVolume, IAudioMeterInformation)> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(6) {
        let tree = process_tree(pid);
        for s in render_sessions(enumerator) {
            if tree.contains(&s.pid) {
                let ch = s.volume.cast::<IChannelAudioVolume>().expect("IChannelAudioVolume");
                return Some((ch, s.volume, s.meter));
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

/// Muting persists per exe (proven above), so a mute applied to a
/// supervised app would follow that exe outside the supervisor. Does
/// per-channel session volume silence the same way without persisting?
#[test]
#[ignore]
fn spike_channel_volume_silence_and_persistence() {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().expect("CoInitializeEx");
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).expect("device enumerator");

        let pid = spawn_player(r"C:\Windows\Media\Alarm01.wav", 10);
        let Some((ch, vol, meter)) = wait_session(&enumerator, pid) else {
            kill_tree(pid);
            panic!("no session");
        };
        let n = ch.GetChannelCount().unwrap_or(0);
        ch.SetAllVolumes(&vec![0.0; n as usize], std::ptr::null()).expect("SetAllVolumes");
        println!("channels={n} muted={:?}", vol.GetMute().map(|b| b.as_bool()));
        // Abort early if channel volume does not silence it.
        let probe = capture_process_tree(pid, Duration::from_millis(300)).map(|s| s.peak).unwrap_or(-1.0);
        if probe > 0.01 {
            kill_tree(pid);
            panic!("channel volume 0 did NOT silence loopback (peak {probe})");
        }
        let st = capture_process_tree(pid, Duration::from_millis(1500));
        let mut meter_max = 0f32;
        for _ in 0..30 {
            meter_max = meter_max.max(meter.GetPeakValue().unwrap_or(0.0));
            std::thread::sleep(Duration::from_millis(20));
        }
        println!(
            "channel-volume 0: loopback peak={:?}, session meter max={meter_max}",
            st.as_ref().map(|s| s.peak).map_err(|e| e.to_string())
        );
        kill_tree(pid);

        let silent = write_silent_wav();
        let pid2 = spawn_player(&silent.to_string_lossy(), 5);
        if let Some((ch2, vol2, _)) = wait_session(&enumerator, pid2) {
            let n2 = ch2.GetChannelCount().unwrap_or(0);
            let levels: Vec<f32> = (0..n2).map(|i| ch2.GetChannelVolume(i).unwrap_or(-1.0)).collect();
            println!(
                "next session: channel volumes {levels:?} (persisted if 0), muted={:?}",
                vol2.GetMute().map(|b| b.as_bool())
            );
            let _ = ch2.SetAllVolumes(&vec![1.0; n2 as usize], std::ptr::null());
        }
        kill_tree(pid2);
    }
}

fn write_silent_wav() -> PathBuf {
    let path = std::env::temp_dir().join("media_spike_silent.wav");
    let samples = 44_100u32; // 1s mono 16-bit
    let data_len = samples * 2;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&44_100u32.to_le_bytes());
    b.extend_from_slice(&88_200u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    b.resize(44 + data_len as usize, 0);
    std::fs::write(&path, b).unwrap();
    path
}

// ---------------------------------------------------------------------
// Windows: host, find, embed, capture, input
// ---------------------------------------------------------------------

unsafe extern "system" fn host_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(h, m, w, l) }
}

fn pump_for(d: Duration) {
    let end = Instant::now() + d;
    while Instant::now() < end {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Clone, Copy, Debug)]
enum HostKind {
    Visible,
    Offscreen,
    Cloaked,
    Layered,
}

fn create_host(kind: HostKind) -> HWND {
    unsafe {
        let class = windows::core::w!("MediaSpikeHost");
        let hinst = GetModuleHandleW(None).expect("module handle");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(host_proc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let mut ex = WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        if matches!(kind, HostKind::Layered) {
            ex |= WS_EX_LAYERED | WS_EX_TRANSPARENT;
        }
        let (x, y) = match kind {
            HostKind::Offscreen => (-20_000, -20_000),
            _ => (120, 120),
        };
        let host = CreateWindowExW(
            ex,
            class,
            windows::core::w!("media spike host"),
            WS_POPUP | WS_CLIPCHILDREN,
            x,
            y,
            HOST_W,
            HOST_H,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .expect("CreateWindowExW host");
        match kind {
            HostKind::Cloaked => {
                let on = BOOL(1);
                DwmSetWindowAttribute(host, DWMWA_CLOAK, &on as *const _ as _, 4).expect("cloak");
            }
            HostKind::Layered => {
                SetLayeredWindowAttributes(host, COLORREF(0), 1, LWA_ALPHA).expect("layered alpha");
            }
            _ => {}
        }
        let _ = ShowWindow(host, SW_SHOWNOACTIVATE);
        if !matches!(kind, HostKind::Visible) {
            let _ = SetWindowPos(host, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        host
    }
}

struct FindCtx {
    pids: HashSet<u32>,
    best: Option<(HWND, i32)>,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
    unsafe {
        let ctx = &mut *(lp.0 as *mut FindCtx);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if !ctx.pids.contains(&pid) || !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return BOOL(1);
        }
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w >= 32 && h >= 32 && ctx.best.is_none_or(|(_, a)| w * h > a) {
            ctx.best = Some((hwnd, w * h));
        }
        BOOL(1)
    }
}

fn find_window(root: u32, timeout: Duration) -> Option<HWND> {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        let mut ctx = FindCtx { pids: process_tree(root), best: None };
        unsafe {
            let _ = EnumWindows(Some(enum_cb), LPARAM(&mut ctx as *mut _ as isize));
        }
        if let Some((h, _)) = ctx.best {
            return Some(h);
        }
        pump_for(Duration::from_millis(100));
    }
    None
}

fn embed(guest: HWND, host: HWND) -> bool {
    unsafe {
        let _ = SetThreadDpiHostingBehavior(DPI_HOSTING_BEHAVIOR_MIXED);
        let style = GetWindowLongPtrW(guest, GWL_STYLE) as u32;
        let new_style = (style & !WS_OVERLAPPEDWINDOW.0 & !WS_POPUP.0) | WS_CHILD.0;
        SetWindowLongPtrW(guest, GWL_STYLE, new_style as isize);
        let ok = SetParent(guest, Some(host)).is_ok();
        let _ = SetWindowPos(guest, None, 0, 0, HOST_W, HOST_H, SWP_FRAMECHANGED | SWP_SHOWWINDOW | SWP_NOACTIVATE);
        ok
    }
}

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

fn find_msedge() -> Option<PathBuf> {
    [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

const PAGE: &str = r#"<!doctype html><html><body style="margin:0;font-family:sans-serif;background:#fff">
<canvas id=c width=900 height=260 style="display:block"></canvas>
<input id=t autofocus style="font-size:40px;width:80%;margin:10px">
<script>
const c=document.getElementById('c').getContext('2d');let f=0;
function tick(){f++;c.fillStyle=`hsl(${f*7%360},90%,50%)`;c.fillRect(0,0,900,260);
c.fillStyle='#000';c.font='60px sans-serif';c.fillText('frame '+f,20,150);requestAnimationFrame(tick)}tick();
document.getElementById('t').addEventListener('input',()=>{document.body.style.background='#00ffff'});
document.addEventListener('mousedown',e=>{if(e.target.id!=='t')document.body.style.background='#ff00ff'});
</script></body></html>"#;

fn run_host_case(kind: HostKind, msedge: &PathBuf, html_url: &str) {
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

#[test]
#[ignore]
fn spike_headless_hosts_capture_and_input() {
    let Some(msedge) = find_msedge() else {
        println!("SKIPPED: msedge.exe not found");
        return;
    };
    let html_dir = std::env::temp_dir().join("media_spike_html");
    let _ = std::fs::create_dir_all(&html_dir);
    let html_path = html_dir.join("page.html");
    std::fs::write(&html_path, PAGE).unwrap();
    let html_url = format!("file:///{}", html_path.to_string_lossy().replace('\\', "/"));
    let only = std::env::var("SPIKE_HOST").ok();
    for kind in [HostKind::Visible, HostKind::Offscreen, HostKind::Cloaked, HostKind::Layered] {
        if only.as_deref().is_some_and(|o| !format!("{kind:?}").eq_ignore_ascii_case(o)) {
            continue;
        }
        run_host_case(kind, &msedge, &html_url);
    }
}
