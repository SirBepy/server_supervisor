# Focus-stealing spec pack

Dev complaint (verbatim): apps launched by this supervisor "open up windows on
my main desktop and cover up my screen, or click around my screen." The second
half is almost certainly focus stealing, not literal clicking: a newly
launched app yanks the foreground window away mid-typing, so keystrokes land
somewhere unintended.

This is a spec pack for a builder, not a narrative. Every claim about the repo
carries a `file:line`. Every claim about Win32 behavior carries a
learn.microsoft.com URL or an explicit UNVERIFIED tag.

---

## Part 1: the spawn path

### 1a. The main spawn call and its creation flags

`src-tauri/src/supervisor/proc/spawn.rs`, `ManagedProc::start()` (signature at
line 71-75):

```rust
pub fn start(
    &mut self,
    dynamic_port: Option<u16>,
    proxy_public_port: Option<u16>,
) -> std::io::Result<u32>
```

The actual child process is built at lines 145-152 and spawned at line 204:

```rust
let mut command = Command::new("cmd");
command
    .arg("/C")
    .arg(&cmd_str)
    .current_dir(&self.spec.cwd)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
...
let mut child = command.spawn()?;
```

The Windows creation-flags block is at lines 194-202:

```rust
#[cfg(windows)]
{
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    // No console window: stdio is piped, so children (dev servers) never
    // need one. Without this every spawn flashes a terminal on Windows.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}
```

Exact current flags: `CREATE_NEW_PROCESS_GROUP` (0x0000_0200) OR'd with
`CREATE_NO_WINDOW` (0x0800_0000). Nothing here touches `wShowWindow` or
foreground behavior at all - `CREATE_NO_WINDOW` only suppresses the console
window that `cmd /C` would otherwise flash; it has no effect on a real GUI
app's own window, which the child creates itself once it starts running.

This is the ONLY site that spawns an arbitrary, unbounded dev-server/app
command (`cmd_str`, built from `self.spec.cmd`, an arbitrary string the user or
the AI-API registered). Every other `creation_flags` site below runs a single
fixed, known Windows utility.

### Every `creation_flags` site in `src-tauri/src` (11 total, more than the "at least eight" asked for)

| # | file:line | What it spawns | GUI-capable? |
|---|---|---|---|
| 1 | `src-tauri/src/ports_os.rs:46` | `netstat -ano` to enumerate ports in use | No - internal helper, output captured, no window possible |
| 2 | `src-tauri/src/supervisor/dev_browser.rs:49` | The pinned CORS-disabled dev Chromium (`chrome.exe --user-data-dir=... --disable-web-security ...`), launched when the user clicks a flutter-web port badge | **Yes** - a real browser window appears, but only on an explicit user click |
| 3 | `src-tauri/src/supervisor/dev_browser.rs:63` | `cmd /C start "" <url>` to open a port in the OS default browser | **Yes** - same, explicit user click (`open_port_url` in `src-tauri/src/ipc/commands/procs.rs:19-30`) |
| 4 | `src-tauri/src/supervisor/validate.rs:163` | `where <exe>` to check a toolchain binary exists | No - internal helper, output captured |
| 5 | `src-tauri/src/supervisor/transient.rs:78` | `git <args>` plumbing (e.g. reading repo metadata) | No - internal helper, output captured |
| 6 | `src-tauri/src/supervisor/spawn_env.rs:125` | `reg.exe query ... /v Path` to read the registry PATH | No - internal helper, output captured |
| 7 | `src-tauri/src/supervisor/proc/spawn.rs:201` | **`cmd /C <arbitrary dev-server/app command>`** - the actual supervised process | **Yes - THE case this spec is about** |
| 8 | `src-tauri/src/supervisor/reaper.rs:48` | `taskkill /T /F /PID <pid>` to kill a process tree | No - internal helper |
| 9 | `src-tauri/src/supervisor/reaper.rs:72` (not a `creation_flags` line itself, args only) / `:93` | `cmd /C netstat -ano \| findstr :<port>` to check port occupancy | No - internal helper, output captured |
| 10 | `src-tauri/src/supervisor/reaper.rs:103` | `tasklist /FI "PID eq <pid>" /NH` to check whether a PID is still alive | No - internal helper |
| 11 | `src-tauri/src/ipc/commands/app.rs:30` | `cmd /c start "" <path>` to open a project folder in Explorer (`open_in_explorer`) | **Yes** - Explorer window, but only on an explicit user click |

