//! Undoes the mute Windows remembers on our behalf. Windows persists a
//! session's mute per executable (`tests/media_spike.rs`: the next process
//! of the same exe started muted), so muting a supervised Edge would also
//! silence the dev's own Edge the next time it plays sound. This records
//! every exe the supervisor muted and unmutes the first session of that exe
//! it sees running OUTSIDE the supervisor.
//!
//! Kept on disk because the remembered mute outlives this process: a crash
//! or quit right after muting must still be undone by the next run.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use windows::core::PWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

const FILE: &str = "audio_muted_exes.json";

pub(super) struct MutedExes {
    path: PathBuf,
    exes: HashSet<String>,
    /// Unsupervised session pids already looked at once. Restoring only on
    /// first sight means a mute the dev applies afterwards, by hand in the
    /// Volume Mixer, is left alone.
    seen: HashSet<u32>,
}

impl MutedExes {
    pub(super) fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE);
        let exes = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, exes, seen: HashSet::new() }
    }

    fn save(&self) {
        if let Ok(json) = serde_json::to_string(&self.exes) {
            let _ = std::fs::write(&self.path, json);
        }
    }

    /// Call after the supervisor flips a session from unmuted to muted.
    pub(super) fn note_muted(&mut self, pid: u32) {
        if let Some(exe) = exe_path(pid) {
            if self.exes.insert(exe) {
                self.save();
            }
        }
    }

    /// Whether an unsupervised, currently muted session should be unmuted:
    /// true the first time a session of a recorded exe shows up. Forgets the
    /// exe once restored, since Windows now remembers it unmuted again.
    pub(super) fn should_restore(&mut self, pid: u32) -> bool {
        if self.exes.is_empty() || !self.seen.insert(pid) {
            return false;
        }
        let Some(exe) = exe_path(pid) else { return false };
        if self.exes.remove(&exe) {
            self.save();
            return true;
        }
        false
    }

    /// Drops pids whose sessions are gone, so a recycled pid counts as new.
    pub(super) fn retain_seen(&mut self, live: &HashSet<u32>) {
        self.seen.retain(|p| live.contains(p));
    }
}

/// Lower-cased full image path: Windows keys the remembered volume on it,
/// and paths on this machine compare case-insensitively.
fn exe_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(handle);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]).to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_a_recorded_exe_once_then_forgets_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = MutedExes::load(dir.path());
        let me = std::process::id();
        m.note_muted(me);
        assert!(MutedExes::load(dir.path()).exes.len() == 1, "recorded exes survive a reload");
        assert!(m.should_restore(me));
        assert!(!m.should_restore(me), "only on first sight");
        assert!(m.exes.is_empty());
    }

    #[test]
    fn unrecorded_exe_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = MutedExes::load(dir.path());
        m.exes.insert("c:\\nothing\\else.exe".to_string());
        assert!(!m.should_restore(std::process::id()));
    }
}
