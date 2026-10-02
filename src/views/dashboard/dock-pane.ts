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
// This file only renders. The per-command state it reads (dock state, log
// tails, screen geometry, in-flight dock attempts) lives in module-level
// caches in dock-pane-poll.ts - this file reaches them only through its
// exported functions (getDockState, getLogCache, getPreviewCache, undock,
// attachPaneObservers, ensurePolling, setHeadless, isDockRefused), never the
// caches themselves. Project-screen.ts only imports `dockSection` and mounts
// it; menus/cmd-menu.ts imports `isDockRefused`, re-exported below so neither
// importer's path changes.

import { html, nothing, type TemplateResult } from "lit-html";
import { ref } from "lit-html/directives/ref.js";
import "./dock-pane.css";
import * as ipc from "../../shared/ipc";
import type { Command, DockOutcome, Project } from "../../types/ipc.generated";
import { ui, act, draw } from "./state";
import { toggleSetMember } from "./helpers";
import { renderAnsi } from "../../shared/ansi";
import { refusedView } from "./dock-pane-refused";
import { attachPaneObservers, ensurePolling, getDockState, getLogCache, getPreviewCache, setHeadless, undock } from "./dock-pane-poll";

export { isDockRefused } from "./dock-pane-poll";

// Project screen's "Docked windows" section is collapsed by default (matches
// envSectionOpen/hubLogOpen elsewhere in this project); absent = collapsed.
const sectionOpen = new Set<string>();

function toggleSection(projectId: string) {
  toggleSetMember(sectionOpen, projectId);
  draw();
}

// ----- render -----

function frozenLogTail(id: string, label: string): TemplateResult {
  const logs = getLogCache(id);
  return html`
    <div class="dockpane-body">
      <p class="dockpane-note">Process ${label}. Last output:</p>
      <pre class="logs">${logs ? renderAnsi(logs) : "(no output)"}</pre>
    </div>
  `;
}

function liveLogTail(id: string): TemplateResult {
  const logs = getLogCache(id);
  return html`
    <div class="dockpane-body">
      <p class="dockpane-note">Waiting for the window to appear:</p>
      <pre class="logs">${logs ? renderAnsi(logs) : "(no output yet)"}</pre>
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
  const preview = getPreviewCache(id);
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
      ${preview
        ? html`<img class="dockpane-preview" src=${preview} alt="Latest capture of ${cmd.name}" />`
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
    const state = getDockState(id);
    if (state.state === "not_docked") inner = liveLogTail(id);
    else if (state.state === "window_lost") inner = windowLostView(id);
    else if (state.state === "refused") inner = refusedView(project.id, cmd.id);
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
    setHeadless(id, c.dock_headless);
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
