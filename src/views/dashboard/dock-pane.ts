// Docked-window pane: one collapsible Project-screen section per project,
// holding one pane per command that opted into `dock_window` (see
// modals.ts's "Dock window in dashboard" checkbox). Only ever rendered for
// those commands - most supervised processes are headless dev servers with
// no window at all, so this section doesn't exist unless at least one
// command in the project asked for it.
//
// THE PART THAT MAKES THIS UNUSUAL: when a command is actually docked, its
// window is a REAL NATIVE WINDOW the Rust backend positions on top of the
// webview (see supervisor::dock), not something this module draws. The pane
// in the "docked" states below is a RESERVED HOLE - an empty, precisely
// measured rect - and anything rendered inside it is invisible while docked.
// Only the not-docked/window-lost/exited states show real content.
//
// State this module owns is kept OUTSIDE `ui` (state.ts) and the Modal union
// on purpose: this file was written under a dispatch whose file allowlist
// covered this module but not state.ts, so everything needed - dock state
// per command, cached log tails, the window's screen geometry, in-flight
// dock attempts - lives in module-level maps here instead. Project-screen.ts
// only imports `dockSection` and mounts it.

import { html, nothing, type TemplateResult } from "lit-html";
import { ref } from "lit-html/directives/ref.js";
import "./dock-pane.css";
import * as ipc from "../../shared/ipc";
import type { Command, DockOutcome, DockRect, DockState, Project } from "../../types/ipc.generated";
import { ui, act, draw } from "./state";
import { toggleSetMember } from "./helpers";
import { renderAnsi } from "../../shared/ansi";
import { computeRect, ensureGeometryListeners, isGeometryReady, refreshWindowGeometry } from "./dock-pane-geometry";

// ----- per-command bookkeeping (all keyed by the composite "project:command" id) -----

const dockStateCache: Record<string, DockState> = {};
// Frozen once the process stops - see the polling loop below, which only
// re-fetches while running.
const logCache: Record<string, string> = {};
// Last status seen per id, so the poll loop can detect a stopped/crashed ->
// running edge (re-arms auto-dock attempts) and a running -> stopped edge
// (one last log fetch to catch the final lines before the tail freezes).
const lastStatus: Record<string, string> = {};
// Set by the pane's own "Undock" button. Suppresses auto-redock for THIS
// process instance only - cleared on the next stopped/crashed -> running
// transition, so a restart always gets a fresh attempt.
const manualUndock = new Set<string>();
const dockAttemptInFlight = new Set<string>();
// Project screen's "Docked windows" section is collapsed by default (matches
// envSectionOpen/hubLogOpen elsewhere in this project); absent = collapsed.
const sectionOpen = new Set<string>();
// Each tracked pane's last-measured rect, in the screen-physical-pixel space
// the backend expects (see reportRect below). Filled in by the ResizeObserver
// the first time the element paints; used both to re-assert an existing dock
// and to attempt a new one.
const lastRect: Record<string, DockRect> = {};
// Disposes the ResizeObserver + scroll listener a previous element instance
// got via attachPaneObservers below. Without this, a project-screen
// remount (e.g. leaving and returning to the project, or the section
// collapsing and reopening) would leave a listener bound to a detached
// element, which reports a zero rect and corrupts the live dock position.
const paneCleanup: Record<string, () => void> = {};
// Commands set to run headless. The backend docks those into its invisible
// host on its own, so this pane must never auto-dock them into the hole or
// re-assert a hole rect onto them; it only shows a screenshot preview.
const headlessIds = new Set<string>();
const previewCache: Record<string, string> = {};

function isPaneDocked(id: string): boolean {
  const s = dockStateCache[id];
  return s?.state === "docked" && s.mode !== "headless";
}

// ----- rect measurement + reporting -----
// Host-window geometry (screen origin, scale factor, ResizeObserver-tick
// refresh) lives in dock-pane-geometry.ts; this file only calls its exports,
// so that module state has exactly one owner.

// Re-sends the current rect for every id that's actually docked right now
// (called on host window resize/move, since that moves every open hole at
// once). Pane-local resize/scroll is handled per-id by the ResizeObserver/
// scroll listener set up in attachPaneObservers instead.
function reassertAllDocked() {
  for (const [id, rect] of Object.entries(lastRect)) {
    if (isPaneDocked(id)) {
      void ipc.setProcDockBounds(id, rect).catch(() => {});
    }
  }
}

function reportRect(id: string, el: HTMLElement) {
  const rect = computeRect(el);
  lastRect[id] = rect;
  if (isPaneDocked(id)) {
    void ipc.setProcDockBounds(id, rect).catch(() => {});
  }
}

