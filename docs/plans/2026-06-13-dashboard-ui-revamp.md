# Dashboard UI Revamp Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reshape the dashboard's bare project headers into structured rows (status dot + icon + name + hover actions), add a project-icon → tech-logo → generic icon chain, a subtle animated background, and density settings, without touching the command card.

**Architecture:** Frontend-only Phase 1 (lit-html rows, Devicon webfont for tech logos, CSS aurora, three new settings). Additive Phase 2 adds a Rust IPC command that scans a project folder for a real icon and serves it as a base64 data URI, slotting in as tier 1 of the chain.

**Tech Stack:** Tauri 2 (Rust), vanilla TypeScript + lit-html + Vite, ts-rs type generation, Devicon webfont, Phosphor (existing).

**Testing reality:** This repo has no JS unit-test runner. Frontend tasks are verified with `npx tsc --noEmit` + `npm run build` and explicitly flagged for Joe's visual review (native webview, not Claude-verifiable). Rust logic (settings defaults, Phase 2 icon resolver) gets real `cargo test` coverage. Always run commands with `--prefix` / `git -C`, never `cd` (see CLAUDE.md memory).

**Spec:** `docs/specs/2026-06-12-dashboard-ui-revamp-design.md`

---

## File structure

**Phase 1**
- `package.json` — add `devicon` dependency (pinned).
- `src/main.ts` — import Devicon CSS; add aurora visibility-pause listener.
- `index.html` — add the `.app-aurora` background layer element.
- `src/styles/base.css` — aurora layer styles + perf guards.
- `src/views/dashboard/helpers.ts` — pure tech-detection helpers (`techFromCmd`, `projectTech`, `deviconClass`).
- `src-tauri/src/settings.rs` — three new `Settings` fields + defaults + a defaults test.
- `src/views/settings/schema.ts` — new "Dashboard" settings section (3 toggles).
- `src/views/dashboard/state.ts` — `ui` fields for the three prefs.
- `src/views/dashboard/dashboard.ts` — load prefs on mount; new project-row render + start-all; gate RAM/port cells + count.
- `src/views/dashboard/dashboard.css` — new `.prow*` styles; remove old `.group-head/.group-chevron/.run-count/.run-dot` hover rules.

**Phase 2**
- `src-tauri/src/icons.rs` — `find_icon_file` resolver (tested) + `get_project_icon` command.
- `src-tauri/src/lib.rs` (or wherever `generate_handler!` lives) — register the command + `mod icons`.
- `src-tauri/Cargo.toml` — add `base64` dep.
- `src/shared/ipc.ts` — `getProjectIcon` wrapper.
- `src/views/dashboard/state.ts` — `iconCache` field.
- `src/views/dashboard/dashboard.ts` — wire tier-1 project icon with `<img>` + onerror fallback.

---

## PHASE 1

### Task 1: Add the Devicon webfont dependency

**Files:**
- Modify: `package.json`
- Modify: `src/main.ts:2` (import block)

- [ ] **Step 1: Safety-check the package**

Devicon is the well-known logo webfont (github.com/devicons/devicon, npm `devicon`). Confirm legitimacy + current version and that no advisory is open against the pinned version:

Run: `npm view devicon version`
Run: `npm view devicon dist.integrity homepage`
Expected: a real version (e.g. `2.x.x`), homepage `https://devicon.dev`. If anything looks off (typosquat, no recent releases), STOP and ask Joe.

- [ ] **Step 2: Install it pinned**

Run: `npm install --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor" --save-exact devicon@<version-from-step-1>`
Expected: `package.json` `dependencies` gains `"devicon": "<version>"` (exact, no caret).

- [ ] **Step 3: Import the Devicon CSS**

In `src/main.ts`, add after the Phosphor import (line 1):

```ts
import "@phosphor-icons/web/regular";
import "devicon/devicon.min.css";
import "./styles/base.css";
```

- [ ] **Step 4: Verify it builds and the font resolves**

Run: `npm run build --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor"`
Expected: build succeeds; the Devicon font files are emitted to `dist/assets`. (Vite bundles the `url()` font refs from the imported CSS.)

- [ ] **Step 5: Commit**

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add package.json package-lock.json src/main.ts
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "CHORE: add devicon webfont for project tech logos"
```

---

### Task 2: Tech-detection helpers (pure)

These mirror the existing `deriveName` program-parsing so the project row can pick a tech logo. No state, no IPC.

**Files:**
- Modify: `src/views/dashboard/helpers.ts` (append to the file)

- [ ] **Step 1: Add the tech key type + program map**

Append to `src/views/dashboard/helpers.ts`:

```ts
import type { Project, ProcInfo } from "../../types/ipc.generated";

