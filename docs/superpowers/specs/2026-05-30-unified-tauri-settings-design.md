# Unified Tauri Settings via Vendored Styleguide — Design (Phase 1)

Date: 2026-05-30
Status: Approved for spec; pending implementation plan
Scope: Phase 1 only — reconcile + redesign the shared kit, prove it on `server_supervisor`. Rollout to `pomodoro-overlay` and `claude_usage_in_taskbar` is deferred to later phases (tracked as todos).

## Goal

One unified settings look (and shared visual language) across all of Joe's Tauri apps — present and future — driven by a single source of truth. The reference for "good" is the bespoke settings screen in `claude_usage_in_taskbar`. The mechanism is the shared `sirbepy_tauri_kit` settings system, restyled onto the existing web design system (`bepy_styleguide`).

## Key insight

A Tauri webview is Chromium. The `bepy_styleguide` is plain CSS (tokens + components) already used across web projects. Therefore the styleguide can be reused **directly** inside Tauri apps — we do not need a separate native design system. "Unify" = make the kit and apps consume the styleguide instead of each re-inventing tokens and components.

## Current state (why this is needed)

Investigation (2026-05-30) found maximal divergence:

- **`bepy_styleguide`** — canonical design system. Defines 4 palettes (Void / Nebula / Glacier / Cosmo), each with dark + light variants, plus tokens (`--color-*`, `--font-*`, `--radius-*`, `--shadow-*`) and **general** components (`.btn`, `.card`, `.input`, `.badge`, `.alert`, `.tooltip`, `.swatch`, `.stat`, `.progress-bar`, grid + text utilities). Distributed via jsDelivr CDN. It does **not** contain settings-specific widgets.
- **`claude_usage_in_taskbar`** — the visual reference, but fully **bespoke**: imports neither the kit nor the styleguide. It copied only the palette color *values* into its own `themes.css` and hand-rolled its own `widgets.css` (where the settings widgets `.section`, `.option`, `.nav-row`, `.switch`, `.theme-card`, info tooltip live). Token names `--bg / --surface / --primary / --text-dim`. Flat settings structure; About is top-level; a "Themes" subview offers a dark/light toggle + 4 palette cards.
- **`sirbepy_tauri_kit`** — the shared settings system, but vendored at **three different commit lineages**:
  - canonical `main` (`34d1d19`): modes only (`light|dark|system`), **no palettes**, `--kit-*` tokens, About reached through System.
  - the commit pomodoro vendored (`7f55b16`): **has** a palette system (`frontend/settings/palettes/sirbepy-default.{css,ts}`) — but this lineage was never merged back to canonical `main`.
  - what `server_supervisor` was on previously (`4b1d4a5` lineage): had a hardcoded section-categories bug (fixed 2026-05-30, commit `d119fc0`, which added an optional `category` field and made `rootPage` schema-driven).
- Three token vocabularies coexist: styleguide `--color-*`, claude_usage `--bg`, kit `--kit-*`.

Net: the palette work and the reference look both partially exist, but nothing is unified, and the kit is forked.

## Decisions (locked with Joe)

1. **Source of truth:** `bepy_styleguide` owns tokens + palettes + general components.
2. **Delivery:** vendor a **synced copy** of the styleguide CSS into the kit (offline/native-safe, no runtime CDN). Apps get it transitively through the kit. A small sync script keeps the copy current. No nested submodules.
3. **Settings widgets home:** the **kit** owns settings-specific widgets (nav-row, option row, toggle switch, palette card), restyled to consume styleguide tokens/classes. The styleguide stays general.
4. **Nav structure:** **flat, grouped** like claude_usage — category headers with nav rows; **About and System are top-level rows** at the bottom (About no longer nested under System).
5. **Color themes:** add the 4 palettes (dark/light each) to the settings, presented as a dark/light toggle + palette cards (claude_usage style).
6. **Scope/sequencing:** phase it. Phase 1 = kit reconcile + redesign, proven on `server_supervisor`. pomodoro + claude_usage = later todos.
7. **claude_usage end state:** leave bespoke for now; mine as reference. Possible later migration once the kit visibly matches it.
8. **server_supervisor token scope:** align the **whole app** (dashboard + settings) to styleguide `--color-*`, so the dashboard and settings share one palette and theme switching restyles everything.

## Ownership architecture

```
bepy_styleguide   → tokens (--color-*, --font-*, --radius-*, --shadow-*)
                    + 4 palettes (Void/Nebula/Glacier/Cosmo, dark+light)
                    + general components (.btn .card .input .tooltip …)
      │  vendored copy, refreshed by a sync script (no runtime CDN)
      ▼
sirbepy_tauri_kit → settings-specific widgets (nav-row, option, switch, palette-card)
                    built on styleguide tokens
                    + settings assembly (PageStack, schema renderer, fields)
      │  git submodule
      ▼
apps (server_supervisor, pomodoro, …)
                  → consume the kit; inherit the styleguide transitively
                  → align their own app tokens to --color-* for a unified look
```

For each unit: the styleguide answers "what are the brand colors/components"; the kit answers "how is a settings screen assembled and what do its rows look like"; an app answers "what settings does this app expose and how is the rest of the app themed."

## Work breakdown

