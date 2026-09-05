//! Per-project disk-usage sampler (`node_modules`, `.dart_tool`, `build/`,
//! `target/`, ...). Deliberately NOT an extension of `sampler.rs`'s ~2.5s
//! RAM/CPU tick - a recursive size walk is far pricier per-project, so this
//! owns its own cache and background worker on a much slower cadence.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Minimum interval between two walks of the same project's tree.
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
/// How often the background worker wakes to check for overdue projects.
const TICK_INTERVAL: Duration = Duration::from_secs(5);

struct Cached {
    bytes: u64,
    sampled_at: Instant,
}

struct DiskState {
    /// Project id -> tree root, refreshed on every `sample_and_snapshot` call.
    tracked: Mutex<HashMap<String, PathBuf>>,
    cache: Mutex<HashMap<String, Cached>>,
}

static STATE: OnceLock<Arc<DiskState>> = OnceLock::new();

fn state() -> Arc<DiskState> {
    STATE
        .get_or_init(|| {
            let state = Arc::new(DiskState {
                tracked: Mutex::new(HashMap::new()),
                cache: Mutex::new(HashMap::new()),
            });
            spawn_worker(state.clone());
            state
        })
        .clone()
}

/// The one background worker thread. Wakes every `TICK_INTERVAL`, walks any
/// tracked project whose cache entry is missing or older than
/// `REFRESH_INTERVAL`, and writes the result back. Readers never wait on
/// this - they only ever read whatever `cache` last held.
fn spawn_worker(state: Arc<DiskState>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(TICK_INTERVAL);
        let due: Vec<(String, PathBuf)> = {
            let tracked = state.tracked.lock().unwrap();
            let cache = state.cache.lock().unwrap();
            tracked
                .iter()
                .filter(|(id, _)| {
                    cache
                        .get(id.as_str())
                        .map(|c| c.sampled_at.elapsed() >= REFRESH_INTERVAL)
                        .unwrap_or(true)
                })
                .map(|(id, root)| (id.clone(), root.clone()))
                .collect()
        };
        for (id, root) in due {
            let bytes = walk_dir_size(&root);
            state
                .cache
                .lock()
                .unwrap()
                .insert(id, Cached { bytes, sampled_at: Instant::now() });
        }
    });
}

/// Sum real file sizes under `root`, recursively, skipping nothing (caches
/// like `node_modules`/`target` count toward the total same as source). A
/// symlink is never followed - only counted as zero - so a symlink loop
/// cannot hang this worker.
fn walk_dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else { continue };
            if file_type.is_symlink() {
                continue;
            } else if file_type.is_dir() {
                stack.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// Register `roots` (current project id -> tree root pairs) with the
/// background worker, drop tracking for any project id no longer present,
/// and return the latest cached byte count for each - omitting any id never
/// sampled yet. Never walks synchronously.
pub fn sample_and_snapshot(roots: &[(String, PathBuf)]) -> HashMap<String, u64> {
    let state = state();
    {
        let mut tracked = state.tracked.lock().unwrap();
        for (id, root) in roots {
            tracked.insert(id.clone(), root.clone());
        }
        let known: HashSet<&str> = roots.iter().map(|(id, _)| id.as_str()).collect();
        tracked.retain(|id, _| known.contains(id.as_str()));
    }
    let cache = state.cache.lock().unwrap();
    roots
        .iter()
        .filter_map(|(id, _)| cache.get(id).map(|c| (id.clone(), c.bytes)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_dir_size_sums_nested_files() {
        let unique = format!(
            "disk_walk_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("a.txt"), vec![0u8; 100]).unwrap();
        std::fs::write(base.join("sub").join("b.txt"), vec![0u8; 250]).unwrap();

        let total = walk_dir_size(&base);

        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(total, 350);
    }
}