// Tech identities we can draw a Devicon logo for.
export type TechKey =
  | "rust" | "flutter" | "node" | "python" | "go" | "deno" | "docker" | "dotnet";

// Program basename (lowercased, extension-stripped) -> tech. Mirrors deriveName's
// program parsing so the row logo matches how commands are actually launched.
const TECH_BY_PROG: Record<string, TechKey> = {
  cargo: "rust", rustc: "rust", rustup: "rust",
  flutter: "flutter", dart: "flutter",
  npm: "node", pnpm: "node", yarn: "node", bun: "node", npx: "node", node: "node",
  python: "python", python3: "python", py: "python",
  go: "go",
  deno: "deno",
  docker: "docker", "docker-compose": "docker",
  dotnet: "dotnet",
};

// Detect the tech of a single command string from its program token.
// Handles the fvm wrapper Joe uses for Flutter ("fvm flutter run").
export function techFromCmd(cmd: string): TechKey | null {
  const toks = cmd.trim().split(/\s+/).filter(Boolean);
  if (toks.length === 0) return null;
  const prog = basename(toks[0]).replace(/\.(exe|cmd|bat|sh|ps1)$/i, "").toLowerCase();
  if (prog === "fvm" && toks[1] && basename(toks[1]).toLowerCase().startsWith("flutter")) {
    return "flutter";
  }
  return TECH_BY_PROG[prog] ?? null;
}
```

- [ ] **Step 2: Add the project-level resolver + Devicon class map**

Continue appending to `helpers.ts`:

```ts
// A project's tech: prefer a currently-live command's tech (that's what the user
// cares about right now), else the first command with a detectable tech.
export function projectTech(
  project: Project,
  statusById: Record<string, ProcInfo>,
): TechKey | null {
  const isLive = (c: Project["commands"][number]) => {
    const st = statusById[`${project.id}:${c.id}`]?.status;
    return st === "running" || st === "starting";
  };
  const live = project.commands.find(isLive);
  const ordered = live ? [live, ...project.commands] : project.commands;
  for (const c of ordered) {
    const t = techFromCmd(c.cmd);
    if (t) return t;
  }
  return null;
}

// Devicon class per tech. `colored` is appended where the brand has a colored
// variant; rust has none (tinted via CSS instead). VERIFY these class names
// against the installed devicon.min.css (see Step 3) - a few differ from the
// obvious spelling (e.g. deno is `denojs`, .NET is `dotnetcore`).
const DEVICON: Record<TechKey, string> = {
  rust: "devicon-rust-plain",
  flutter: "devicon-flutter-plain colored",
  node: "devicon-nodejs-plain colored",
  python: "devicon-python-plain colored",
  go: "devicon-go-original-wordmark colored",
  deno: "devicon-denojs-original",
  docker: "devicon-docker-plain colored",
  dotnet: "devicon-dotnetcore-plain colored",
};

