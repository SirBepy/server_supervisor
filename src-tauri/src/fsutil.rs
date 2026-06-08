//! Crash-safe file writes shared by the JSON persisters (projects.json,
//! pids.json, ports.json). A bare `fs::write` truncates the target and then
//! writes in place, so a crash mid-write - exactly when the supervisor owns
//! live children - leaves a half-written, unparseable file. `write_atomic`
//! writes to a sibling `<path>.tmp` and renames it over the target; rename is
//! atomic on NTFS (Rust's `fs::rename` uses MoveFileEx with REPLACE_EXISTING),
//! so a reader sees either the old file or the complete new one, never a torn one.

use std::ffi::OsString;
use std::io;
use std::path::Path;

pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // Append ".tmp" (rather than replacing the extension) so the temp name can't
    // collide with another persisted file that shares this stem.
    let mut tmp: OsString = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_existing_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        write_atomic(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        // Overwrites in place.
        write_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        // The temp file is renamed away, not left behind.
        assert!(!dir.path().join("data.json.tmp").exists());
    }
}