// Attached via the `ref` directive below - lit-html invokes a ref callback
// with the element on mount and with `undefined` on disconnect (including a
// disconnect immediately followed by mounting a fresh element instance, e.g.
// a project-screen remount), so both branches below matter: skipping the
// teardown branch would leave a ResizeObserver/scroll listener bound to a
// detached element, which reports a zero rect and corrupts the live dock
// position for `id`.
function attachPaneObservers(id: string, el: HTMLElement | undefined) {
  paneCleanup[id]?.();
  delete paneCleanup[id];
  if (!el) return;
  ensureGeometryListeners(reassertAllDocked);

  const setup = () => {
    reportRect(id, el);
    const ro = new ResizeObserver(() => reportRect(id, el));
    ro.observe(el);
    const onScroll = () => reportRect(id, el);
    // The app has no inner scroll container (body/document scrolls natively -
    // see base.css, which sets height:100% with no overflow rule) - the
    // window is "whatever actually scrolls" here.
    window.addEventListener("scroll", onScroll, { passive: true });
    paneCleanup[id] = () => {
      ro.disconnect();
      window.removeEventListener("scroll", onScroll);
    };
  };
  // Defer the first rect (and the observers that keep it live) until the
  // window's screen origin/scale have resolved at least once - otherwise the
  // very first reported rect would use the winOriginX/Y=0 placeholder and
  // hand the backend a wrong-corner dock target on a fresh app launch.
  if (isGeometryReady()) setup();
  else void refreshWindowGeometry().then(setup);
}

// ----- polling: dock state, auto-dock attempts, live log tail -----

const POLL_MS = 2000;
let pollStarted = false;
// The interval callback reads through this reference rather than closing
// over the `getTrackedIds` passed to whichever `ensurePolling` call started
// it - `dockSection` runs on every render (once per project the dashboard
// shows), and only the first call's closure would otherwise ever run,
// freezing the poll on whichever project happened to render first.
let currentGetTrackedIds: () => string[] = () => [];

function ensurePolling(getTrackedIds: () => string[]) {
  currentGetTrackedIds = getTrackedIds;
  if (pollStarted) return;
  pollStarted = true;
  window.setInterval(() => {
    for (const id of currentGetTrackedIds()) pollOne(id);
  }, POLL_MS);
}

function pollOne(id: string) {
  const status = ui.statusById[id]?.status ?? "stopped";
  const running = status === "running" || status === "starting";
  const wasRunning = lastStatus[id] === "running" || lastStatus[id] === "starting";
  if (running && !wasRunning) manualUndock.delete(id); // fresh start: allow auto-dock again
  if (!running && wasRunning) void fetchLogs(id); // last catch-up fetch before the tail freezes
  lastStatus[id] = status;

  if (!running) {
    delete dockStateCache[id];
    return;
  }

  void ipc
    .getDockState(id)
    .then((s) => {
      dockStateCache[id] = s;
      draw();
    })
    .catch(() => {});

  const state = dockStateCache[id];
  const notDocked = !state || state.state === "not_docked";
  if (notDocked) void fetchLogs(id);
  if (state?.state === "docked" && state.mode === "headless") void fetchPreview(id);

  const headless = headlessIds.has(id);
  if (notDocked && !headless && !manualUndock.has(id) && !dockAttemptInFlight.has(id) && lastRect[id]) {
    dockAttemptInFlight.add(id);
    void ipc
      .dockProcWindow(id, lastRect[id])
      .then((outcome) => {
        dockStateCache[id] = { state: "docked", mode: outcome };
      })
      .catch(() => {
        // No window found within the backend's own timeout yet (a slow
        // Flutter/Tauri build) - stays not_docked, retried next tick.
      })
      .finally(() => {
        dockAttemptInFlight.delete(id);
        draw();
      });
  }
}

function fetchPreview(id: string) {
  void ipc
    .captureProcWindow(id)
    .then((url) => {
      previewCache[id] = url;
      draw();
    })
    .catch(() => {});
}

function fetchLogs(id: string): Promise<void> {
  return ipc
    .getProcLogs(id)
    .then((lines) => {
      logCache[id] = lines.map((l) => l.text).join("\n");
      draw();
    })
    .catch(() => {});
}

// ----- actions -----

function undock(id: string) {
  manualUndock.add(id);
  void act(ipc.undockProcWindow(id));
  dockStateCache[id] = { state: "not_docked" };
  draw();
}

function toggleSection(projectId: string) {
  toggleSetMember(sectionOpen, projectId);
  draw();
}

// ----- render -----

function frozenLogTail(id: string, label: string): TemplateResult {
  return html`
    <div class="dockpane-body">
      <p class="dockpane-note">Process ${label}. Last output:</p>
      <pre class="logs">${logCache[id] ? renderAnsi(logCache[id]) : "(no output)"}</pre>
    </div>
  `;
}

