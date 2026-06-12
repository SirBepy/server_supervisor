// Dashboard controller: mounts the view, drives the poll loop, and renders the
// project/command list. Shared view state + the render trigger live in ./state;
// the modals (add project/command, edit, delete) live in ./modals.

import { html, render, nothing, type TemplateResult } from "lit-html";
import "./dashboard.css";
import * as ipc from "../../shared/ipc";
import type { Project } from "../../types/ipc.generated";
import { ui, setDraw, refresh, act } from "./state";
import { formatBytes, displayName, formatUptime, projectTech, deviconClass } from "./helpers";
import { modalView, startAddCommand } from "./modals";
import { startAddProject } from "./add-project";
import { renderAnsi } from "../../shared/ansi";

const POLL_MS = 2500;

export function mountDashboard(el: HTMLElement): () => void {
  ui.root = el;
  setDraw(draw);
  void refresh();
  void loadPrefs();
  // Capture the poll handle and clear it on teardown. Without this the interval
  // outlives navigation and keeps calling draw() into the (now replaced) root,
  // throwing lit-html "ChildPart has no parentNode" every tick and corrupting
  // whatever view replaced it.
  const timer = window.setInterval(() => void refresh(), POLL_MS);

  // Close the per-project "more options" menu on any outside click or Escape.
  // The button + menu both stopPropagation, so any click reaching the document
  // is outside the open menu.
  const onDocClick = () => {
    if (ui.openMenuFor !== null || ui.openCmdMenuFor !== null) {
      ui.openMenuFor = null;
      ui.openCmdMenuFor = null;
      draw();
    }
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape" && (ui.openMenuFor !== null || ui.openCmdMenuFor !== null)) {
      ui.openMenuFor = null;
      ui.openCmdMenuFor = null;
      draw();
    }
  };
  document.addEventListener("click", onDocClick);
  document.addEventListener("keydown", onKey);

  return () => {
    window.clearInterval(timer);
    document.removeEventListener("click", onDocClick);
    document.removeEventListener("keydown", onKey);
  };
}

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

async function toggleLogs(id: string) {
  if (ui.openLogsFor === id) {
    ui.openLogsFor = null;
    ui.logText = "";
  } else {
    ui.openLogsFor = id;
    ui.logText = (await ipc.getProcLogs(id)).map((l) => l.text).join("\n");
    // Newly opened: start at the newest line.
    ui.scrollLogsToBottom = true;
  }
  draw();
}

// ----- rendering -----

function runningCount(project: Project): number {
  return project.commands.filter(
    (c) => ui.statusById[`${project.id}:${c.id}`]?.status === "running",
  ).length;
}

function toggleCollapse(projectId: string) {
  if (ui.collapsed.has(projectId)) {
    ui.collapsed.delete(projectId);
  } else {
    ui.collapsed.add(projectId);
  }
  draw();
}

// Map a process status to the card's status class (drives the colored left
// edge). Unknown/absent reads as stopped.
function statusClass(status: string | undefined): string {
  switch (status) {
    case "running":
    case "starting":
    case "crashed":
      return status;
    default:
      return "stopped";
  }
}

// Open a running command's port in a browser (from the kebab menu). Flutter-web
// ports go to the dedicated CORS-disabled dev browser (new tab in the same
// window); everything else opens in the default browser. Always http: these are
// localhost dev servers (and the flutter reload proxy), never https.
function openInBrowser(port: number, flutter: boolean) {
  void ipc.openPortUrl(`http://localhost:${port}`, flutter);
}

// Copy a command's localhost URL to the clipboard (from the kebab menu).
function copyPortUrl(port: number) {
  void navigator.clipboard?.writeText(`http://localhost:${port}`);
}

// The expanded-card header: pid + RAM + port + uptime, in one muted line. These
// move here from the resting row so the row stays clean; when the card is open
// the right-side stats are hidden, so this is where they remain visible.
function drawerHeader(
  pid: number | null | undefined,
  mem: bigint | number | null | undefined,
  port: number | null | undefined,
  startedAt: bigint | number | null | undefined,
): string {
  const parts: string[] = [];
  if (pid != null) parts.push(`pid ${pid}`);
  if (mem != null) parts.push(formatBytes(mem));
  if (port != null) parts.push(`port ${port}`);
  const up = formatUptime(startedAt);
  if (up) parts.push(`started ${up}`);
  return parts.length ? parts.join(" · ") : "no run info";
}

