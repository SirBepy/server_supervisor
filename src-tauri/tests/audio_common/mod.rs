//! Shared by both tests in `audio_spike.rs`: enumerating render-device audio
//! sessions, activating a process-loopback capture client, and the silent
//! WAV used so mute/volume persistence probes make no sound either way.
//! Split out of the former `tests/media_spike.rs` (todo 0060).
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, Ref, HRESULT};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{
    eRender, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, IAudioSessionControl2, IChannelAudioVolume, IAudioSessionManager2,
    IMMDeviceEnumerator, ISimpleAudioVolume, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    DEVICE_STATE_ACTIVE, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{IAgileObject, IAgileObject_Impl, CLSCTX_ALL};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::spike_common::{process_tree, CREATE_NO_WINDOW};

const VT_BLOB: u16 = 65;
const BUFFERFLAGS_SILENT: u32 = 2;

pub(crate) struct Session {
    pub(crate) pid: u32,
    pub(crate) volume: ISimpleAudioVolume,
    pub(crate) meter: IAudioMeterInformation,
}

pub(crate) fn render_sessions(enumerator: &IMMDeviceEnumerator) -> Vec<Session> {
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

pub(crate) struct CaptureStats {
    pub(crate) frames: u64,
    pub(crate) nonsilent_packets: u64,
    pub(crate) peak: f32,
    pub(crate) rms: f32,
}

pub(crate) fn capture_process_tree(pid: u32, duration: Duration) -> windows::core::Result<CaptureStats> {
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

pub(crate) fn spawn_player(wav: &str, secs: u32) -> u32 {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    let script = format!("$p = New-Object System.Media.SoundPlayer '{wav}'; $p.PlayLooping(); Start-Sleep -Seconds {secs}");
    Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("spawn player")
        .id()
}

pub(crate) fn wait_session(enumerator: &IMMDeviceEnumerator, pid: u32) -> Option<(IChannelAudioVolume, ISimpleAudioVolume, IAudioMeterInformation)> {
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

pub(crate) fn write_silent_wav() -> PathBuf {
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
