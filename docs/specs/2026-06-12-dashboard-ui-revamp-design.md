# Dashboard UI revamp - design

Date: 2026-06-12
Status: approved (visual companion brainstorm), ready for planning

## Problem

The dashboard's command card was recently reworked and looks good, but everything
around it does not. A "project" today renders as a bare `group-head`: a tiny
chevron plus the project name as plain text, floating in space with no container,
icon, or metadata. Most visible rows are these naked headers, so the app reads as
a half-loaded list rather than a dashboard. Stopped and running states look like
two different apps, and the near-flat background gives the one polished component
(the command card) nothing to sit against.

The fix is to give every project row the same level of structure the command card
already has, add per-project identity via icons, and add subtle depth behind it -
without touching the command card itself.

## Goals

- Replace the bare project header with a structured project row: status dot, an
  icon, the name, and hover-revealed actions.
- Give each project a meaningful icon via a resolution chain.
- Add a subtle, cheap, animated background for depth.
- Keep the shipped command card exactly as-is.
- Add user settings to tune density (command count, RAM, port).

## Non-goals

- No change to command-card markup, styling, or behavior.
- No change to the poll loop, IPC surface for processes, or the start/stop model.
- No change to the topbar or All/Running filter behavior (only background bleed).

## Locked design decisions

These were settled during the visual-companion brainstorm:

1. **Row direction:** the "polished list" direction (not full cards per project,
   not nested panels). A project is a light list row; its command cards remain the
   only heavy card element, nested beneath when expanded.
2. **Icon strategy:** resolution chain **project icon -> tech logo -> generic
   glyph**. Tech logos use **Devicon** (a logo webfont, used exactly like
   Phosphor) - an explicit, scoped exception to the Phosphor-only rule for this one
   icon slot. Phosphor remains for all other UI chrome.
3. **Status indicator:** a leading **status dot** (glowing green when the project
   has >= 1 running command, dim otherwise). Not a left-edge accent.
4. **Command count:** **hidden by default**, exposed via a settings toggle (off by
   default).
5. **RAM / port stats:** stay **on by default**, each exposed via its **own**
   settings toggle (two separate switches).
6. **Hover actions:** the row's right side is a swap zone like the command card -
   empty at rest, revealing a start-all play button and a more-options kebab on
   hover.
7. **Background:** keep a **subtle drifting aurora** (CSS only, GPU-cheap, paused
   when hidden / reduced-motion).

## Components

### 1. Project row (replaces `group-head` in `projectSection`)

Current `projectSection` renders a `group-head` (chevron + `<h2>` name +
`run-count` pill + hover `moreMenu`) followed by command cards. The redesign
reshapes the header; the command-card list below is unchanged.

New row, left to right:

- **Status dot** - `.dot`, 7px. Green + soft glow when `runningCount(project) > 0`,
  dim (`#33485a`) otherwise. Replaces the `run-count` pill as the status signal.
- **Project icon** - 24px slot (see Icon system).
- **Name** - the existing `project.name`, ellipsis-truncated.
- **Right swap zone** - empty at rest; on row hover reveals:
  - **Start-all** play button (`ph-play`, green) - starts every non-running
    command in the project. Hidden when all commands are already running.
  - **More-options kebab** (`ph-dots-three-vertical`) - opens the existing
    `moreMenu` (Add command / Rename project / Open in file explorer). No new menu
    content; only the trigger moves into the swap zone.

Interaction:

- The row keeps **click-to-collapse** (`toggleCollapse`), `cursor: pointer`, with a
  hover background as the affordance. The standalone caret is dropped (the approved
  mockups read clean without it; presence/absence of the nested cards is the cue).
- Action buttons `stopPropagation` so clicking them never toggles collapse - the
  same pattern the command card's `.controls` already use.
- The `run-count` pill is removed. When the Running filter hides the kebab today
  (`!ui.filterRunning ? moreMenu : nothing`), keep that behavior: hover actions
  still suppressed under the Running filter is acceptable, but prefer showing them
  in both filters for consistency (decided: show in both).

### 2. Icon system

A small frontend helper resolves a project's icon in three tiers:

1. **Project icon (Phase 2):** a new backend IPC command scans the project's
   `root` for a real icon file and returns its bytes as a base64 data URI (+ mime).
   Frontend renders `<img>`; `onerror` falls through to tier 2. Search order
   (first match wins):
   - root: `icon.{svg,png,ico}`, `logo.{svg,png}`, `app-icon.png`,
     `favicon.{svg,ico,png}`
   - web: `public/favicon.{svg,ico,png}`, `public/logo.png`, `static/favicon.*`
   - flutter: `web/icons/Icon-192.png`, `web/favicon.png`, `assets/icon/*`
   - tauri: `src-tauri/icons/128x128.png`, `src-tauri/icons/icon.png`
   Result cached per project id (icons rarely change); refreshed on project reload.
