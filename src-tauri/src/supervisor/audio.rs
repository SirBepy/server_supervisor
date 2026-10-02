//! Per-process audio for supervised apps. A background watcher keeps every
//! supervised process tree muted unless its command opted into sound
//! (`Command::play_sound`), and `listen` lets an agent hear an app through
//! its session meters even while the dev hears nothing.
//!
//! The mute is applied after Windows creates the app's audio session, so a
//! sound played in the very first `WATCH_INTERVAL` of a session can still
//! leak. Apps normally open their session at startup, well before their
//! first sound, which is what makes polling acceptable here.

mod listen;
mod restore;
mod session;

pub use listen::{listen, Listening};

use super::Supervisor;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const WATCH_INTERVAL: Duration = Duration::from_millis(100);
const IDLE_INTERVAL: Duration = Duration::from_secs(1);

impl Supervisor {
    /// Root pid -> play_sound for every proc that currently has a pid.
    fn sound_policy(&self) -> HashMap<u32, bool> {
        self.procs
            .lock()
            .unwrap()
            .values()
            .filter_map(|p| p.pid.map(|pid| (pid, p.spec.play_sound)))
            .collect()
    }
}

/// Starts the mute watcher on its own thread (COM objects are bound to the
/// thread that created them). Logs and gives up if Core Audio is
/// unavailable, rather than taking the supervisor down with it.
pub fn spawn_watcher(sup: Arc<Supervisor>) {
    let spawned = std::thread::Builder::new().name("audio-watcher".into()).spawn(move || {
        let scanner = match session::Scanner::new() {
            Ok(s) => s,
            Err(e) => {
                log::warn!("audio watcher disabled, Core Audio unavailable: {e}");
                return;
            }
        };
        let mut muted_exes = restore::MutedExes::load(&sup.data_dir);
        let mut policy = HashMap::new();
        loop {
            // Nothing supervised has a session to mute, so only the slow
            // restore check is left to do.
            std::thread::sleep(if policy.is_empty() { IDLE_INTERVAL } else { WATCH_INTERVAL });
            policy = sup.sound_policy();
            // Runs even with no supervised procs: that is exactly when the
            // dev's own copy of a once-muted exe needs its sound back.
            scanner.apply(&policy, &mut muted_exes);
        }
    });
    if let Err(e) = spawned {
        log::warn!("could not start the audio watcher thread: {e}");
    }
}