export function deviconClass(tech: TechKey): string {
  return DEVICON[tech];
}
```

- [ ] **Step 3: Verify the Devicon class names actually exist**

Use the Grep tool (output_mode "content", `-o`) with pattern `devicon-(rust|flutter|nodejs|python|go|denojs|docker|dotnetcore)-[a-z-]+` on file `node_modules/devicon/devicon.min.css`.
Expected: each class from the `DEVICON` map appears in the file. If one doesn't (Devicon occasionally renames, e.g. deno is `denojs`, .NET is `dotnetcore`, Go's colored mark is `go-original-wordmark`), find the real class for that tech in the CSS and correct the map. Do not leave a class that isn't in the file - it renders as a blank box.

- [ ] **Step 4: Typecheck**

Run: `npx --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor" tsc --noEmit`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add src/views/dashboard/helpers.ts
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "FEAT: tech-detection helpers for project tech logos"
```

---

### Task 3: New settings (Rust fields + schema + types)

**Files:**
- Modify: `src-tauri/src/settings.rs:11-44`
- Modify: `src/views/settings/schema.ts:6-48`

- [ ] **Step 1: Write the failing Rust defaults test**

Append to `src-tauri/src/settings.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn omitted_dashboard_prefs_use_defaults() {
        // An empty settings object must deserialize to the locked defaults:
        // count off, RAM/port on.
        let s: Settings = serde_json::from_str("{}").expect("deserialize {}");
        assert_eq!(s.show_command_count, false);
        assert_eq!(s.show_ram, true);
        assert_eq!(s.show_port, true);
    }
}
```

- [ ] **Step 2: Run it to confirm it fails to compile**

Run: `cargo test --manifest-path "C:/Users/tecno/Desktop/Projects/server_supervisor/src-tauri/Cargo.toml" omitted_dashboard_prefs_use_defaults`
Expected: FAIL — `no field show_command_count on type Settings` (fields not added yet). If `serde_json` is missing from dev-deps, add it: `cargo add serde_json --dev --manifest-path "C:/Users/tecno/Desktop/Projects/server_supervisor/src-tauri/Cargo.toml"`.

- [ ] **Step 3: Add the three fields + defaults**

In `src-tauri/src/settings.rs`, add to the `Settings` struct (after `ai_can_add_projects`, before the `kit` flatten):

```rust
    #[serde(default = "default_true")]
    pub ai_can_add_projects: bool,
    #[serde(default)] // default false
    pub show_command_count: bool,
    #[serde(default = "default_true")]
    pub show_ram: bool,
    #[serde(default = "default_true")]
    pub show_port: bool,
    #[serde(flatten)]
    #[ts(skip)]
    pub kit: KitSettings,
```

And in the `Default` impl, add the matching fields:

```rust
            ai_can_add_projects: true,
            show_command_count: false,
            show_ram: true,
            show_port: true,
            kit: KitSettings::default(),
```

- [ ] **Step 4: Run the test to confirm it passes**

Run: `cargo test --manifest-path "C:/Users/tecno/Desktop/Projects/server_supervisor/src-tauri/Cargo.toml" omitted_dashboard_prefs_use_defaults`
Expected: PASS.

- [ ] **Step 5: Regenerate the TS types**

Run: `npm run gen-types --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor"`
Expected: `src/types/ipc.generated.ts` `Settings` type now includes `show_command_count: boolean, show_ram: boolean, show_port: boolean`.

- [ ] **Step 6: Add the Dashboard settings section**

In `src/views/settings/schema.ts`, add a new section to the `sections` array (after the "App" section):

```ts
      {
        title: "Dashboard",
        fields: [
          {
            key: "show_command_count",
            kind: "toggle",
            label: "Show command count",
            tooltip: "Show how many commands each project has on its row.",
          },
          {
            key: "show_ram",
            kind: "toggle",
            label: "Show RAM",
            tooltip: "Show the RAM stat on running command cards.",
          },
          {
            key: "show_port",
            kind: "toggle",
            label: "Show port",
            tooltip: "Show the port stat on running command cards.",
          },
        ],
      },
```

- [ ] **Step 7: Typecheck + build**

Run: `npx --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor" tsc --noEmit`
Expected: no errors.

- [ ] **Step 8: Commit**

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add src-tauri/src/settings.rs src-tauri/Cargo.toml src-tauri/Cargo.lock src/views/settings/schema.ts src/types/ipc.generated.ts
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "FEAT: dashboard density settings (command count, RAM, port)"
```

---

### Task 4: Dashboard reads the prefs

**Files:**
- Modify: `src/views/dashboard/state.ts:88-119` (`ui` object)
- Modify: `src/views/dashboard/dashboard.ts:17-52` (`mountDashboard`)

- [ ] **Step 1: Add pref fields to `ui`**

In `src/views/dashboard/state.ts`, add to the `ui` object (after `openCmdMenuFor`):

```ts
  openCmdMenuFor: null as string | null,
  // Dashboard density prefs, loaded from settings on mount (see loadPrefs).
  // Defaults mirror the Rust Settings defaults so first paint matches.
  showCommandCount: false,
  showRam: true,
  showPort: true,
```

- [ ] **Step 2: Load prefs on mount**

In `src/views/dashboard/dashboard.ts`, inside `mountDashboard`, after `void refresh();` (line 20):

```ts
  setDraw(draw);
  void refresh();
  void loadPrefs();