### Step 0 — Reconcile the kit (precondition, highest risk)

Establish a single canonical `sirbepy_tauri_kit` `main` that contains, in one lineage:
- the palette system currently stranded in `7f55b16` (`palettes/sirbepy-default.{css,ts}` + the `palettes` / `theme.defaultPalette` render options),
- the `rootPage` schema-driven fix + optional `category` field (kit commit `d119fc0`),
- everything else from canonical `main`.

Output: a known-good kit commit on `main` that every app can re-point to. This step is mostly git reconciliation + verifying the merged result builds and its tests pass. Treat as its own discrete chunk before any redesign; do not start Step 2 until this is green.

Risk: the three lineages may have conflicting edits to the same files (`renderer.ts`, `root.ts`, `styles/*`). Reconciliation may require manual merge and re-testing. Budget for it explicitly.

### Step 1 — Vendor the styleguide into the kit

- Copy the styleguide's token + palette + general-component CSS into `frontend/styleguide/` inside the kit (exact file split TBD in the plan; mirror the styleguide's own partition).
- Add a sync script (e.g. `scripts/sync-styleguide.*`) that refreshes the vendored copy from the styleguide repo/CDN source, so updates are a one-command pull, not hand edits.
- Standardize the token vocabulary on the styleguide's `--color-*` (+ `--font-*`, `--radius-*`, `--shadow-*`). Retire `--kit-*` and ad-hoc `--bg` names (provide temporary aliases only if needed to stage the migration).

### Step 2 — Redesign the kit settings to the claude_usage look

- Restyle the settings widgets onto styleguide tokens, matching claude_usage's spacing, card treatment, and typography:
  - section card (`.section` + uppercase dimmed `.section-title`),
  - nav row (label + chevron, hover highlights accent),
  - option row (label + control, 44px min-height, separators),
  - toggle switch (iOS-style),
  - info tooltip (hover `(i)`),
  - buttons (primary/secondary/danger) — prefer the styleguide's `.btn*` where they fit.
- **Flat grouped root**: render schema sections under category headers as nav rows; append **About** and **System** as top-level nav rows at the bottom. (Builds on the already schema-driven `rootPage`; `category` becomes the group header.)
- **Appearance/Themes page**: dark/light mode toggle + 4 palette cards with color swatches; selecting applies immediately and persists. Palette + mode stored in kit settings keys (e.g. `__kit_palette`, `__kit_mode`); `applyTheme` sets `data-theme` (palette) and `data-mode` (dark/light) on `<html>`, matching the styleguide's `[data-theme][data-mode]` selector convention.
- **About** becomes a top-level page reached from the root (not via System).

### Step 3 — Prove on server_supervisor (the unified-look target)

- Re-point `server_supervisor`'s `vendor/tauri_kit` submodule to the reconciled kit commit.
- Align server_supervisor's **own** `src/styles/tokens.css` (and any component CSS referencing `--bg/--surface/--fg/--accent/--border`) to the styleguide `--color-*` vocabulary, so dashboard + settings share the palette and theme switching restyles the whole app.
- Ensure palette/mode persistence round-trips through the Rust `Settings` (verify `get_settings`/`save_settings` tolerate the kit's theme keys; extend the struct if it rejects unknown fields).
- Verify: `tsc --noEmit`, `vite build`, Playwright browser-drive of dashboard + settings + theme switching, then Joe's visual QA in the real Tauri app.

### Step 4 — Rollout (later phases, todos only in Phase 1)

- Write `ai_todos` for `pomodoro-overlay` and `claude_usage_in_taskbar` to adopt the reconciled kit + styleguide tokens.
- pomodoro: re-point submodule, drop its `sirbepy-default` duplication in favor of the vendored styleguide, align app tokens.
- claude_usage: optional later migration onto the kit once the kit visibly matches it; until then it stays the reference.

## Testing & verification

- Fast floor every step: `tsc --noEmit` (app + kit where applicable), `vite build`, kit unit tests (`vitest`) for changed kit logic (e.g. `root.test.ts`).
- UI/layout/nav/theming: Playwright against the Vite dev server (port 6970). The frontend is browser-driveable; only Tauri IPC (`invoke`/`listen`) throws in a plain browser and is expected-and-ignored.
- Real-app confirmation (IPC persistence of palette/mode, native webview): Joe's eyes — explicitly out of Claude's automated reach.

## Risks & open items

- **Kit reconciliation (Step 0)** is the main risk — three diverged lineages, possible manual merges. Must be green before redesign.
- **Token migration** touches every kit settings stylesheet and server_supervisor's app CSS. Stage with aliases if a big-bang rename is risky.
- **Palette persistence** depends on the Rust `Settings` struct tolerating kit theme keys; may need a struct change or a `#[serde(flatten)]`/extra-keys strategy.
- **Styleguide sync mechanism** (script vs manual) — exact form decided in the implementation plan.
- The styleguide currently lacks settings widgets; per decision 3 they live in the kit. If a future web project wants them, revisit promoting them into the styleguide.

## Out of scope (Phase 1)

- Migrating claude_usage onto the kit.
- Implementing pomodoro's adoption (todos only).
- Any non-settings redesign beyond the token alignment needed for a unified palette in server_supervisor.