**The distinction that matters:** only rows 2, 3, 7, 11 can ever put a new
window in front of the user. Rows 2/3/11 are always a direct, synchronous
result of the user clicking something in the dashboard right now (foreground
window is already the dashboard at that instant, so a resulting browser/
Explorer window is not really a *surprise* focus steal - the user just clicked
something that opens a window). Row 7 is the one the dev is complaining about:
it can fire from a background trigger (HTTP API, autostart, crash-retry) while
the user is doing something completely unrelated in another app, and the
spawned command is arbitrary (Flutter run windows, Electron apps, anything).
**Any fix belongs on row 7 only.** Do not touch rows 1, 4, 5, 6, 8, 9, 10 -
they can never show a window, and CREATE_NO_WINDOW there is load-bearing
(suppresses a console flash), not a focus-stealing concern.

### 1b. Existing window/foreground manipulation code

Grepped `src-tauri/src` for `HWND`, `SetForegroundWindow`, `ShowWindow`, `SW_`,
`AllowSetForegroundWindow`, `STARTUPINFO`, `wShowWindow`,
`LockSetForegroundWindow`: **zero matches.** Nothing in this codebase touches
window handles, show-state, or foreground state today. This is a greenfield
problem - no existing mechanism to build on or conflict with.

### 1c. Where the PID lives

`src-tauri/src/supervisor/proc.rs:32-72`, struct `ManagedProc`:

```rust
pub struct ManagedProc {
    pub spec: ProcSpec,
    pub status: ProcStatus,
    pub pid: Option<u32>,          // <-- line 35
    pub started_at: Option<u64>,
    pub crashed_at: Option<u64>,
    child: Option<Child>,           // std::process::Child, line 41
    stdin: Option<ChildStdin>,
    ...
}
```

Field name and type: `pid: Option<u32>` (`proc.rs:35`), set at
`spawn.rs:238` (`self.pid = Some(pid);`, where `pid = child.id()` at
`spawn.rs:205`). `child: Option<Child>` (`proc.rs:41`) holds the actual
`std::process::Child` handle while the supervisor still owns stdio; on
re-adoption after a restart (`self.adopted = true`) `child` is `None` and only
`pid` survives, polled via OS calls (`reaper::pid_is_our_wrapper`, referenced
at `spawn.rs:53`). A fix that needs to find the child's HWND(s) later (e.g. a
post-hoc `EnumWindows` + `GetWindowThreadProcessId` sweep) has both a live
`Child` (which exposes `.id()` again) and the persisted `pid: Option<u32>` to
key off, in either the freshly-spawned or the re-adopted case.

### 1d. Is the supervisor's own window the foreground window at spawn time?

Three classes of entry point reach `ManagedProc::start` / `Supervisor::start`:

1. **UI click through a Tauri command** (dashboard is open and focused):
   - `src-tauri/src/ipc/commands/procs.rs:33` `start_proc` -> `sup.start(&id)`
   - `src-tauri/src/ipc/commands/procs.rs:43` `restart_proc` -> `sup.restart(&id)`
   - `src-tauri/src/ipc/commands/procs.rs:48` `reload_proc` -> `sup.reload(&id, full)`
   - These fire only when the user clicked a button in the webview, so the
     Tauri window is almost certainly the foreground window at that instant.
     **This is the highest-risk path for a "spawned by the foreground
     process" grant** (see Part 2) - Windows will very likely let the child
     take focus in this case, by design.

2. **The localhost HTTP API** (`src-tauri/src/api.rs`), used by an AI agent,
   independent of what window is focused:
   - `api.rs:232` `start_proc` -> `s.sup.start(&id)`
   - `api.rs:240` `restart_proc` -> `s.sup.restart(&id)`
   - `api.rs:244` `reload_proc` -> `s.sup.reload(&id, true)`
   - `src-tauri/src/api/commands.rs` `run()` (register-and-run, `POST /run`)
   - The dev is very likely in some *other* app (editor, browser) when this
     fires - the supervisor's own window is probably NOT foreground here.
     **This is the lower-risk path**: per the Part 2 rule, the newly spawned
     child is not "spawned by the foreground process," so by default Windows
     should deny it the right to self-activate. This matches the dev's
     complaint pattern less well than path 1 unless something else is
     granting it rights (see caveats in Part 2).