```

And add the function near the other top-level helpers (after `mountDashboard`'s closing brace, before `toggleLogs`):

```ts
// Read the density prefs from settings into ui state, then redraw. Runs on every
// dashboard mount - route() remounts the dashboard when returning from #settings,
// so this also picks up changes the user just made without a separate subscription.
async function loadPrefs() {
  try {
    const s = await ipc.getSettings();
    ui.showCommandCount = s.show_command_count;
    ui.showRam = s.show_ram;
    ui.showPort = s.show_port;
    draw();
  } catch {
    // Settings unavailable (e.g. IPC down): keep the defaults already in ui.
  }
}
```

- [ ] **Step 3: Typecheck**

Run: `npx --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor" tsc --noEmit`
Expected: no errors (`getSettings` already exists in `ipc.ts`; `Settings` now has the fields from Task 3).

- [ ] **Step 4: Commit**

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add src/views/dashboard/state.ts src/views/dashboard/dashboard.ts
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "FEAT: load dashboard density prefs on mount"
```

---

### Task 5: The new project row

Reshape `projectSection`'s header into the row, add start-all, gate RAM/port + count. The command-card list below is unchanged.

**Files:**
- Modify: `src/views/dashboard/dashboard.ts` — `moreMenu`, `projectSection`, `commandRow` stats; add `startAll`, `projectIconTemplate`.
- Modify: `src/views/dashboard/dashboard.css` — new `.prow*` rules, remove old header rules.

- [ ] **Step 1: Add `startAll` + the icon template**

In `src/views/dashboard/dashboard.ts`, add imports at the top (extend the helpers import on line 10 and the ipc import):

```ts
import { formatBytes, displayName, formatUptime, projectTech, deviconClass } from "./helpers";
```

Add these two functions above `moreMenu`:

```ts
// Start every command in the project that isn't already live. The row's play
// button; analogous to the command card's per-command start.
function startAll(project: Project) {
  for (const c of project.commands) {
    const id = `${project.id}:${c.id}`;
    const st = ui.statusById[id]?.status;
    if (st !== "running" && st !== "starting") void act(ipc.startProc(id));
  }
}

// True if the project has at least one command that could be started.
function hasStartable(project: Project): boolean {
  return project.commands.some((c) => {
    const st = ui.statusById[`${project.id}:${c.id}`]?.status;
    return st !== "running" && st !== "starting";
  });
}

// The project's icon slot. Phase 1: tech logo (Devicon) if detectable, else a
// generic Phosphor terminal glyph. Phase 2 prepends a real project icon.
function projectIconTemplate(project: Project): TemplateResult {
  const tech = projectTech(project, ui.statusById);
  if (tech) {
    return html`<span class="picon"><i class="${deviconClass(tech)}"></i></span>`;
  }
  return html`<span class="picon"><i class="ph ph-terminal-window"></i></span>`;
}
```

- [ ] **Step 2: Convert `moreMenu` to the kebab trigger only (popover unchanged)**

Replace the `moreMenu` function's wrapper so it no longer owns the hover-reveal (the row handles that now). Change the opening of `moreMenu` from:

```ts
  return html`
    <div class="group-actions ${open ? "menu-open" : ""}">
      <button
        class="more-btn ${open ? "active" : ""}"
```

to:

```ts
  return html`
    <div class="proj-more ${open ? "menu-open" : ""}">
      <button
        class="abtn ${open ? "active" : ""}"
```

Leave the popover menu markup (Add command / Rename project / Open in file explorer) exactly as-is.

- [ ] **Step 3: Rewrite the `projectSection` header**

Replace the `group-head` block in `projectSection` (the `<div class="group-head">...</div>`) with:

```ts
  return html`
    <section class="group">
      <div class="prow" @click=${() => toggleCollapse(project.id)}>
        <span class="pdot ${count > 0 ? "on" : ""}"></span>
        ${projectIconTemplate(project)}
        <span class="pname" title=${project.name}>${project.name}</span>
        <div class="prow-right" @click=${(e: Event) => e.stopPropagation()}>
          ${ui.showCommandCount
            ? html`<span class="pcount"><i class="ph ph-terminal-window"></i>${project.commands.length}</span>`
            : nothing}
          <div class="prow-actions">
            ${hasStartable(project)
              ? html`<button class="abtn start" title="Start all" @click=${() => startAll(project)}>
                  <i class="ph ph-play"></i>
                </button>`
              : nothing}
            ${moreMenu(project)}
          </div>
        </div>
      </div>
      ${collapsed
        ? nothing
        : visibleCmds.length === 0
          ? html`<p class="empty-cmd">No commands. Add one.</p>`
          : visibleCmds.map((c) => commandRow(project, c))}
    </section>
  `;