// Per-command "more options" (kebab) menu. For a live process (running or
// starting): Open-in-browser + Copy URL for its port (the port is no longer a
// bare clickable badge - opening it lives here), then Restart / Stop. For a
// stopped or crashed process: Edit / Remove (you can't edit a live command).
function cmdMenu(
  project: Project,
  cmd: Project["commands"][number],
  id: string,
  status: string,
): TemplateResult {
  const open = ui.openCmdMenuFor === id;
  const live = status === "running" || status === "starting";
  const port = ui.statusById[id]?.port;
  const isFlutter = cmd.kind === "flutter";
  const close = () => {
    ui.openCmdMenuFor = null;
  };
  return html`
    <div class="cmd-more">
      <button
        class=${open ? "active" : ""}
        title="More options"
        @click=${(e: Event) => {
          e.stopPropagation();
          ui.openCmdMenuFor = open ? null : id;
          draw();
        }}
      >
        <i class="ph ph-dots-three-vertical"></i>
      </button>
      ${open
        ? html`
            <div class="more-menu" @click=${(e: Event) => e.stopPropagation()}>
              ${live
                ? html`
                    ${port != null
                      ? html`
                          <button class="accent" @click=${() => { close(); openInBrowser(port, isFlutter); }}>
                            <i class="ph ph-globe-simple"></i> Open :${port} in browser
                          </button>
                          <button @click=${() => { close(); copyPortUrl(port); }}>
                            <i class="ph ph-copy"></i> Copy URL
                          </button>
                          <div class="menu-div"></div>
                        `
                      : nothing}
                    <button @click=${() => { close(); void act(ipc.restartProc(id)); }}>
                      <i class="ph ph-arrow-clockwise"></i> Restart
                    </button>
                    <button @click=${() => { close(); void act(ipc.stopProc(id)); }}>
                      <i class="ph ph-stop"></i> Stop
                    </button>
                  `
                : html`
                    <button
                      @click=${() => {
                        close();
                        ui.modal = {
                          t: "editCommand",
                          projectId: project.id,
                          commandId: cmd.id,
                          root: project.root,
                          name: cmd.name,
                          cmd: cmd.cmd,
                          autostart: cmd.autostart,
                          useDynamicPort: cmd.use_dynamic_port,
                          env: cmd.env,
                          check: null,
                        };
                        ui.comboOpen = false;
                        draw();
                      }}
                    >
                      <i class="ph ph-pencil-simple"></i> Edit command
                    </button>
                    <button
                      @click=${() => {
                        close();
                        ui.modal = {
                          t: "confirmDeleteCommand",
                          projectId: project.id,
                          commandId: cmd.id,
                          cmdName: cmd.name,
                          lastOne: project.commands.length === 1,
                        };
                        draw();
                      }}
                    >
                      <i class="ph ph-trash"></i> Remove command
                    </button>
                  `}
            </div>
          `
        : nothing}
    </div>
  `;
}

function commandRow(project: Project, cmd: Project["commands"][number]): TemplateResult {
  const id = `${project.id}:${cmd.id}`;
  const info = ui.statusById[id];
  const status = info?.status ?? "stopped";
  const running = status === "running";
  const pid = info?.pid;
  const port = info?.port;
  const mem = info?.mem_bytes;
  const startedAt = info?.started_at;
  const isFlutter = cmd.kind === "flutter";
  const logsOpen = ui.openLogsFor === id;
  // Only live/crashed processes have logs worth expanding; stopped ones are inert.
  const expandable = status !== "stopped";
  const menuOpen = ui.openCmdMenuFor === id;

  return html`
    <div
      class="card ${statusClass(status)} ${expandable ? "expandable" : ""} ${logsOpen ? "logs-open" : ""} ${menuOpen ? "cmd-menu-open" : ""}"
    >
      <div
        class="row"
        role=${expandable ? "button" : nothing}
        title=${expandable ? `Click to ${logsOpen ? "hide" : "show"} logs` : nothing}
        @click=${expandable ? () => toggleLogs(id) : nothing}
      >
        <span class="name" title=${cmd.name}>${displayName(cmd)}</span>
        ${isFlutter ? html`<span class="ftag">flutter</span>` : nothing}
        ${status === "crashed" ? html`<span class="statusword">crashed</span>` : nothing}
        ${status === "starting" ? html`<span class="statusword">starting</span>` : nothing}
        <div class="right">
          <div class="stats">
            ${ui.showRam && mem != null
              ? html`<span class="cell"><span class="k">RAM</span><span class="v">${formatBytes(mem)}</span></span>`
              : nothing}
            ${ui.showPort && port != null
              ? html`<span class="cell"><span class="k">Port</span><span class="v">${port}</span></span>`
              : nothing}
          </div>
          <div class="controls" @click=${(e: Event) => e.stopPropagation()}>
            ${running && isFlutter
              ? html`<button class="abtn" title="Hot restart" @click=${() => act(ipc.reloadProc(id))}>
                  <i class="ph ph-arrows-clockwise"></i>
                </button>`
              : nothing}
            ${status === "stopped" || status === "crashed"
              ? html`<button class="abtn start" title="Start" @click=${() => act(ipc.startProc(id))}>
                  <i class="ph ph-play"></i>
                </button>`
              : nothing}
            ${cmdMenu(project, cmd, id, status)}
            ${expandable
              ? html`<i
                  class="ph ${logsOpen ? "ph-caret-up" : "ph-caret-down"} chev"
                  title="${logsOpen ? "Hide" : "Show"} logs"
                  @click=${() => toggleLogs(id)}
                ></i>`
              : nothing}
          </div>
        </div>
      </div>
      ${logsOpen
        ? html`
            <div class="drawer">
              <div class="pidline">${drawerHeader(pid, mem, port, startedAt)}</div>
              <pre class="logs">${ui.logText ? renderAnsi(ui.logText) : "(no output yet)"}</pre>
            </div>
          `
        : nothing}
    </div>
  `;
}

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

