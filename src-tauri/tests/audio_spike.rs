//! Spike for per-process audio: does a muted audio session still produce
//! samples for process-loopback capture, does its session peak meter still
//! move, and does per-channel volume silence without the mute's persistence?
//! Answers the unknowns the production `supervisor::audio` module is built
//! on. Split from `tests/media_spike.rs`'s Core Audio half (todo 0060); the
//! window-capture half now lives in `tests/window_spike.rs`.
//!
//! Every test is `#[ignore]`: they spawn real audio sessions and play real
//! sound. Run with:
//!   cargo test --test audio_spike -- --ignored --nocapture --test-threads=1

#![cfg(windows)]

mod audio_common;
mod spike_common;

use std::os::windows::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

use audio_common::{capture_process_tree, render_sessions, spawn_player, wait_session, write_silent_wav};
use spike_common::{kill_tree, process_tree, CREATE_NO_WINDOW};

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