3. **App-launch autostart** (`src-tauri/src/lib.rs:98`,
   `supervisor.start_autostart()`, called from the Tauri `setup` hook before
   the main loop starts pumping) -> `Supervisor::start_autostart` at
   `src-tauri/src/supervisor/registry/lifecycle.rs:74-88` -> `self.start(&id)`
   at line 84. This runs once, automatically, whenever the supervisor process
   itself launches (e.g. at Windows logon if autostart is enabled in
   Settings). At that moment the supervisor's OWN process was *just* created,
   which per the Part 2 rule can itself inherit foreground-setting rights
   from whatever launched IT (explorer.exe / the logon shell). Whether that
   right propagates transitively to the supervisor's own children is
   UNVERIFIED - the docs describe one hop ("started by the foreground
   process"), not a chain, so treat this path as PROBABLY similar risk to
   path 2, not path 1.

4. A crash-retry-on-EADDRINUSE path exists inside `Supervisor::start` itself
   (`lifecycle.rs:190-233`) but it re-enters the same `start()` call already in
   flight from one of the three entry points above - it is not an independent
   trigger.

### 1e. Existing settings/toggle infrastructure

Settings live in `src-tauri/src/settings.rs`. The `Settings` struct
(`settings.rs:11-30`) is a flat, serde-derived, `ts-rs`-derived struct; every
field is a plain scalar (bool/u16) with a `#[serde(default = "...")]` for
backward-compatible deserialization of old `settings.json` files written
before the field existed. Example, an existing boolean end to end:

- Rust field: `#[serde(default = "default_true")] pub show_ram: bool,`
  (`settings.rs:23-24`), defaulted in `Default for Settings`
  (`settings.rs:48`: `show_ram: true`).
- Persistence: `load()`/`save()` (`settings.rs:55-61`) just delegate to
  `tauri_kit_settings::load_for`/`save_for` against `settings.json` - no
  per-field plumbing needed on the Rust persistence side.
- Frontend schema: declared in `src/views/settings/schema.ts` (referenced from
  `src/views/settings/settings.ts:6`, `buildSettingsSchema(token)`) and
  rendered by the vendored kit's `renderSettingsPage`
  (`src/views/settings/settings.ts:1-2,41-68`) - the kit is schema-driven, so a
  new boolean setting is one new field in `Settings` (Rust) plus one new
  schema entry in `schema.ts`, no bespoke UI code. (The exact schema entry
  shape for a checkbox was not read this session - open
  `src/views/settings/schema.ts` before implementing to copy an existing
  boolean entry's shape verbatim.)

---

## Part 2: the Win32 mechanisms

### `SetForegroundWindow` - who is allowed to steal focus

https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow

> The system restricts which processes can set the foreground window. A
> process can set the foreground window by calling **SetForegroundWindow**
> only if:
> - All of the following conditions are true:
>   - The calling process belongs to a desktop application, not a UWP app...
>   - The foreground process has not disabled calls to
>     **SetForegroundWindow** by a previous call to the
>     **LockSetForegroundWindow** function.
>   - No menus are active.
> - Additionally, at least one of the following conditions is true:
>   - The foreground lock time-out has expired (see
>     **SPI_GETFOREGROUNDLOCKTIMEOUT**).
>   - The calling process is the foreground process.
>   - **The calling process was started by the foreground process.**
>   - There is currently no foreground window, and thus no foreground
>     process.
>   - The calling process received the last input event.
>   - Either the foreground process or the calling process is being
>     debugged.
>
> It is possible for a process to be denied the right to set the foreground
> window even if it meets these conditions.

The "spawned by the foreground process" condition ("The calling process was
started by the foreground process") is real and quoted exactly above. This
directly explains why path 1 in section 1d (a UI-click-triggered spawn, where
the supervisor's Tauri window is foreground) is the higher-risk path: the
child dev-server process was literally started by the then-foreground
process, so Windows grants it the right to self-activate. **VERDICT: this
confirms the dev's exact complaint mechanism for UI-triggered starts.**

### `AllowSetForegroundWindow` / `ASFW_ANY` and `LockSetForegroundWindow`

https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-allowsetforegroundwindow

> Enables the specified process to set the foreground window... **The calling
> process must already be able to set the foreground window.** ... If
> [dwProcessId] is **ASFW_ANY**, all processes will be enabled to set the
> foreground window.

This is a **grant-only** API, and directionally the opposite of what's
needed here: it lets a process that already has foreground-setting rights
hand that right to some OTHER named process. There is no argument or mode
that revokes/denies rights from a specific child. **A parent cannot use
`AllowSetForegroundWindow` to deny a child it is about to spawn** - the API
has no such capability. VERDICT: NOT VIABLE for this use case (wrong
direction).

https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-locksetforegroundwindow

> The foreground process can call the **LockSetForegroundWindow** function to
> disable calls to **SetForegroundWindow**.
>
> | LSFW_LOCK (1) | Disables calls to SetForegroundWindow. |
> | LSFW_UNLOCK (2) | Enables calls to SetForegroundWindow. |
>
> The system automatically enables calls to SetForegroundWindow if the user
> presses the ALT key or takes some action that causes the system itself to
> change the foreground window... This function is provided so applications
> can prevent OTHER applications from making a foreground change that can
> interrupt its interaction with the user.

Critical constraint: **only the current foreground process may call this**
(it locks calls made by others, on behalf of itself). The supervisor is not
reliably the foreground process at the moment it wants to protect the dev's
*other* app from being interrupted - the dev's own app (editor/browser) is
the foreground process the dev wants protected, and the supervisor cannot
call `LockSetForegroundWindow` on that app's behalf. Also note the
auto-unlock on any ALT press or system-initiated foreground change, which
makes it fragile even when applicable. VERDICT: NOT VIABLE as a general fix
(wrong caller, auto-resets, and would also block legitimate foreground
changes for everything, not just this one child).

### `ForegroundLockTimeout` (`SPI_GETFOREGROUNDLOCKTIMEOUT` / `SPI_SETFOREGROUNDLOCKTIMEOUT`)

The `SystemParametersInfo` docs page describes the parameter (constants
`0x2000`/`0x2001`) as: retrieves/sets "the amount of time following user
input, in milliseconds, during which the system will not allow applications
to force themselves into the foreground." I could not get the official page
to state a numeric default in the fetched content - **UNVERIFIED: exact
default value on Windows 11** (commonly cited elsewhere as 0, i.e. no lockout
by default on modern consumer Windows, but that figure did not come from an
MS Learn page reachable this session, so treat it as unconfirmed).

The caveat the task asked to flag is confirmed structurally regardless of the
exact default: this is a single **machine-wide** value read/written via
`SystemParametersInfo`, not a per-process or per-app setting - changing it
would alter foreground-stealing behavior for every application on the user's
machine, not just this supervisor's children. VERDICT: NOT VIABLE (global
blast radius is disproportionate to a per-app problem; also doesn't
distinguish this supervisor's children from anything else).

### `STARTUPINFO.wShowWindow` + `STARTF_USESHOWWINDOW`

https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/ns-processthreadsapi-startupinfow

> `wShowWindow`: If **dwFlags** specifies STARTF_USESHOWWINDOW, this member
> can be any of the values that can be specified in the *nCmdShow* parameter
> for the ShowWindow function... For GUI processes, **the first time
> ShowWindow is called, its nCmdShow parameter is ignored; wShowWindow
> specifies the default value.** In subsequent calls to ShowWindow, the
> wShowWindow member is used [only] if nCmdShow is set to SW_SHOWDEFAULT.

And from https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-showwindow
on the `nCmdShow` parameter of `ShowWindow`:

> This parameter is ignored the first time an application calls
> **ShowWindow**, if the program that launched the application provides a
> STARTUPINFO structure. Otherwise, the first time ShowWindow is called, the
> value should be the value obtained by the WinMain function in its nCmdShow
> parameter.

So `SW_SHOWNOACTIVATE` (4) / `SW_SHOWMINNOACTIVE` (7) via
`wShowWindow`+`STARTF_USESHOWWINDOW` DOES suppress activation on a GUI app's
FIRST `ShowWindow` call *by the letter of the spec, IF that app's toolkit
actually calls `ShowWindow(hwnd, nCmdShow)` with the WinMain-provided
`nCmdShow` on startup* (the conventional, well-behaved pattern). **Honest
reliability assessment**: this is a convention, not an enforcement - nothing
stops (and in practice much cross-platform tooling does) a GUI app calling
`ShowWindow(hwnd, SW_SHOW)` unconditionally on its own main window, ignoring
whatever `nCmdShow`/`wShowWindow` the launcher supplied. Flutter's Windows
embedder, Electron/Chromium-based apps, and many Node/Electron dev-server GUI
shells are exactly this kind of toolkit - they do not thread the
launch-provided show-state hint through. VERDICT: PARTIAL - correct for a
well-behaved native Win32 app, unreliable for the actual population of apps
this supervisor launches (dev servers wrapping Flutter/Electron/etc.).

**Rust `std::process::Command` exposure**: fetched
https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html - on
**stable** Rust, `CommandExt` exposes `creation_flags` (stable since 1.16.0)
and `raw_arg` (stable since 1.62.0). A `show_window(cmd_show: u16)` method
exists that sets `wShowWindow` directly, but it is gated behind an unstable
feature (nightly-only, confirmed in the fetched docs) - **not usable on
stable Rust**, which this project builds on. So using `wShowWindow` from this
codebase, as the task anticipated, requires either a raw `CreateProcessW`
call (bypassing `std::process::Command` entirely) or a crate that wraps it
(e.g. one that builds its own `STARTUPINFOW`).

### Post-hoc detect-and-suppress (let it flash, then push it back)

Mechanism: capture `GetForegroundWindow()` before spawning; after spawn,
poll/hook for the new top-level window(s) belonging to the child's PID (e.g.
`EnumWindows` + `GetWindowThreadProcessId`, matching against the stored
`pid: Option<u32>` from section 1c); call
`ShowWindow(newHwnd, SW_SHOWNOACTIVATE)` and/or
`SetWindowPos(newHwnd, HWND_BOTTOM, 0,0,0,0, SWP_NOACTIVATE)`
(https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos
confirms `HWND_BOTTOM` = "(HWND)1, Places the window at the bottom of the Z
order" and `SWP_NOACTIVATE` = "0x0010, Does not activate the window"), then
call `SetForegroundWindow()` again on the previously-captured handle to
restore it.

Race assessment: this is fundamentally reactive - the child's window is
created and (if the child's toolkit self-activates, per the PARTIAL verdict
above) already stolen focus and possibly already painted a frame or two
before the supervisor's poll loop can observe the new HWND, diff it against
the pre-spawn foreground handle, and act. On a modern machine this window is
likely tens of milliseconds, so a brief visible flash/flicker of the stolen
window before it gets shoved back and focus restored is the realistic
outcome, not a clean zero-flash suppression. **This is, however, what several
real-world "focus stealing prevention" utilities for Windows actually do**
(reactive detect + reassert), precisely because the deny-in-advance paths in
this section (`AllowSetForegroundWindow`, `LockSetForegroundWindow`,
`ForegroundLockTimeout`) don't fit a single app's single-child use case.
VERDICT: PARTIAL - works well enough in practice for the common case (window
snaps back before the dev has typed more than a keystroke or two), not a
provably flash-free guarantee, and adds real implementation cost (a
polling/hook loop, PID-to-HWND matching, race-safe restore of the prior
foreground handle).

### Summary verdict table

| Mechanism | Verdict | Why |
|---|---|---|
| `AllowSetForegroundWindow` | NOT VIABLE | Grant-only API; no deny direction exists |
| `LockSetForegroundWindow` | NOT VIABLE | Only the current foreground process may call it; supervisor isn't reliably that process; auto-unlocks on ALT/user input |
| `ForegroundLockTimeout` (SPI_*) | NOT VIABLE | System-wide setting; wrong blast radius for a per-app fix; default value UNVERIFIED |
| `wShowWindow` / `STARTF_USESHOWWINDOW` via raw `CreateProcessW` or a crate | PARTIAL | Real Win32 mechanism, but only works if the child's own toolkit respects the launcher's show-state hint on its first `ShowWindow` call; unreliable for Flutter/Electron-style children; unavailable via stable `std::process::Command` (`show_window` is nightly-only) |
| Post-hoc detect + `ShowWindow(SW_SHOWNOACTIVATE)` / `SetWindowPos(HWND_BOTTOM, SWP_NOACTIVATE)` + restore prior foreground | PARTIAL | Matches what real tools do; brief flash is the realistic outcome, not a guarantee; needs a PID-to-HWND watcher loop |
| Not spawning the risky commands from a UI-click context in the first place (see recommendation below) | VIABLE for the highest-risk path only | Removes the "started by the foreground process" grant condition for UI-triggered starts; does nothing for autostart/API-triggered starts whose child toolkit still self-activates on its own |

---

## Part 3: recommendation

**Bottom line: there is no clean, guaranteed way to stop a determined GUI app
from stealing focus on Windows once it decides to call `ShowWindow(SW_SHOW)`
on itself.** Every deny-in-advance Win32 API is either grant-only
(`AllowSetForegroundWindow`), scoped to "the current foreground process
protecting itself" (`LockSetForegroundWindow`, and even then only until the
next ALT press), or system-wide (`ForegroundLockTimeout`). The one
per-launch lever that actually exists (`wShowWindow` via `STARTF_USESHOWWINDOW`)
is a hint the child's toolkit is free to ignore, and Flutter/Electron-style
dev-server GUIs are exactly the toolkits known to ignore it. This is a
legitimate, useful finding on its own: don't promise the dev a fix that fully
solves this.

Ranked, cheapest first:

1. **Try the `wShowWindow` hint anyway, as a cheap first layer.** Effort: ~1
   file touched (`src-tauri/src/supervisor/proc/spawn.rs`, plus a new small
   dependency or a hand-rolled `CreateProcessW` call, since stable Rust's
   `Command` doesn't expose it). Set `STARTF_USESHOWWINDOW` +
   `SW_SHOWMINNOACTIVE` on the row-7 spawn only (the arbitrary
   dev-server/app command). Cheap, harmless, and helps for any child that
   *does* respect the hint - but per the PARTIAL verdict above, expect it to
   do nothing for a chunk of real-world targets. Gate it behind the new
   settings toggle from item 3 below so it can be disabled if a particular
   dev-server command needs to be visible on launch (e.g. one the dev
   deliberately wants to see).

2. **Post-hoc detect + suppress + restore foreground, as the real fix.**
   Effort: ~2-3 files touched: a new small module (e.g.
   `src-tauri/src/supervisor/focus_guard.rs`) implementing "capture
   `GetForegroundWindow()` before spawn, poll for new top-level windows
   belonging to the freshly-spawned PID (using the existing
   `pid: Option<u32>` from `proc.rs:35`) for a bounded window (e.g. a few
   hundred ms), and on each new HWND found call
   `ShowWindow(SW_SHOWNOACTIVATE)` + restore the captured foreground handle
   via `SetForegroundWindow`"; a call-site change in
   `src-tauri/src/supervisor/proc/spawn.rs` around the existing
   `child.spawn()` call (line 204) to invoke it; and a settings field +
   schema entry (item 3) to gate it. This is the option that would actually
   address the dev's complaint in the common case, with the caveat (stated
   plainly above) that a brief flash is the realistic outcome, not a
   flash-free guarantee.

3. **Add the settings toggle itself** (needed by both 1 and 2 above, so not
   really an independent increment): one new `bool` field in `Settings`
   (`src-tauri/src/settings.rs:12-30`, following the exact `show_ram` pattern
   at lines 17-18/23-24/48) plus one new schema entry in
   `src/views/settings/schema.ts`. Effort: ~2 files touched.

**Pick: option 2 (post-hoc detect + suppress), gated by the toggle in option
3, applied only at `spawn.rs:201`'s call site.** Reasoning: option 1 alone is
too unreliable against the actual target population (Flutter/Electron dev
servers) to be worth calling "fixed"; option 2 is the mechanism real
focus-stealing-prevention tools use and is the only one with a realistic
chance of visibly helping. Layering option 1 on top of option 2 is cheap and
strictly additive, so do both, but option 2 is the one doing the real work.
Do **not** attempt options in the NOT VIABLE row of the Part 2 table
(`AllowSetForegroundWindow`, `LockSetForegroundWindow`,
`ForegroundLockTimeout`) - they were investigated and ruled out above, not
merely skipped.

**Scope reminder for the builder**: apply any of this only to the row-7
spawn (`spawn.rs:201`'s `command.spawn()` at line 204) and, if desired,
symmetrically to the two explicit-user-click GUI spawns (rows 2/3/11:
`dev_browser.rs`, `app.rs:30`) if the dev also wants those suppressed - but
those are lower priority since they're synchronous with a click the user just
made. Never touch the seven internal-helper `creation_flags` sites (rows 1,
4, 5, 6, 8, 9, 10) - they never show a window and their existing
`CREATE_NO_WINDOW` flag is unrelated to this problem.