function liveLogTail(id: string): TemplateResult {
  return html`
    <div class="dockpane-body">
      <p class="dockpane-note">Waiting for the window to appear:</p>
      <pre class="logs">${logCache[id] ? renderAnsi(logCache[id]) : "(no output yet)"}</pre>
    </div>
  `;
}

function windowLostView(id: string): TemplateResult {
  return html`
    <div class="dockpane-body dockpane-lost">
      <i class="ph ph-arrow-square-out"></i>
      <p>The process is still running, but its window is gone - this happens when the supervisor was force-killed while docked.</p>
      <button class="abtn" @click=${() => void act(ipc.restartProc(id))}>
        <i class="ph ph-arrow-clockwise"></i> Restart
      </button>
    </div>
  `;
}

// Not a hole: the window lives in the backend's invisible host, so this
// pane draws real content, the latest screenshot an agent would also get.
function headlessView(project: Project, cmd: Command, id: string): TemplateResult {
  return html`
    <div class="dockpane-chrome">
      <span class="dockpane-pill dockpane-pill-headless">
        <i class="ph ph-eye-slash"></i>
        Headless
      </span>
      <button
        class="abtn"
        title="Show window normally"
        @click=${() => void act(ipc.setCommandHeadless(project.id, cmd.id, false))}
      >
        <i class="ph ph-app-window"></i>
      </button>
    </div>
    <div class="dockpane-body">
      ${previewCache[id]
        ? html`<img class="dockpane-preview" src=${previewCache[id]} alt="Latest capture of ${cmd.name}" />`
        : html`<p class="dockpane-note">Waiting for the first capture...</p>`}
    </div>
  `;
}

function dockedHole(id: string, mode: DockOutcome): TemplateResult {
  const soft = mode === "soft_docked";
  return html`
    <div class="dockpane-chrome">
      <span class="dockpane-pill ${soft ? "dockpane-pill-soft" : "dockpane-pill-embedded"}">
        <i class="ph ${soft ? "ph-arrows-out-cardinal" : "ph-frame-corners"}"></i>
        ${soft ? "Soft-docked" : "Embedded"}
      </span>
      <button class="abtn" title="Undock" @click=${() => undock(id)}>
        <i class="ph ph-arrow-square-out"></i>
      </button>
    </div>
    ${soft
      ? html`<p class="dockpane-note dockpane-soft-note">
          This app refused to be embedded, so it's being positioned over this spot instead - it can still float over other windows.
        </p>`
      : nothing}
    <div class="dockpane-hole"></div>
  `;
}

function dockPaneOne(project: Project, cmd: Command): TemplateResult {
  const id = `${project.id}:${cmd.id}`;
  const status = ui.statusById[id]?.status ?? "stopped";
  const running = status === "running" || status === "starting";

  let inner: TemplateResult;
  if (!running) {
    inner = frozenLogTail(id, status);
  } else {
    const state: DockState = dockStateCache[id] ?? { state: "not_docked" };
    if (state.state === "not_docked") inner = liveLogTail(id);
    else if (state.state === "window_lost") inner = windowLostView(id);
    else if (state.mode === "headless") inner = headlessView(project, cmd, id);
    else inner = dockedHole(id, state.mode);
  }

  return html`
    <div class="dockpane" data-cmd-id=${id} ${ref((el) => attachPaneObservers(id, el as HTMLElement | undefined))}>
      <div class="dockpane-title">${cmd.name}</div>
      ${inner}
    </div>
  `;
}

// The Project screen's collapsible "Docked windows" section. Renders nothing
// at all (no header, no empty state) when the project has zero commands with
// `dock_window: true` - this section must never appear for the headless dev
// servers that make up most of the command list.
export function dockSection(project: Project): TemplateResult | typeof nothing {
  const docked = project.commands.filter((c) => c.dock_window || c.dock_headless);
  for (const c of project.commands) {
    const id = `${project.id}:${c.id}`;
    if (c.dock_headless) headlessIds.add(id);
    else headlessIds.delete(id);
  }
  if (docked.length === 0) return nothing;

  ensurePolling(() => docked.map((c) => `${project.id}:${c.id}`));

  const open = sectionOpen.has(project.id);
  return html`
    <div class="dockpane-section">
      <button class="env-toggle" @click=${() => toggleSection(project.id)}>
        <i class="ph ${open ? "ph-caret-down" : "ph-caret-right"}"></i>
        <span>Docked windows</span>
      </button>
      ${open ? html`<div class="dockpane-list">${docked.map((c) => dockPaneOne(project, c))}</div>` : nothing}
    </div>
  `;
}
