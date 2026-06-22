// Dashboard controller: mounts the view, drives the poll loop, and renders the
// project/command list. Shared view state + the render trigger live in ./state;
// the modals (add project/command, edit, delete) live in ./modals.

import { html, render, nothing, type TemplateResult } from "lit-html";
import "./dashboard.css";
import * as ipc from "../../shared/ipc";
import type { Group, Project } from "../../types/ipc.generated";
import { ui, setDraw, refresh, act } from "./state";
import { formatBytes, displayName, formatUptime, projectTech, deviconClass, deviconClassByName } from "./helpers";
import { modalView } from "./modals";
import { cmdMenu, groupMenu, moreMenu, portalMenu, setMouseAnchor } from "./menus";
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
    if (
      ui.openMenuFor !== null ||
      ui.openCmdMenuFor !== null ||
      ui.openGroupMenuFor !== null ||
      ui.openMoveToGroupFor !== null ||
      ui.openEmptyMenu
    ) {
      ui.openMenuFor = null;
      ui.openCmdMenuFor = null;
      ui.openGroupMenuFor = null;
      ui.openMoveToGroupFor = null;
      ui.openEmptyMenu = false;
      ui.menuAnchor = null;
      draw();
    }
  };
  const onKey = (e: KeyboardEvent) => {
    if (
      e.key === "Escape" &&
      (ui.openMenuFor !== null ||
        ui.openCmdMenuFor !== null ||
        ui.openGroupMenuFor !== null ||
        ui.openMoveToGroupFor !== null ||
        ui.openEmptyMenu)
    ) {
      ui.openMenuFor = null;
      ui.openCmdMenuFor = null;
      ui.openGroupMenuFor = null;
      ui.openMoveToGroupFor = null;
      ui.openEmptyMenu = false;
      ui.menuAnchor = null;
      draw();
    }
  };
  const onContextMenu = (e: MouseEvent) => {
    if ((e.target as HTMLElement).closest(".prow, .card, .grow, .more-menu, .proj-more, .cmd-more")) return;
    e.preventDefault();
    ui.openMenuFor = null;
    ui.openCmdMenuFor = null;
    ui.openGroupMenuFor = null;
    ui.openMoveToGroupFor = null;
    ui.openEmptyMenu = true;
    setMouseAnchor(e, 80);
    draw();
  };
  document.addEventListener("click", onDocClick);
  document.addEventListener("keydown", onKey);
  el.addEventListener("contextmenu", onContextMenu);

  return () => {
    window.clearInterval(timer);
    document.removeEventListener("click", onDocClick);
    document.removeEventListener("keydown", onKey);
    el.removeEventListener("contextmenu", onContextMenu);
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

// Jump-bar click: reveal a running command in the list. Expand its project and
// open its log drawer, then scroll its card into view. The expand + scroll draw
// happens IMMEDIATELY (synchronously); the log text is fetched in the background
// and drawn in when it lands, so a slow getProcLogs never delays the visible
// response. A second click on the already-open command just re-focuses (scrolls)
// without refetching.
function focusCommand(projectId: string, id: string) {
  ui.collapsed.delete(projectId);
  const needLogs = ui.openLogsFor !== id;
  if (needLogs) {
    ui.openLogsFor = id;
    ui.logText = ""; // clear the previous command's text; "(no output yet)" until the fetch lands
    ui.scrollLogsToBottom = true;
  }
  draw();
  // Next frame: the (now expanded) card exists in the DOM, so scroll to it.
  requestAnimationFrame(() => {
    const el = ui.root.querySelector(`[data-cmd-id="${CSS.escape(id)}"]`);
    el?.scrollIntoView({ block: "nearest" });
  });
  if (needLogs) {
    void ipc.getProcLogs(id).then((lines) => {
      // Only apply if this command is still the open one (the user may have
      // clicked elsewhere while the fetch was in flight).
      if (ui.openLogsFor !== id) return;
      ui.logText = lines.map((l) => l.text).join("\n");
      ui.scrollLogsToBottom = true;
      draw();
    });
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

function toggleGroupCollapse(id: string) {
  if (ui.collapsedGroups.has(id)) {
    ui.collapsedGroups.delete(id);
  } else {
    ui.collapsedGroups.add(id);
  }
  draw();
}

function groupRunningCount(group: Group): number {
  return group.project_ids
    .flatMap((pid) => {
      const p = ui.projects.find((p) => p.id === pid);
      return p ? p.commands.map((c) => ui.statusById[`${p.id}:${c.id}`]?.status) : [];
    })
    .filter((s) => s === "running").length;
}

function groupSection(group: Group): TemplateResult {
  const projects = group.project_ids
    .map((id) => ui.projects.find((p) => p.id === id))
    .filter((p): p is Project => p != null);
  const running = groupRunningCount(group);
  const collapsed = ui.collapsedGroups.has(group.id);
  return html`
    <section class="ggroup">
      <div
        class="grow"
        @click=${() => toggleGroupCollapse(group.id)}
        @contextmenu=${(e: Event) => {
          e.preventDefault();
          e.stopPropagation();
          ui.openMenuFor = null;
          ui.openCmdMenuFor = null;
          ui.openMoveToGroupFor = null;
          ui.openEmptyMenu = false;
          ui.openGroupMenuFor = group.id;
          setMouseAnchor(e as MouseEvent, 120);
          draw();
        }}
      >
        <i class="ph ${collapsed ? "ph-caret-right" : "ph-caret-down"} gchev"></i>
        <span class="gname">${group.name}</span>
        ${running > 0 ? html`<span class="gbadge">${running} running</span>` : nothing}
        <div @click=${(e: Event) => e.stopPropagation()}>${groupMenu(group)}</div>
      </div>
      ${collapsed ? nothing : projects.map((p) => projectSection(p))}
    </section>
  `;
}

function otherSection(projects: Project[]): TemplateResult | typeof nothing {
  if (projects.length === 0) return nothing;
  const collapsed = ui.collapsedGroups.has("__other__");
  const running = projects.reduce((n, p) => n + runningCount(p), 0);
  return html`
    <section class="ggroup">
      <div class="grow other" @click=${() => toggleGroupCollapse("__other__")}>
        <i class="ph ${collapsed ? "ph-caret-right" : "ph-caret-down"} gchev"></i>
        <span class="gname">other</span>
        ${running > 0 ? html`<span class="gbadge">${running} running</span>` : nothing}
      </div>
      ${collapsed ? nothing : projects.map((p) => projectSection(p))}
    </section>
  `;
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
      data-cmd-id=${id}
      class="card ${statusClass(status)} ${expandable ? "expandable" : ""} ${logsOpen ? "logs-open" : ""} ${menuOpen ? "cmd-menu-open" : ""}"
    >
      <div
        class="row"
        role=${expandable ? "button" : nothing}
        title=${expandable ? `Click to ${logsOpen ? "hide" : "show"} logs` : nothing}
        @click=${expandable ? () => toggleLogs(id) : nothing}
        @contextmenu=${(e: Event) => {
          e.preventDefault();
          e.stopPropagation();
          ui.openMenuFor = null;
          ui.openCmdMenuFor = id;
          setMouseAnchor(e as MouseEvent, 200);
          draw();
        }}
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
              ? html`<button class="abtn start" title="Start" @click=${() => { if (ui.openLogsFor === id) { ui.logText = ""; } act(ipc.startProc(id)); }}>
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

// One-time backend marker-file tech fetch for a project, cached. Only called when
// the command name didn't reveal the tech. No-op if already fetched/pending.
function ensureProjectTech(project: Project) {
  if (project.id in ui.techCache) return;
  ui.techCache[project.id] = undefined; // mark pending (key now present)
  void ipc
    .getProjectTech(project.root)
    .then((tech) => {
      ui.techCache[project.id] = tech;
      draw();
    })
    .catch(() => {
      ui.techCache[project.id] = null;
      draw();
    });
}

// The project's icon, resolved in tiers (the bare <img>/<i>, no wrapper):
//   1. real project icon (backend folder scan)
//   2a. tech logo from the command program (e.g. `cargo`, `flutter`)
//   2b. tech logo from project marker files (e.g. pyproject.toml -> python), for
//       custom launcher commands that hide the tech
//   3. generic Phosphor terminal glyph
// Factored out so both the project row (.picon) and a jump-bar icon (.ji) can
// reuse the same tier logic with their own wrappers.
function resolveProjectIcon(project: Project): TemplateResult {
  ensureProjectIcon(project);
  const cached = ui.iconCache[project.id];
  if (typeof cached === "string") {
    // onerror falls back to the tech logo if the bytes fail to decode.
    return html`<img
      src=${cached}
      alt=""
      @error=${() => {
        ui.iconCache[project.id] = null;
        draw();
      }}
    />`;
  }
  const cmdTech = projectTech(project, ui.statusById);
  if (cmdTech) {
    return html`<i class="${deviconClass(cmdTech)}"></i>`;
  }
  // Command didn't reveal the tech: fall back to the backend marker-file scan.
  ensureProjectTech(project);
  const fileTech = ui.techCache[project.id];
  if (typeof fileTech === "string") {
    const cls = deviconClassByName(fileTech);
    if (cls) return html`<i class="${cls}"></i>`;
  }
  return html`<i class="ph ph-terminal-window"></i>`;
}

// The project's icon slot for the project row.
function projectIconTemplate(project: Project): TemplateResult {
  return html`<span class="picon">${resolveProjectIcon(project)}</span>`;
}

function projectSection(project: Project): TemplateResult | typeof nothing {
  const count = runningCount(project);
  const collapsed = ui.collapsed.has(project.id);
  const singleCmd = project.commands.length === 1 ? project.commands[0] : null;
  const singleId = singleCmd ? `${project.id}:${singleCmd.id}` : null;
  const singleStatus = singleId ? (ui.statusById[singleId]?.status ?? "stopped") : null;
  return html`
    <section class="group">
      <div
        class="prow"
        @click=${() => toggleCollapse(project.id)}
        @contextmenu=${(e: Event) => {
          e.preventDefault();
          e.stopPropagation();
          ui.openCmdMenuFor = null;
          ui.openMenuFor = project.id;
          setMouseAnchor(e as MouseEvent, 140);
          draw();
        }}
      >
        <span class="pdot ${count > 0 ? "on" : ""}"></span>
        ${projectIconTemplate(project)}
        <span class="pname" title=${project.name}>${project.name}</span>
        <div class="prow-right" @click=${(e: Event) => e.stopPropagation()}>
          ${ui.showCommandCount
            ? html`<span class="pcount"><i class="ph ph-terminal-window"></i>${project.commands.length}</span>`
            : nothing}
          <div class="prow-actions">
            ${singleCmd && singleId && (singleStatus === "stopped" || singleStatus === "crashed")
              ? html`<button class="abtn start" title="Start ${singleCmd.name}" @click=${() => act(ipc.startProc(singleId))}>
                  <i class="ph ph-play"></i>
                </button>`
              : nothing}
            ${singleCmd && singleId && (singleStatus === "running" || singleStatus === "starting")
              ? html`<button class="abtn" title="Stop ${singleCmd.name}" @click=${() => act(ipc.stopProc(singleId))}>
                  <i class="ph ph-stop"></i>
                </button>`
              : nothing}
            ${moreMenu(project)}
          </div>
        </div>
      </div>
      ${collapsed
        ? nothing
        : project.commands.length === 0
          ? html`<p class="empty-cmd">No commands. Add one.</p>`
          : project.commands.map((c) => commandRow(project, c))}
    </section>
  `;
}

// The running jump bar: one icon per live command, pinned under the topbar. A
// pure projection of process state (no own state), hidden entirely when nothing
// is running. Hover shows "project · command"; click reveals it in the list.
function jumpBar(): TemplateResult | typeof nothing {
  const items: { project: Project; cmd: Project["commands"][number]; id: string }[] = [];
  for (const project of ui.projects) {
    for (const cmd of project.commands) {
      const id = `${project.id}:${cmd.id}`;
      const status = ui.statusById[id]?.status;
      // Only live commands have a terminal worth jumping to.
      if (status === "running" || status === "starting") {
        items.push({ project, cmd, id });
      }
    }
  }
  if (items.length === 0) return nothing;
  return html`
    <div class="jump">
      ${items.map(
        ({ project, cmd, id }) => html`
          <button
            class="ji ${ui.openLogsFor === id ? "active" : ""}"
            title=${`${project.name} · ${displayName(cmd)}`}
            @click=${() => void focusCommand(project.id, id)}
            @contextmenu=${(e: Event) => {
              e.preventDefault();
              e.stopPropagation();
              ui.openMenuFor = null;
              ui.openCmdMenuFor = id;
              const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
              ui.menuAnchor = {
                top: rect.top,
                bottom: rect.bottom,
                left: rect.left,
                right: rect.right,
                flipUp: rect.bottom + 200 > window.innerHeight,
              };
              draw();
            }}
          >
            ${resolveProjectIcon(project)}
          </button>
        `,
      )}
    </div>
  `;
}

function draw() {
  const groupedIds = new Set(ui.groups.flatMap((g) => g.project_ids));
  const ungrouped = ui.projects.filter((p) => !groupedIds.has(p.id));
  const isEmpty = ui.projects.length === 0 && ui.groups.length === 0;

  render(
    html`
      <div class="header-block">
        <header class="topbar">
          <h1>Server Supervisor</h1>
          <button class="icon-btn" title="Settings" @click=${() => { location.hash = "#settings"; }}>
            <i class="ph ph-gear"></i>
          </button>
        </header>
        ${jumpBar()}
      </div>
      ${ui.error ? html`<div class="error">${ui.error}</div>` : nothing}
      ${isEmpty
        ? html`<p class="empty">Right-click to add a project or group.</p>`
        : nothing}
      <div class="project-list">
        ${ui.groups.map(groupSection)}
        ${otherSection(ungrouped)}
      </div>
      ${modalView()}
      ${portalMenu()}
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