// The project's icon slot: real project icon (tier 1) -> tech logo (tier 2) ->
// generic Phosphor terminal glyph (tier 3).
function projectIconTemplate(project: Project): TemplateResult {
  ensureProjectIcon(project);
  const cached = ui.iconCache[project.id];
  if (typeof cached === "string") {
    // onerror falls back to the tech logo if the bytes fail to decode.
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

// Per-project "more options" (kebab) button + its popover menu. The kebab trigger
// lives in the row's hover swap zone (.prow-actions); this provides the button +
// anchored menu (Add command / Rename project / Open in file explorer).
function moreMenu(project: Project): TemplateResult {
  const open = ui.openMenuFor === project.id;
  return html`
    <div class="proj-more ${open ? "menu-open" : ""}">
      <button
        class="abtn ${open ? "active" : ""}"
        title="More options"
        @click=${(e: Event) => {
          e.stopPropagation();
          ui.openMenuFor = open ? null : project.id;
          draw();
        }}
      >
        <i class="ph ph-dots-three-vertical"></i>
      </button>
      ${open
        ? html`
            <div class="more-menu" @click=${(e: Event) => e.stopPropagation()}>
              <button
                @click=${() => {
                  ui.openMenuFor = null;
                  void startAddCommand(project.id, project.root);
                }}
              >
                <i class="ph ph-plus"></i> Add command
              </button>
              <button
                @click=${() => {
                  ui.openMenuFor = null;
                  ui.modal = { t: "renameProject", projectId: project.id, name: project.name };
                  draw();
                }}
              >
                <i class="ph ph-pencil-simple"></i> Rename project
              </button>
              <button
                @click=${() => {
                  ui.openMenuFor = null;
                  void ipc.openInExplorer(project.root);
                  draw();
                }}
              >
                <i class="ph ph-folder-open"></i> Open in file explorer
              </button>
            </div>
          `
        : nothing}
    </div>
  `;
}

function projectSection(project: Project): TemplateResult | typeof nothing {
  const count = runningCount(project);
  if (ui.filterRunning && count === 0) return nothing;

  const collapsed = ui.collapsed.has(project.id);
  const visibleCmds = ui.filterRunning
    ? project.commands.filter(
        (c) => ui.statusById[`${project.id}:${c.id}`]?.status === "running",
      )
    : project.commands;

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
}

function draw() {
  const emptyMsg = ui.projects.length === 0
    ? html`<p class="empty">No projects yet. Use the + button to add a project.</p>`
    : ui.filterRunning && ui.projects.every((p) => runningCount(p) === 0)
      ? html`<p class="empty">No running processes.</p>`
      : nothing;

  render(
    html`
      <div class="header-block">
        <header class="topbar">
          <button class="icon-btn" title="Add project" @click=${() => void startAddProject()}>
            <i class="ph ph-folder-plus"></i>
          </button>
          <h1>Server Supervisor</h1>
          <button class="icon-btn" title="Settings" @click=${() => { location.hash = "#settings"; }}>
            <i class="ph ph-gear"></i>
          </button>
        </header>
        <div class="filterbar">
          <button
            class="filter-chip ${ui.filterRunning ? "" : "active"}"
            @click=${() => { ui.filterRunning = false; draw(); }}
          >All</button>
          <button
            class="filter-chip ${ui.filterRunning ? "active" : ""}"
            @click=${() => { ui.filterRunning = true; draw(); }}
          >Running</button>
        </div>
      </div>
      ${ui.error ? html`<div class="error">${ui.error}</div>` : nothing}
      ${emptyMsg}
      ${ui.projects.map(projectSection)}
      ${modalView()}
    `,
    ui.root,
  );

  // Post-render: if this draw was flagged to pin the log pane (open, or new
  // lines while already at the bottom), scroll it to the newest line. lit-html
  // reuses the <pre> node across renders, so a scrolled-up reader's position is
  // preserved on the draws that do NOT set this flag.
  if (ui.scrollLogsToBottom) {
    const logsEl = ui.root.querySelector<HTMLElement>(".logs");
    if (logsEl) logsEl.scrollTop = logsEl.scrollHeight;
    ui.scrollLogsToBottom = false;
  }
}
