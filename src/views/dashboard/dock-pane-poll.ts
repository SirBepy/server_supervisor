// Per-command dock state for dock-pane.ts: the caches, rect reporting and
// polling loop that decide what each pane's render should show. Split out of
// dock-pane.ts (todo 0051) because that file grew past the project's ~300
// line bar; the render/view side stayed behind.
//
// This module is the ONLY owner of the caches below. dock-pane.ts never reads
// or writes them directly - it goes through the exported functions here
// (getDockState, getLogCache, getPreviewCache, undock, attachPaneObservers,
// ensurePolling, setHeadless, isDockRefused), so there is exactly one copy.

import * as ipc from "../../shared/ipc";
import type { DockRect, DockState } from "../../types/ipc.generated";
import { ui, act, draw } from "./state";
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

// Read by cmd-menu.ts (via dock-pane.ts's re-export), which has no dock-state
// access of its own, so its headless toggle label can say "(refused)"
// instead of claiming it worked.
export function isDockRefused(id: string): boolean {
  return dockStateCache[id]?.state === "refused";
}

// Read by dock-pane.ts's render: the fallback to "not_docked" matches the
// prior inline `dockStateCache[id] ?? { state: "not_docked" }`.
export function getDockState(id: string): DockState {
  return dockStateCache[id] ?? { state: "not_docked" };
}

export function getLogCache(id: string): string | undefined {
  return logCache[id];
}

export function getPreviewCache(id: string): string | undefined {
  return previewCache[id];
}

// Called from dock-pane.ts's dockSection loop, once per command per render.
export function setHeadless(id: string, isHeadless: boolean) {
  if (isHeadless) headlessIds.add(id);
  else headlessIds.delete(id);
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

// Attached via the `ref` directive in dock-pane.ts - lit-html invokes a ref
// callback with the element on mount and with `undefined` on disconnect
// (including a disconnect immediately followed by mounting a fresh element
// instance, e.g. a project-screen remount), so both branches below matter:
// skipping the teardown branch would leave a ResizeObserver/scroll listener
// bound to a detached element, which reports a zero rect and corrupts the
// live dock position for `id`.
export function attachPaneObservers(id: string, el: HTMLElement | undefined) {
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
// it - dockSection runs on every render (once per project the dashboard
// shows), and only the first call's closure would otherwise ever run,
// freezing the poll on whichever project happened to render first.
let currentGetTrackedIds: () => string[] = () => [];

export function ensurePolling(getTrackedIds: () => string[]) {
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

export function undock(id: string) {
  manualUndock.add(id);
  void act(ipc.undockProcWindow(id));
  dockStateCache[id] = { state: "not_docked" };
  draw();
}
