# Auto-update for server_supervisor + reusable release workflow

Date: 2026-06-02
Status: approved design, pending spec review

## Goal

Give server_supervisor in-app auto-update (on-startup check + a manual "Check
for updates" button), matching how the sibling Tauri apps (pomodoro-overlay,
claude_usage_in_taskbar) already update. In the same effort, extract the
near-identical release workflow those apps copy-paste into a single reusable
`workflow_call` workflow hosted in the kit, with server_supervisor as its first
consumer.

## Background

- The vendored kit (`vendor/tauri_kit`, repo `sirbepy_tauri_kit`) already holds
  the reusable update *logic*: the Rust plugin wrapper (`tauri_kit_updater::plugin()`),
  the frontend `frontend/updater/check.ts` (manual check + prompt) and
  `frontend/updater/auto-check.ts` (on-startup check reading `__kit_auto_update`),
  and the About page's "Check for updates" button wired inside `renderSettingsPage`.
- server_supervisor currently **stubs** `@tauri-apps/plugin-updater` to a no-op
  (`vite.config.ts` alias -> `src/vendor-stubs/plugin-updater.ts` whose `check()`
  returns `null`), has no Rust updater plugin, and its `tauri-release.yml`
  produces unsigned installers with no `latest.json`. So the About "check" does
  nothing today.
- Each app has its **own** signing keypair (claude_usage and pomodoro pubkeys
  differ). A shared pubkey would mean a shared private signing key, which we do
  not want. server_supervisor needs a new keypair.

## Scope

In scope:
- A reusable Windows release+updater workflow in `sirbepy_tauri_kit`.
- server_supervisor consuming it (thin caller) and the per-app updater wiring.

Out of scope (deliberately):
- Migrating pomodoro-overlay / claude_usage onto the reusable workflow. Their
  pipelines work; pomodoro's has app-specific gates (OpenSSL-leak assertion,
  push-crypto check) a generic workflow would drop, and claude_usage is multi-OS
  with a bespoke updater. Leave them be; migrate later only if clearly worth it.
- Multi-OS builds. server_supervisor is Windows-only (its backend is
  Windows-specific), so the reusable workflow targets Windows (msi + nsis).

## Design

### 1. Reusable workflow (in `sirbepy_tauri_kit`)

New file `sirbepy_tauri_kit/.github/workflows/tauri-windows-release.yml`,
`on: workflow_call`. Body is the existing check -> tag -> build -> publish
pipeline, generalized:

- Inputs:
  - `asset-name` (required, string) - base name for release assets, e.g.
    `Server-Supervisor`.
  - `tag-prefix` (optional, default `v`) - git tag prefix.
  - `sign` (optional, default `true`) - gates the signing env, `.sig` capture,
    per-platform json, and `latest.json` generation. `false` reproduces the old
    unsigned-installers-only behavior.
- Secrets: `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
  (both optional; required only when `sign: true`). Flow in via `secrets: inherit`.
- The release-download URL is built from `${{ github.repository }}`, which in a
  reusable workflow resolves to the **caller's** repo, so it is per-app with no
  input. Likewise `actions/checkout` checks out the caller's repo.
- Build uses `cargo-tauri build` (the tauri-cli binary, to avoid the rustup shim
  shadowing `cargo tauri`), `submodules: recursive` (the kit is a submodule and
  the crate depends on it), version sync into tauri.conf.json + Cargo.toml +
  package.json, NSIS installer as the updater payload, MSI as an extra.
- Action versions pinned to currently-real majors (checkout@v4, setup-node@v4,
  upload-artifact@v4, download-artifact@v4, gh-release@v2) - server_supervisor's
  current file references non-existent majors (v6/v7/v8) and is replaced wholesale.
- Callers reference it as
  `uses: SirBepy/sirbepy_tauri_kit/.github/workflows/tauri-windows-release.yml@main`.
  Tradeoff: `@main` means a kit-workflow change takes effect immediately for all
  callers (convenient, but a bad change could break a release). Acceptable for a
  one-consumer start; pin to a tag later if the consumer set grows.

### 2. server_supervisor per-app wiring

- `src-tauri/Cargo.toml`: add `tauri-plugin-updater` (align with the kit crate's
  `2.0` line) and `tauri_kit_updater = { path = "../vendor/tauri_kit/tauri/updater" }`.
  `tauri-plugin-dialog` is already present.
- `src-tauri/src/lib.rs`: add `.plugin(tauri_kit_updater::plugin())` to the
  builder.
- `src-tauri/tauri.conf.json`: `bundle.createUpdaterArtifacts: true`; add
  `plugins.updater` with `active: true`, `endpoints:
  ["https://github.com/SirBepy/server_supervisor/releases/latest/download/latest.json"]`,
  the new `pubkey`, and `dialog: false` (the kit frontend drives the prompt).
- `package.json`: add real `@tauri-apps/plugin-updater` (^2.9). `@tauri-apps/plugin-dialog`
  is already a dependency.
- Remove the stub: delete the `@tauri-apps/plugin-updater` alias in
  `vite.config.ts`, delete `src/vendor-stubs/plugin-updater.ts`, and drop its
  declaration from `src/types/vendor-stubs.d.ts`.
- `src/main.ts`: `if (!import.meta.env.DEV) runAutoUpdateCheck();` (imported from
  the kit's `frontend/updater/auto-check`). The DEV guard avoids a dev binary
  falsely "finding" an update. The About "check now" button works for free once
  the real plugin is present.
- `.github/workflows/`: replace `tauri-release.yml` with the thin caller
  `release.yml` shown above (`asset-name: Server-Supervisor`, `secrets: inherit`).

### 3. Auto-update behavior + the server-restart caveat

- Mode comes from the kit's `__kit_auto_update` setting (`never` / `onStartup` /
  `immediate`), changeable on the About page; default `onStartup`. `onStartup`
  checks once at app launch and, if an update exists, downloads + installs +
  relaunches.
- Caveat unique to this app: installing an update restarts the process, and on
  exit the supervisor tree-kills every child it owns (same as Quit). Because the
  check only fires **on startup** (never mid-session, since the app lives in the
  tray), the blast radius is minimal: at a fresh launch the user has not started
  a work session yet, and autostart commands simply relaunch on the new version.
  Documented for the user; not a blocker.

## Signing key (Joe's manual steps)

1. Generate a per-app keypair:
   `npx @tauri-apps/cli signer generate -w "$env:USERPROFILE\.tauri\server_supervisor.key" --password "" -f`
   (empty password for simplicity). Claude reads the `.pub` file and wires the
   pubkey into `tauri.conf.json`; never reads/prints the private key.
2. Before the first release, add two repo secrets on GitHub:
   `TAURI_SIGNING_PRIVATE_KEY` = contents of `server_supervisor.key`,
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` = empty.

## Verification

- Local floor: `cargo test`, `tsc --noEmit`, `vite build` (the stub removal +
  real plugin must still typecheck and build).
- Workflow YAML: parse-check both files; reason through the job graph. A release
  workflow cannot be truly exercised without cutting a release.
- Real proof (tomorrow): a `pushnbump` to v0.1.2 cuts a signed release whose
  `latest.json` resolves at the endpoint, and an older installed build offers the
  update. This is the actual acceptance test and is inherently post-merge.

## Risks

- Release-pipeline blast radius: mitigated by making server_supervisor the sole
  consumer; siblings stay on their proven workflows.
- `@main` reusable ref: a kit-workflow regression would affect the next release;
  acceptable at one consumer, revisit if more apps adopt it.
- Restart-kills-servers on update: low risk because the check is startup-only
  (see above).