```

Note: this drops the `${!ui.filterRunning ? moreMenu(project) : nothing}` guard — hover actions now show in both filters (per spec decision). The `count` const at the top of `projectSection` is still used for the dot and the Running-filter early return; keep it.

- [ ] **Step 4: Gate the RAM/port cells by pref**

In `commandRow`, change the stats cells (lines ~256-262) to respect the prefs:

```ts
          <div class="stats">
            ${ui.showRam && mem != null
              ? html`<span class="cell"><span class="k">RAM</span><span class="v">${formatBytes(mem)}</span></span>`
              : nothing}
            ${ui.showPort && port != null
              ? html`<span class="cell"><span class="k">Port</span><span class="v">${port}</span></span>`
              : nothing}
          </div>
```

- [ ] **Step 5: Add the new CSS, remove the old header CSS**

In `src/views/dashboard/dashboard.css`:

Remove these now-unused rules: `.group-head`, `.group-head .titles`, `.group-chevron`, `.group-chevron.open`, `.run-count`, `.run-dot`, `.group-actions`, `.group-head:hover .group-actions`, `.group-actions.menu-open`. Keep `.more-menu` and its children (shared with the command kebab), keep `.group` and `.group h2`.

Add:

```css
/* Project row: status dot + icon + name, with a hover swap zone on the right
   (quiet at rest, reveals start-all + more-options on hover) - mirrors the
   command card's right-side swap. The whole row toggles collapse on click. */
.prow {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 9px 10px;
  border-radius: 8px;
  cursor: pointer;
  user-select: none;
}
.prow:hover {
  background: var(--surface);
}

.pdot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #33485a;
  flex: 0 0 auto;
}
.pdot.on {
  background: #46d369;
  box-shadow: 0 0 7px rgba(70, 211, 105, 0.8);
}

.picon {
  width: 24px;
  height: 24px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 19px;
  flex: 0 0 auto;
}
/* Rust has no colored Devicon variant - tint it so it's not a flat grey. */
.picon .devicon-rust-plain {
  color: #e0a884;
}
/* Phase 2 real project icons render as an <img> in this slot. */
.picon img {
  width: 22px;
  height: 22px;
  border-radius: 6px;
  object-fit: cover;
}

.pname {
  font-size: 14px;
  font-weight: 600;
  color: var(--fg);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  min-width: 0;
}

/* Right swap zone. The count sits at rest; actions overlay it on hover. */
.prow-right {
  position: relative;
  margin-left: auto;
  display: flex;
  align-items: center;
  min-height: 26px;
  flex: 0 0 auto;
}
.pcount {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 11px;
  color: var(--muted);
  opacity: 0.75;
  transition: opacity 0.12s ease;
}
.pcount i {
  font-size: 12px;
}
.prow-actions {
  position: absolute;
  right: 0;
  top: 50%;
  transform: translateY(-50%);
  display: flex;
  align-items: center;
  gap: 6px;
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.12s ease;
}
/* Reveal actions (and hide the count) on row hover, or while this project's
   kebab menu is open (so the menu anchor never vanishes). */
.prow:hover .prow-actions,
.prow:has(.proj-more.menu-open) .prow-actions {
  opacity: 1;
  pointer-events: auto;
}
.prow:hover .pcount,
.prow:has(.proj-more.menu-open) .pcount {
  opacity: 0;
}
.proj-more {
  position: relative;
  display: inline-flex;
}
/* Active (menu-open) kebab gets the accent treatment, matching the command kebab. */
.proj-more > button.active {
  border-color: var(--accent);
  color: var(--accent);
}
```

The `.abtn` button style already exists (used by command cards) and is reused here for the start-all + kebab buttons.

- [ ] **Step 6: Typecheck + build**

Run: `npx --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor" tsc --noEmit`
Run: `npm run build --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor"`
Expected: both succeed.

- [ ] **Step 7: Visual check (Joe)**

This is a native webview - not Claude-verifiable. Bring the app up via `/supervised-run` and have Joe confirm: dot color (green when a command runs), tech logos render, name truncation, hover reveals start-all + kebab, kebab menu works, count appears only when the setting is on, RAM/port hide when their toggles are off. Capture a screenshot via SendUserFile.