2. **Tech logo:** derived by parsing the project's primary command's program token
   (mirrors `deriveName`): `cargo`/`rustc` -> rust, `flutter` -> flutter,
   `npm`/`pnpm`/`yarn`/`bun`/`npx`/`node` -> nodejs, plus `python`, `go`, `deno`,
   `docker`, `dotnet`. Rendered as a Devicon glyph (`devicon-<name>-plain`,
   `colored` where a colored variant exists; rust has none, so it is tinted in
   CSS). "Primary command" = the running command if any, else the first command.
3. **Generic glyph:** a Phosphor `ph-terminal-window` when nothing else resolves.

Phase 1 ships tiers 2 and 3 (frontend only). Phase 2 adds tier 1 (the backend
scan). Tier 1 is purely additive - it slots above the existing chain.

### 3. Command card

Unchanged. `commandRow`, `cmdMenu`, `.card` styling, the log drawer, RAM/port
cells, and hover controls all stay exactly as shipped. The only interaction with
settings is gating the RAM/port cells (see Settings).

### 4. Animated background

A CSS-only aurora: two large blurred radial-gradient blobs (glacier blue + a faint
violet) drifting on slow (26s / 32s) ease-in-out loops, ~0.2-0.26 opacity, as a
fixed layer behind the app content. Surfaces (header, cards) stay opaque enough for
legibility; the list gaps let the aurora show through for depth.

Performance guards (this is an always-on tray app):

- Pure `transform`/`opacity` animation on 2 elements - no canvas, no JS RAF.
- `@media (prefers-reduced-motion: reduce)` removes the animation (static gradient).
- Pause via `animation-play-state: paused` toggled on `document.visibilitychange`
  so it never repaints while the window is hidden to tray.

### 5. Settings additions

Three new boolean settings. Each requires: a field on the Rust `Settings` struct
(serde default), a `toggle` entry in `schema.ts`, and a read in the dashboard.

| key                   | label                  | default | effect                                  |
|-----------------------|------------------------|---------|-----------------------------------------|
| `show_command_count`  | Show command count     | false   | shows a quiet count on each project row |
| `show_ram`            | Show RAM               | true    | gates the RAM cell in the command card  |
| `show_port`           | Show port              | true    | gates the Port cell in the command card |

A new **"Dashboard"** section in `schema.ts` holds these three toggles.

Data flow: the dashboard does not read settings today. It will fetch these three
values via `get_settings` on mount and re-read on `hashchange` back to the
dashboard (settings change rarely; no need to poll). Values are cached in the
dashboard `ui` state and gate rendering. Backend default values mean a fresh
install behaves exactly as the locked defaults above.

When `show_command_count` is on, the count renders as a quiet, borderless number
(`ph-terminal-window` + N) in the row's rest state, hidden on hover behind the
actions - never the noisy bordered pill from the rejected mockup.

## Dependencies

- **Devicon** webfont, added as a pinned npm dependency (`devicon`) and imported
  for offline use (the app must work without network; no CDN). Subject to the
  standard package safety check before adding (well-known, widely used; verify
  the pinned version against advisories).

## Phasing

- **Phase 1** (frontend-only, ships the visible revamp): project row, status dot,
  tech-logo + generic icon tiers, hover actions, aurora background, the three
  settings (incl. the Rust `Settings` fields + schema). This is the bulk of the
  user-visible change and needs no new IPC.
- **Phase 2** (additive): backend project-icon scan IPC + frontend wiring as tier 1
  of the chain. Can land in a follow-up without reworking Phase 1.

## Testing & verification

- `cargo test`, `tsc --noEmit`, `vite build` (the project's verification floor).
- New backend logic (Phase 2 icon scan; Phase 1 Settings defaults) gets Rust unit
  tests where pure (e.g. the icon-path resolver given a fake dir tree; serde
  defaults round-trip).
- Frontend icon-resolution helper (program token -> tech) gets unit tests like the
  existing `deriveName` coverage.
- Visual/interaction correctness (dot color, hover reveal, aurora feel, icon
  rendering) is **not** Claude-verifiable in this native webview - it needs Joe's
  eyes. State that explicitly rather than claiming it verified.

## Open questions

None blocking. Minor follow-ups deferred to Phase 2 (icon size normalization for
non-square source images; whether to also surface the generic glyph's tech via
tooltip).
