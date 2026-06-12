//! Open a supervised process's port in a browser when its dashboard badge is
//! clicked.
//!
//! Flutter web is the painful case: a dev build talking to a local backend trips
//! the browser's CORS / same-origin checks, drowning the app in errors. The
//! standard escape hatch is `flutter run -d chrome --web-browser-flag=
//! --disable-web-security`, which launches Chrome with web security off in an
//! isolated profile. We do the same thing here, but as a persistent dedicated
//! dev browser: a Chromium instance pinned to a fixed `--user-data-dir` with
//! `--disable-web-security`. Launching that same binary again with a URL routes
//! it to the already-open window as a NEW TAB (Chrome's single-instance behaviour
//! per user-data-dir), so every flutter port the user clicks lands in the one
//! security-disabled window.
//!
//! Non-flutter ports open in the OS default browser, untouched and secure.
//!
//! The browser is spawn-and-forget: it is NOT a supervised child. The supervisor
//! never owns, tracks, or kills it.

use std::path::{Path, PathBuf};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Open `url` in the dedicated CORS-disabled dev browser, keyed to `profile_dir`
/// (reused across sessions so the same window persists). Falls back to the
/// default browser if no Chromium browser is installed - the URL still opens, it
/// just won't have web security disabled.
pub fn open_flutter_web(url: &str, profile_dir: &Path) -> Result<(), String> {
    let Some(browser) = chromium_path() else {
        return open_default(url);
    };
    let _ = std::fs::create_dir_all(profile_dir);

    let mut cmd = std::process::Command::new(&browser);
    cmd.arg(format!("--user-data-dir={}", profile_dir.display()))
        // The reason this whole module exists: kill the CORS / same-origin
        // errors that make flutter web + local backend dev miserable. Only
        // honoured because we also pass a non-default user-data-dir above.
        .arg("--disable-web-security")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg(url);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Suppress the console window on the spawn itself; the browser GUI shows
        // normally. Matches the rest of the codebase's child spawns.
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Open `url` in the OS default browser.
#[cfg(windows)]
pub fn open_default(url: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // `cmd /C start "" "<url>"` hands the URL to the default handler. The empty
    // "" is the (required) window-title argument so the URL isn't consumed as a
    // title when it is quoted.
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
pub fn open_default(url: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Locate a Chromium browser: Chrome first (it matches flutter's default `-d
/// chrome` web device), then Edge (always present on Windows 11). The first path
/// that exists wins; `None` if neither is installed.
#[cfg(windows)]
pub fn chromium_path() -> Option<PathBuf> {
    chromium_candidates().into_iter().find(|p| p.exists())
}

/// Candidate install locations, in preference order: Chrome (machine, then
/// per-user), then Edge. Pure (drives off env vars) so the ordering is testable.
#[cfg(windows)]
fn chromium_candidates() -> Vec<PathBuf> {
    let dir = |k: &str| std::env::var(k).ok().map(PathBuf::from);
    let pf = dir("ProgramFiles");
    let pf86 = dir("ProgramFiles(x86)");
    let local = dir("LOCALAPPDATA");

    let mut out = Vec::new();
    // Chrome: Program Files, Program Files (x86), then a per-user install.
    for base in [pf.clone(), pf86.clone(), local].into_iter().flatten() {
        out.push(base.join(r"Google\Chrome\Application\chrome.exe"));
    }
    // Edge: shipped in Program Files (x86) on Windows 11, but probe both.
    for base in [pf86, pf].into_iter().flatten() {
        out.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
    }
    out
}

#[cfg(not(windows))]
pub fn chromium_path() -> Option<PathBuf> {
    // The dashboard ships on Windows; on other targets report "not found" so the
    // flutter-web path falls back to xdg-open via open_default.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn chrome_is_preferred_over_edge() {
        // Whatever the host has installed, the candidate ORDER must list every
        // Chrome path before any Edge path (Chrome matches flutter's default
        // device). This pins the preference without depending on what exists.
        let cands = chromium_candidates();
        let first_edge = cands.iter().position(|p| p.ends_with("msedge.exe"));
        let last_chrome = cands.iter().rposition(|p| p.ends_with("chrome.exe"));
        if let (Some(edge), Some(chrome)) = (first_edge, last_chrome) {
            assert!(chrome < edge, "all Chrome candidates must precede Edge");
        }
    }
}