- [ ] **Step 8: Commit**

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add src/views/dashboard/dashboard.ts src/views/dashboard/dashboard.css
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "FEAT: structured project rows with icon, status dot, hover actions"
```

---

### Task 6: Animated aurora background

**Files:**
- Modify: `index.html` (add the layer element)
- Modify: `src/styles/base.css` (layer styles + guards)
- Modify: `src/main.ts` (pause when hidden)

- [ ] **Step 1: Add the background layer element**

In `index.html`, add immediately before `<div id="app">` (find the existing `#app` element):

```html
    <div class="app-aurora" aria-hidden="true"></div>
    <div id="app"></div>
```

- [ ] **Step 2: Style the layer + content stacking + guards**

In `src/styles/base.css`, after the `body` rule, add:

```css
/* #app sits above the fixed aurora layer; its own surfaces are opaque, so the
   aurora only shows through the gaps between rows/cards for subtle depth. */
#app {
  position: relative;
  z-index: 1;
}

/* Subtle drifting aurora: two large blurred blobs on slow loops. Pure CSS,
   GPU-cheap (transform/opacity only), paused when the window is hidden. */
.app-aurora {
  position: fixed;
  inset: 0;
  z-index: 0;
  overflow: hidden;
  pointer-events: none;
}
.app-aurora::before,
.app-aurora::after {
  content: "";
  position: absolute;
  width: 60vw;
  height: 60vw;
  border-radius: 50%;
  filter: blur(72px);
}
.app-aurora::before {
  background: var(--accent);
  opacity: 0.16;
  left: -20vw;
  top: -18vw;
  animation: aurora-drift-1 26s ease-in-out infinite;
}
.app-aurora::after {
  background: #7c3aed;
  opacity: 0.12;
  right: -22vw;
  bottom: -20vw;
  animation: aurora-drift-2 32s ease-in-out infinite;
}
@keyframes aurora-drift-1 {
  0%, 100% { transform: translate(0, 0); }
  50% { transform: translate(8vw, 7vw); }
}
@keyframes aurora-drift-2 {
  0%, 100% { transform: translate(0, 0); }
  50% { transform: translate(-7vw, -6vw); }
}
/* Pause repainting while the window is hidden to tray. */
body.bg-paused .app-aurora::before,
body.bg-paused .app-aurora::after {
  animation-play-state: paused;
}
/* Respect reduced-motion: hold the blobs still. */
@media (prefers-reduced-motion: reduce) {
  .app-aurora::before,
  .app-aurora::after {
    animation: none;
  }
}
```

- [ ] **Step 3: Pause on visibility change**

In `src/main.ts`, after the `route()` setup (after line 34 `void route();`), add:

```ts
// Pause the background animation while the window is hidden (tray) so it never
// repaints off-screen.
document.addEventListener("visibilitychange", () => {
  document.body.classList.toggle("bg-paused", document.hidden);
});
```

- [ ] **Step 4: Build**

Run: `npm run build --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor"`
Expected: succeeds.

- [ ] **Step 5: Visual check (Joe)**

Native webview - Joe confirms the aurora is subtle (not distracting), drifts slowly, cards still read clearly against it, and it's gone under reduced-motion. Screenshot via SendUserFile.

- [ ] **Step 6: Commit**

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add index.html src/styles/base.css src/main.ts
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "FEAT: subtle animated aurora background"
```

---

## PHASE 2

### Task 7: Backend project-icon resolver + IPC

**Files:**
- Create: `src-tauri/src/icons.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod icons;` + register command in `generate_handler!`)
- Modify: `src-tauri/Cargo.toml` (add `base64`)

- [ ] **Step 1: Add the base64 dependency**

Run: `cargo add base64 --manifest-path "C:/Users/tecno/Desktop/Projects/server_supervisor/src-tauri/Cargo.toml"`
Expected: `base64` added to `[dependencies]`.

- [ ] **Step 2: Write the failing resolver test**

Create `src-tauri/src/icons.rs`:

```rust
use base64::Engine;
use serde::Serialize;
use std::path::{Path, PathBuf};
use ts_rs::TS;

// Candidate icon locations under a project root, in priority order. First file
// that exists wins. Covers generic roots, web/public, Flutter, and Tauri layouts.
const CANDIDATES: &[&str] = &[
    "icon.svg", "icon.png", "icon.ico",
    "logo.svg", "logo.png",
    "app-icon.png",
    "favicon.svg", "favicon.ico", "favicon.png",
    "public/favicon.svg", "public/favicon.ico", "public/favicon.png", "public/logo.png",
    "static/favicon.svg", "static/favicon.ico", "static/favicon.png",
    "web/icons/Icon-192.png", "web/favicon.png",
    "src-tauri/icons/128x128.png", "src-tauri/icons/icon.png",
];

