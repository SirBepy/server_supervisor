//! Core Audio session scan: finds every render session on every active
//! output device and mutes or unmutes the ones a supervised process tree
//! owns. A session the supervisor did not launch is only ever unmuted, to
//! undo a mute Windows carried over from a supervised run (see `restore`).

use super::restore::MutedExes;
use std::collections::{HashMap, HashSet};
use windows::core::Interface;
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{
    eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    ISimpleAudioVolume, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

/// A parent chain longer than this is a cycle from recycled pids, not a real
/// process tree.
const MAX_ANCESTRY: usize = 64;

pub(super) struct Scanner {
    enumerator: IMMDeviceEnumerator,
}

impl Scanner {
    /// Initialises COM (MTA) on the calling thread, so the scanner must stay
    /// on the thread that built it.
    pub(super) fn new() -> windows::core::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            let enumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            Ok(Self { enumerator })
        }
    }

    /// Every render session as (owning pid, volume control). The session
    /// manager is re-activated on every call on purpose: an enumerator taken
    /// from a long-lived manager does not reliably list sessions created
    /// after it, and a device plugged in later would be missed entirely.
    fn sessions(&self) -> Vec<(u32, ISimpleAudioVolume)> {
        let mut out = Vec::new();
        unsafe {
            let Ok(devices) = self.enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else {
                return out;
            };
            for i in 0..devices.GetCount().unwrap_or(0) {
                let Ok(device) = devices.Item(i) else { continue };
                let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
                let Ok(list) = manager.GetSessionEnumerator() else { continue };
                for j in 0..list.GetCount().unwrap_or(0) {
                    let Ok(control) = list.GetSession(j) else { continue };
                    let (Ok(control2), Ok(volume)) =
                        (control.cast::<IAudioSessionControl2>(), control.cast::<ISimpleAudioVolume>())
                    else {
                        continue;
                    };
                    // pid 0 is the shared system-sounds session.
                    match control2.GetProcessId() {
                        Ok(pid) if pid != 0 => out.push((pid, volume)),
                        _ => {}
                    }
                }
            }
        }
        out
    }

    /// Peak meters of every session owned by `root`'s process tree. The
    /// session meter reads the signal BEFORE the session's mute is applied
    /// (`tests/media_spike.rs` measured 0.36 on a muted session), which is
    /// what lets an agent hear an app the dev has muted.
    pub(super) fn meters_for(&self, root: u32) -> Vec<IAudioMeterInformation> {
        let sessions = self.sessions();
        if sessions.is_empty() {
            return Vec::new();
        }
        let parents = crate::supervisor::window::snapshot_parent_map();
        let only_root = HashMap::from([(root, true)]);
        sessions
            .into_iter()
            .filter(|(pid, _)| owning_policy(*pid, &parents, &only_root).is_some())
            .filter_map(|(_, volume)| volume.cast::<IAudioMeterInformation>().ok())
            .collect()
    }

    /// Brings every supervised session in line with `policy` (root pid ->
    /// play_sound). Only issues `SetMute` when the state actually differs,
    /// so the dev's own Volume Mixer is not spammed with change events.
    pub(super) fn apply(&self, policy: &HashMap<u32, bool>, muted_exes: &mut MutedExes) {
        let sessions = self.sessions();
        let live: HashSet<u32> = sessions.iter().map(|(pid, _)| *pid).collect();
        muted_exes.retain_seen(&live);
        if sessions.is_empty() {
            return;
        }
        let parents = if policy.is_empty() { HashMap::new() } else { crate::supervisor::window::snapshot_parent_map() };
        for (pid, volume) in sessions {
            let muted =unsafe { volume.GetMute() }.map(|b| b.as_bool()).ok();
            let want_muted = match owning_policy(pid, &parents, policy) {
                Some(play) => !play,
                // Not ours: only ever unmuted, and only to undo a mute
                // Windows carried over from a supervised run of the same exe.
                None if muted == Some(true) && muted_exes.should_restore(pid) => false,
                None => continue,
            };
            if muted == Some(want_muted) {
                continue;
            }
            match unsafe { volume.SetMute(want_muted, std::ptr::null()) } {
                Ok(()) if want_muted => muted_exes.note_muted(pid),
                Ok(()) => {}
                Err(e) => log::warn!("audio: SetMute({want_muted}) failed for pid {pid}: {e}"),
            }
        }
    }
}

/// Walks `pid`'s parent chain until it reaches a supervised root, returning
/// that root's play_sound flag. The audible process is usually a grandchild
/// of the root (`cmd /C` -> flutter -> app.exe), never the root itself.
pub(super) fn owning_policy(
    pid: u32,
    parents: &HashMap<u32, u32>,
    policy: &HashMap<u32, bool>,
) -> Option<bool> {
    let mut cur = pid;
    for _ in 0..MAX_ANCESTRY {
        if let Some(&play) = policy.get(&cur) {
            return Some(play);
        }
        match parents.get(&cur) {
            Some(&parent) if parent != 0 && parent != cur => cur = parent,
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grandchild_session_takes_its_roots_policy() {
        let parents = HashMap::from([(30, 20), (20, 10), (10, 1)]);
        let policy = HashMap::from([(10, false)]);
        assert_eq!(owning_policy(30, &parents, &policy), Some(false));
        assert_eq!(owning_policy(10, &parents, &policy), Some(false));
    }

    #[test]
    fn unrelated_process_is_left_alone() {
        let parents = HashMap::from([(30, 20), (20, 1)]);
        let policy = HashMap::from([(10, false)]);
        assert_eq!(owning_policy(30, &parents, &policy), None);
    }

    #[test]
    fn parent_cycle_terminates() {
        let parents = HashMap::from([(30, 20), (20, 30)]);
        let policy = HashMap::from([(10, true)]);
        assert_eq!(owning_policy(30, &parents, &policy), None);
    }
}