/// First existing icon file under `root`, or None.
pub fn find_icon_file(root: &Path) -> Option<PathBuf> {
    CANDIDATES
        .iter()
        .map(|rel| root.join(rel))
        .find(|p| p.is_file())
}

/// MIME type for a supported image extension, or None if unsupported.
pub fn mime_for(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str())?.to_ascii_lowercase().as_str() {
        "svg" => Some("image/svg+xml"),
        "png" => Some("image/png"),
        "ico" => Some("image/x-icon"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_first_candidate_by_priority() {
        let dir = std::env::temp_dir().join(format!("ss_icons_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("web/icons")).unwrap();
        // Two candidates present; favicon.png is lower priority than icon.png.
        fs::write(dir.join("web/icons/Icon-192.png"), b"x").unwrap();
        fs::write(dir.join("icon.png"), b"x").unwrap();
        let found = find_icon_file(&dir).unwrap();
        assert!(found.ends_with("icon.png"), "got {found:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn none_when_no_icon() {
        let dir = std::env::temp_dir().join(format!("ss_icons_none_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(find_icon_file(&dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mime_known_and_unknown() {
        assert_eq!(mime_for(Path::new("a/icon.svg")), Some("image/svg+xml"));
        assert_eq!(mime_for(Path::new("a/icon.PNG")), Some("image/png"));
        assert_eq!(mime_for(Path::new("a/icon.txt")), None);
    }
}
```

- [ ] **Step 3: Run the tests to confirm they pass**

First wire the module so it compiles AND is reachable from the export test: in `src-tauri/src/lib.rs` add `pub mod icons;` near the other `pub mod` declarations (the export test imports `server_supervisor_lib::settings::Settings`, so modules holding exported types must be `pub`).

Run: `cargo test --manifest-path "C:/Users/tecno/Desktop/Projects/server_supervisor/src-tauri/Cargo.toml" icons::`
Expected: 3 tests PASS.

- [ ] **Step 4: Add the Tauri command**

Append to `src-tauri/src/icons.rs` (the `use base64/serde/ts_rs` lines were already added at the top in Step 2):

```rust
const MAX_ICON_BYTES: u64 = 512 * 1024; // skip absurdly large files

// Exported to TS via tests/export_types.rs (this repo composes all TS types
// there rather than using per-type #[ts(export)] - see that file's header).
#[derive(Serialize, TS)]
pub struct ProjectIcon {
    pub mime: String,
    /// base64-encoded file bytes (no data: prefix; the frontend builds the URI).
    pub data: String,
}

/// Scan a project root for a real icon and return it as base64. None if no
/// supported icon is found (frontend then falls back to the tech logo).
#[tauri::command]
pub fn get_project_icon(root: String) -> Option<ProjectIcon> {
    let path = find_icon_file(Path::new(&root))?;
    let mime = mime_for(&path)?;
    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() > MAX_ICON_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    Some(ProjectIcon {
        mime: mime.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}
```

- [ ] **Step 5: Register the command**

In `src-tauri/src/lib.rs`, find the `tauri::generate_handler![...]` list (where `get_settings` is registered) and add `icons::get_project_icon` to it.

Use the Grep tool with pattern `generate_handler` on `src-tauri/src/lib.rs` to locate it.

- [ ] **Step 6: Register ProjectIcon for TS export**

In `src-tauri/tests/export_types.rs`, add the import and the decl line so the type lands in `ipc.generated.ts`:

```rust
use server_supervisor_lib::icons::ProjectIcon;
```

and inside `emit_ipc_types`, after the `CommandCheck` push:

```rust
    out.push_str(&decl::<CommandCheck>());
    out.push_str(&decl::<ProjectIcon>());
```

- [ ] **Step 7: Build Rust + regenerate types**

Run: `cargo test --manifest-path "C:/Users/tecno/Desktop/Projects/server_supervisor/src-tauri/Cargo.toml"`
Expected: all tests pass (incl. `emit_ipc_types`, which now emits `ProjectIcon`).
Run: `npm run gen-types --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor"`
Expected: `ipc.generated.ts` gains `export type ProjectIcon = { mime: string, data: string, };`.

- [ ] **Step 8: Commit**

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add src-tauri/src/icons.rs src-tauri/src/lib.rs src-tauri/tests/export_types.rs src-tauri/Cargo.toml src-tauri/Cargo.lock src/types/ipc.generated.ts
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "FEAT: backend project-icon scan IPC"
```

---

### Task 8: Wire project icons as tier 1 on the frontend

**Files:**
- Modify: `src/shared/ipc.ts` (wrapper)
- Modify: `src/views/dashboard/state.ts` (`iconCache`)
- Modify: `src/views/dashboard/dashboard.ts` (`projectIconTemplate` + fetch)

- [ ] **Step 1: Add the IPC wrapper**

In `src/shared/ipc.ts`, add to the imports the `ProjectIcon` type and a wrapper:

```ts
import type {
  ProcInfo, LogLine, Settings, Project, Command, DetectedCommand, CommandCheck, ProjectIcon,
} from "../types/ipc.generated";
```

```ts
export const getProjectIcon = (root: string) =>
  invoke<ProjectIcon | null>("get_project_icon", { root });
```

- [ ] **Step 2: Add the icon cache to `ui`**

In `src/views/dashboard/state.ts`, add to the `ui` object:

```ts
  showPort: true,
  // Per-project resolved icon data URI. undefined = not fetched, null = none
  // found (use tech-logo fallback), string = ready-to-render data URI.
  iconCache: {} as Record<string, string | null | undefined>,
```

- [ ] **Step 3: Fetch + render the project icon as tier 1**

In `src/views/dashboard/dashboard.ts`, replace `projectIconTemplate` with the tiered version:

```ts
// Kick off a one-time icon fetch for a project, caching the result. Redraws when
// it resolves so the <img> appears. No-op if already fetched/pending.
function ensureProjectIcon(project: Project) {
  if (project.id in ui.iconCache) return;
  ui.iconCache[project.id] = undefined; // mark pending (key now present)
  void ipc
    .getProjectIcon(project.root)
    .then((icon) => {
      ui.iconCache[project.id] = icon ? `data:${icon.mime};base64,${icon.data}` : null;
      draw();
    })
    .catch(() => {
      ui.iconCache[project.id] = null;
      draw();
    });
}

// Icon slot: real project icon (tier 1) -> tech logo (tier 2) -> generic (tier 3).
function projectIconTemplate(project: Project): TemplateResult {
  ensureProjectIcon(project);
  const cached = ui.iconCache[project.id];
  if (typeof cached === "string") {
    // onerror falls back to tech logo if the bytes fail to decode.
    return html`<span class="picon"
      ><img
        src=${cached}
        alt=""
        @error=${() => {
          ui.iconCache[project.id] = null;
          draw();
        }}
    /></span>`;
  }
  const tech = projectTech(project, ui.statusById);
  if (tech) {
    return html`<span class="picon"><i class="${deviconClass(tech)}"></i></span>`;
  }
  return html`<span class="picon"><i class="ph ph-terminal-window"></i></span>`;
}
```

Note: `in ui.iconCache` keys on presence, and we set `undefined` to mark pending — so `if (project.id in ui.iconCache) return;` prevents refetching on every 2.5s poll.

- [ ] **Step 4: Typecheck + build**

Run: `npx --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor" tsc --noEmit`
Run: `npm run build --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor"`
Expected: both succeed.

- [ ] **Step 5: Visual check (Joe)**

Native webview. Joe confirms projects with a real icon (e.g. zng-app) show it, projects without fall back to the tech logo, and nothing flickers each poll. Screenshot via SendUserFile.

- [ ] **Step 6: Commit**

```bash
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" add src/shared/ipc.ts src/views/dashboard/state.ts src/views/dashboard/dashboard.ts
git -C "C:/Users/tecno/Desktop/Projects/server_supervisor" commit -m "FEAT: render detected project icons as the primary row icon"
```

---

## Final verification

- [ ] `cargo test --manifest-path "C:/Users/tecno/Desktop/Projects/server_supervisor/src-tauri/Cargo.toml"` — all pass.
- [ ] `npx --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor" tsc --noEmit` — clean.
- [ ] `npm run build --prefix "C:/Users/tecno/Desktop/Projects/server_supervisor"` — succeeds.
- [ ] Joe's visual pass on the full revamp (rows, icons, dot, hover actions, aurora, settings toggles live).
